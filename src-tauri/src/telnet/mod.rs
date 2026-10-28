use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch, Mutex, Semaphore};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::ssh::{ResizeRequest, SESSION_CLOSED_ERROR};

// ── Telnet protocol constants (RFC 854 / RFC 1073) ─────────────────

const IAC: u8 = 255; // Interpret As Command
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250; // Sub-negotiation Begin
const SE: u8 = 240; // Sub-negotiation End

// Telnet options we care about.
const OPT_ECHO: u8 = 1;
const OPT_SUPPRESS_GO_AHEAD: u8 = 3;
const OPT_NAWS: u8 = 31; // Negotiate About Window Size

/// Parameters for establishing a Telnet connection.
pub struct TelnetConnectParams {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub cols: u16,
    pub rows: u16,
    pub app_handle: AppHandle,
    pub restrict_private_ips: bool,
    pub connect_timeout_secs: u64,
    pub logger: Option<std::sync::Arc<std::sync::Mutex<crate::session_logger::SessionLogManager>>>,
    /// Receiver for the "terminal ready" signal. The reader task waits on this
    /// before entering its read loop.
    pub ready_rx: tokio::sync::oneshot::Receiver<()>,
    pub login_sequence: Option<crate::login_sequence::LoginSequenceConfig>,
}

/// Per-connection Telnet session state.
pub(crate) struct TelnetSession {
    reader_task: JoinHandle<()>,
    write_tx: mpsc::Sender<Vec<u8>>,
    /// Watch channel carrying the latest pending resize. `None` is the
    /// initial sentinel — the initial NAWS is sent during connection setup
    /// in `telnet_io_loop`, so the reader does not need to fire one at
    /// startup. Latest-wins semantics make intermediate resize values
    /// during a burst safely discardable.
    resize_tx: watch::Sender<Option<ResizeRequest>>,
    /// Bounds the number of in-flight write tasks per session. Prevents a
    /// runaway frontend from spawning unbounded tasks (each holding up to
    /// `MAX_WRITE_SIZE` bytes) when the writer task is slow to drain the
    /// write channel (e.g. a stalled remote network).
    write_semaphore: Arc<Semaphore>,
}

impl TelnetSession {
    /// Close the session and wait until the socket is closed. The I/O task
    /// owns the socket, so it closes when the stopped task is dropped.
    pub(crate) async fn close(mut self) {
        self.reader_task.abort();
        // Cancelled is the expected outcome; anything else is a panic.
        if let Err(e) = (&mut self.reader_task).await {
            if !e.is_cancelled() {
                warn!(error = %e, "Telnet I/O task panicked");
            }
        }
    }
}

impl Drop for TelnetSession {
    /// A session dropped without [`TelnetSession::close`] — refused at
    /// registration, or cleared by a page load — must not stay connected
    /// where nobody can see it. Dropping a `JoinHandle` leaves its task
    /// running, so stop it explicitly.
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

/// Manages all active Telnet sessions.
#[derive(Default)]
pub struct TelnetManager {
    sessions: HashMap<String, TelnetSession>,
}

impl TelnetManager {
    const MAX_SESSIONS: usize = 100;

    /// Pre-flight check: verify we haven't hit the session limit.
    pub fn check_capacity(&self) -> Result<(), String> {
        if self.sessions.len() >= Self::MAX_SESSIONS {
            return Err(format!(
                "Session limit reached (max {})",
                Self::MAX_SESSIONS
            ));
        }
        Ok(())
    }

    /// Register an established Telnet session. Re-checks capacity to prevent
    /// TOCTOU races when multiple connections are established concurrently.
    /// A refused session is dropped, which closes it.
    pub fn register_session(&mut self, id: String, session: TelnetSession) -> Result<(), String> {
        self.check_capacity()?;
        self.sessions.insert(id, session);
        Ok(())
    }

    /// Get the write sender for a session.
    ///
    /// Fails with [`SESSION_CLOSED_ERROR`] once the session's I/O task has
    /// ended — connection closed, a read or write error, or no way left to
    /// emit output. That task owns the write channel's receiver, so the
    /// channel closes exactly then. A session stays registered until the
    /// frontend calls `telnet_disconnect`, and the send happens on a detached
    /// task, so without this check writes to a dead session would report
    /// success and the frontend would never learn it is gone.
    pub fn get_write_sender(&self, id: &str) -> Result<mpsc::Sender<Vec<u8>>, String> {
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| format!("Telnet session {id} not found"))?;
        if session.write_tx.is_closed() {
            return Err(SESSION_CLOSED_ERROR.to_string());
        }
        Ok(session.write_tx.clone())
    }

    /// Get the resize sender for a session.
    pub fn get_resize_sender(
        &self,
        id: &str,
    ) -> Result<watch::Sender<Option<ResizeRequest>>, String> {
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| format!("Telnet session {id} not found"))?;
        Ok(session.resize_tx.clone())
    }

    /// Get the inflight-writes semaphore for a session.
    pub fn get_write_semaphore(&self, id: &str) -> Result<Arc<Semaphore>, String> {
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| format!("Telnet session {id} not found"))?;
        Ok(Arc::clone(&session.write_semaphore))
    }

    /// Unregister a session so the caller can [`TelnetSession::close`] it
    /// after releasing the manager lock.
    pub fn remove(&mut self, id: &str) -> Result<TelnetSession, String> {
        self.sessions
            .remove(id)
            .ok_or_else(|| format!("Telnet session {id} not found"))
    }

    /// Unregister every session, returning their `ID`s and sessions.
    pub fn take_all(&mut self) -> Vec<(String, TelnetSession)> {
        self.sessions.drain().collect()
    }
}

/// Thread-safe wrapper around `TelnetManager` for use as Tauri managed state.
pub struct TelnetState(pub Arc<Mutex<TelnetManager>>);

impl Default for TelnetState {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(TelnetManager::default())))
    }
}

/// Establish a Telnet connection and start streaming I/O.
///
/// This is intentionally a free function (not a method on `TelnetManager`) so
/// that callers can perform the expensive TCP handshake **without** holding the
/// manager lock — preventing one slow or timed-out connection from blocking all
/// other sessions.
pub async fn establish_telnet_connection(
    params: TelnetConnectParams,
) -> Result<TelnetSession, String> {
    let TelnetConnectParams {
        id,
        host,
        port,
        cols,
        rows,
        app_handle,
        restrict_private_ips,
        connect_timeout_secs,
        logger,
        ready_rx,
        login_sequence,
    } = params;

    // Validate against restricted IP ranges if enabled.
    if restrict_private_ips {
        crate::ssh::validate_ssh_target_public(&host, port)?;
    }

    info!(session_id = %id, host = %host, port = %port, "Connecting via Telnet");

    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(connect_timeout_secs),
        TcpStream::connect((host.as_str(), port)),
    )
    .await
    .map_err(|_| format!("Telnet connection to {host}:{port} timed out"))?
    .map_err(|e| sanitize_telnet_error(&e.to_string()))?;

    let (read_half, write_half) = stream.into_split();

    // Channel for sending data to the writer task.
    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(64);
    // Channel for resize requests.
    // Watch channel for resize requests. `None` initial state means the
    // reader task does not fire a redundant NAWS at startup — the initial
    // NAWS is already sent during the option negotiation in `telnet_io_loop`.
    let (resize_tx, resize_rx) = watch::channel::<Option<ResizeRequest>>(None);

    let event_name = format!("terminal-output-{id}");
    let exit_event = format!("terminal-exit-{id}");
    let session_id = id.clone();
    let app = app_handle;

    let reader_task = tokio::spawn(async move {
        match tokio::time::timeout(std::time::Duration::from_secs(5), ready_rx).await {
            Ok(Ok(())) => {
                debug!(session_id = %session_id, "Frontend listener ready, starting Telnet reader");
            }
            Ok(Err(_)) => {
                debug!(session_id = %session_id, "Ready signal sender dropped, starting Telnet reader anyway");
            }
            Err(_) => {
                warn!(session_id = %session_id, "Frontend ready signal timed out after 5s, starting Telnet reader anyway");
            }
        }
        telnet_io_loop(
            read_half,
            write_half,
            write_rx,
            resize_rx,
            cols,
            rows,
            &event_name,
            &exit_event,
            &session_id,
            &app,
            logger,
            login_sequence,
        )
        .await;
    });

    info!(session_id = %id, "Telnet session established");

    Ok(TelnetSession {
        reader_task,
        write_tx,
        resize_tx,
        write_semaphore: Arc::new(Semaphore::new(crate::limits::MAX_INFLIGHT_WRITES)),
    })
}

// ── I/O loop ────────────────────────────────────────────────────────

/// Main I/O loop: reads from socket, negotiates telnet options, forwards
/// clean data to the frontend, and handles writes + resizes.
#[allow(clippy::too_many_arguments)]
async fn telnet_io_loop(
    mut reader: tokio::net::tcp::OwnedReadHalf,
    mut writer: tokio::net::tcp::OwnedWriteHalf,
    mut write_rx: mpsc::Receiver<Vec<u8>>,
    mut resize_rx: watch::Receiver<Option<ResizeRequest>>,
    initial_cols: u16,
    initial_rows: u16,
    event_name: &str,
    exit_event: &str,
    session_id: &str,
    app: &AppHandle,
    logger: Option<std::sync::Arc<std::sync::Mutex<crate::session_logger::SessionLogManager>>>,
    login_sequence: Option<crate::login_sequence::LoginSequenceConfig>,
) {
    // Send initial WILL NAWS + window size so the server knows our terminal size.
    let mut init = vec![IAC, WILL, OPT_NAWS];
    init.extend_from_slice(&naws_subnegotiation(initial_cols, initial_rows));
    if writer.write_all(&init).await.is_err() {
        let _ = app.emit(exit_event, ());
        return;
    }

    let mut buf = [0u8; 4096];
    // State machine for IAC parsing.
    let mut iac_state = IacState::Normal;
    // Buffer for clean (non-IAC) data to emit.
    let mut out_buf = Vec::with_capacity(4096);

    if let Some(config) = login_sequence {
        let io = crate::login_sequence::LoginSequenceIO {
            config,
            event_name,
            app,
            session_id,
            logger: logger.as_ref(),
        };
        let mut transport = TelnetLoginTransport {
            reader: &mut reader,
            writer: &mut writer,
            iac_state: &mut iac_state,
            buf: &mut buf,
            out_buf: &mut out_buf,
        };
        crate::login_sequence::run_login_sequence(io, &mut transport).await;
    }

    loop {
        tokio::select! {
            result = reader.read(&mut buf) => {
                match result {
                    Ok(0) => {
                        info!(session_id = %session_id, "Telnet connection closed");
                        if let Some(ref lg) = logger {
                            if let Ok(mut mgr) = lg.lock() {
                                mgr.close_log(session_id);
                            }
                        }
                        let _ = app.emit(exit_event, ());
                        break;
                    }
                    Ok(n) => {
                        out_buf.clear();
                        let mut responses: Vec<u8> = Vec::new();
                        for &byte in &buf[..n] {
                            process_iac_byte(byte, &mut iac_state, &mut out_buf, &mut responses);
                        }

                        if !responses.is_empty()
                            && writer.write_all(&responses).await.is_err()
                        {
                            let _ = app.emit(exit_event, ());
                            break;
                        }

                        // Emit clean terminal data to frontend.
                        if !out_buf.is_empty() {
                            if let Some(ref lg) = logger {
                                if let Ok(mut mgr) = lg.lock() {
                                    mgr.write_log(session_id, &out_buf);
                                }
                            }
                            let payload = base64::prelude::BASE64_STANDARD.encode(&out_buf);
                            if app.emit(event_name, &payload).is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(session_id = %session_id, error = %e, "Telnet read error");
                        if let Some(ref lg) = logger {
                            if let Ok(mut mgr) = lg.lock() {
                                mgr.close_log(session_id);
                            }
                        }
                        let _ = app.emit(exit_event, ());
                        break;
                    }
                }
            }
            Some(data) = write_rx.recv() => {
                if writer.write_all(&data).await.is_err() {
                    if let Some(ref lg) = logger {
                        if let Ok(mut mgr) = lg.lock() {
                            mgr.close_log(session_id);
                        }
                    }
                    let _ = app.emit(exit_event, ());
                    break;
                }
            }
            Ok(()) = resize_rx.changed() => {
                let pending = *resize_rx.borrow_and_update();
                if let Some(req) = pending {
                    let naws = naws_subnegotiation(req.cols as u16, req.rows as u16);
                    if writer.write_all(&naws).await.is_err() {
                        warn!(session_id = %session_id, "Failed to send NAWS resize");
                    }
                }
            }
        }
    }
}

/// `IAC` state machine states.
#[derive(Clone, Copy)]
enum IacState {
    Normal,
    Iac,
    Will,
    Wont,
    Do,
    Dont,
    Sb,
    SbIac,
}

/// Telnet `IAC` (Interpret As Command) state machine: one byte in, mutating
/// `state` and either appending to `out_buf` (real terminal data) or to
/// `responses` (option-negotiation reply).
///
/// ## Why this function is large
///
/// RFC 854 specifies a strict state machine: every `IAC` byte introduces a
/// command, the next byte selects between WILL/WONT/DO/DONT/SB/SE and the
/// byte after that names the option. The `match (state, byte)` shape
/// here is a direct transcription of that table — each arm corresponds
/// to one cell in the RFC 854 state diagram, and the branchiness
/// measured by cyclomatic complexity is intrinsic to the protocol, not
/// to the implementation.
///
/// The function is unit-testable in principle (drive it byte-by-byte
/// with synthetic `IAC` sequences and assert the resulting state +
/// responses), but each test would need to know the RFC 854 layout
/// in detail to be meaningful. Adding such a corpus is a separate work
/// item — the per-byte transitions here cannot be simplified without
/// rewriting against a different protocol.
fn process_iac_byte(
    byte: u8,
    state: &mut IacState,
    out_buf: &mut Vec<u8>,
    responses: &mut Vec<u8>,
) {
    match *state {
        IacState::Normal => {
            if byte == IAC {
                *state = IacState::Iac;
            } else {
                out_buf.push(byte);
            }
        }
        IacState::Iac => match byte {
            WILL => *state = IacState::Will,
            WONT => *state = IacState::Wont,
            DO => *state = IacState::Do,
            DONT => *state = IacState::Dont,
            SB => *state = IacState::Sb,
            IAC => {
                out_buf.push(IAC);
                *state = IacState::Normal;
            }
            _ => {
                *state = IacState::Normal;
            }
        },
        IacState::Will => {
            match byte {
                OPT_ECHO | OPT_SUPPRESS_GO_AHEAD => {
                    responses.extend_from_slice(&[IAC, DO, byte]);
                }
                _ => {
                    responses.extend_from_slice(&[IAC, DONT, byte]);
                }
            }
            *state = IacState::Normal;
        }
        IacState::Wont => {
            responses.extend_from_slice(&[IAC, DONT, byte]);
            *state = IacState::Normal;
        }
        IacState::Do => {
            match byte {
                OPT_NAWS => {}
                OPT_SUPPRESS_GO_AHEAD => {
                    responses.extend_from_slice(&[IAC, WILL, byte]);
                }
                _ => {
                    responses.extend_from_slice(&[IAC, WONT, byte]);
                }
            }
            *state = IacState::Normal;
        }
        IacState::Dont => {
            responses.extend_from_slice(&[IAC, WONT, byte]);
            *state = IacState::Normal;
        }
        IacState::Sb => {
            if byte == IAC {
                *state = IacState::SbIac;
            }
        }
        IacState::SbIac => {
            if byte == SE {
                *state = IacState::Normal;
            } else {
                *state = IacState::Sb;
            }
        }
    }
}

struct TelnetLoginTransport<'a> {
    reader: &'a mut tokio::net::tcp::OwnedReadHalf,
    writer: &'a mut tokio::net::tcp::OwnedWriteHalf,
    iac_state: &'a mut IacState,
    buf: &'a mut [u8; 4096],
    out_buf: &'a mut Vec<u8>,
}

#[async_trait::async_trait]
impl crate::login_sequence::LoginSequenceTransport for TelnetLoginTransport<'_> {
    async fn read(&mut self) -> Option<Vec<u8>> {
        loop {
            match self.reader.read(&mut *self.buf).await {
                Ok(0) | Err(_) => return None,
                Ok(n) => {
                    self.out_buf.clear();
                    let mut responses: Vec<u8> = Vec::new();
                    for &byte in &self.buf[..n] {
                        process_iac_byte(byte, self.iac_state, self.out_buf, &mut responses);
                    }
                    if !responses.is_empty() && self.writer.write_all(&responses).await.is_err() {
                        return None;
                    }
                    if !self.out_buf.is_empty() {
                        return Some(self.out_buf.clone());
                    }
                }
            }
        }
    }

    async fn write(&mut self, data: &[u8]) -> Result<(), String> {
        self.writer
            .write_all(data)
            .await
            .map_err(|e| format!("Telnet write error: {e}"))
    }
}

/// Build a `NAWS` sub-negotiation payload: `IAC` `SB` `NAWS` <`w_hi`> <`w_lo`> <`h_hi`> <`h_lo`> `IAC` `SE`.
/// Any 0xFF bytes in the size values must be doubled (escaped) per RFC 855.
fn naws_subnegotiation(cols: u16, rows: u16) -> Vec<u8> {
    let mut buf = vec![IAC, SB, OPT_NAWS];
    for byte in cols.to_be_bytes() {
        buf.push(byte);
        if byte == IAC {
            buf.push(IAC);
        }
    }
    for byte in rows.to_be_bytes() {
        buf.push(byte);
        if byte == IAC {
            buf.push(IAC);
        }
    }
    buf.push(IAC);
    buf.push(SE);
    buf
}

/// Sanitize Telnet error messages to avoid leaking internal network topology.
fn sanitize_telnet_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("connection refused") {
        "Telnet connection failed: connection refused".to_string()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "Telnet connection failed: connection timed out".to_string()
    } else if lower.contains("no route") || lower.contains("network is unreachable") {
        "Telnet connection failed: host unreachable".to_string()
    } else if lower.contains("name or service not known") || lower.contains("resolve") {
        "Telnet connection failed: hostname could not be resolved".to_string()
    } else {
        "Telnet connection failed".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // sanitize_telnet_error mirrors sanitize_ssh_error in structure but
    // covers the smaller telnet error surface. Tested for the same
    // reason: internal OS paths / FDs / addresses must not leak through.

    #[test]
    fn telnet_connection_refused() {
        assert!(sanitize_telnet_error("Connection refused (os error 111)")
            .contains("connection refused"));
    }

    #[test]
    fn telnet_timeout() {
        assert!(sanitize_telnet_error("connection timed out").contains("timed out"));
        assert!(sanitize_telnet_error("read timeout").contains("timed out"));
    }

    #[test]
    fn telnet_unreachable() {
        assert!(sanitize_telnet_error("No route to host").contains("unreachable"));
        assert!(sanitize_telnet_error("Network is unreachable").contains("unreachable"));
    }

    #[test]
    fn telnet_dns() {
        assert!(sanitize_telnet_error("Name or service not known").contains("resolved"));
        assert!(sanitize_telnet_error("could not resolve hostname").contains("resolved"));
    }

    #[test]
    fn telnet_fallback() {
        let s = sanitize_telnet_error("/dev/pts/3: I/O error reading from device");
        assert_eq!(s, "Telnet connection failed");
    }

    // ── get_write_sender ─────────────────────────────────────────────────
    // A dead session must reject writes with the exact string the frontend
    // matches, or its tab keeps looking alive while input goes nowhere.

    /// A session whose write channel receiver stands in for the I/O task.
    fn session_with_receiver() -> (TelnetSession, mpsc::Receiver<Vec<u8>>) {
        session_with_task(tokio::spawn(async {}))
    }

    /// A session around a given I/O task.
    fn session_with_task(task: JoinHandle<()>) -> (TelnetSession, mpsc::Receiver<Vec<u8>>) {
        let (write_tx, write_rx) = mpsc::channel(1);
        let (resize_tx, _) = watch::channel(None);
        let session = TelnetSession {
            reader_task: task,
            write_tx,
            resize_tx,
            write_semaphore: Arc::new(Semaphore::new(1)),
        };
        (session, write_rx)
    }

    /// A session whose I/O task runs until stopped, standing in for one that
    /// holds a live socket, and a receiver that resolves once it stops.
    fn session_with_live_task() -> (TelnetSession, tokio::sync::oneshot::Receiver<()>) {
        let (stopped_tx, stopped_rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _stopped = stopped_tx; // dropped with the task
            std::future::pending::<()>().await;
        });
        (session_with_task(task).0, stopped_rx)
    }

    const STOPPED_WITHIN: std::time::Duration = std::time::Duration::from_secs(5);

    // ── Closing sessions ─────────────────────────────────────────────────
    // A session's I/O task owns its socket, so a task left running is a
    // connection left open.

    #[tokio::test]
    async fn refused_session_stops_its_io_task() {
        let mut manager = TelnetManager::default();
        for i in 0..TelnetManager::MAX_SESSIONS {
            let (session, _) = session_with_receiver();
            manager.register_session(i.to_string(), session).unwrap();
        }

        let (session, stopped) = session_with_live_task();
        assert!(manager
            .register_session("over".to_string(), session)
            .is_err());
        assert!(tokio::time::timeout(STOPPED_WITHIN, stopped).await.is_ok());
    }

    #[tokio::test]
    async fn dropped_session_stops_its_io_task() {
        let (session, stopped) = session_with_live_task();
        drop(session);
        assert!(tokio::time::timeout(STOPPED_WITHIN, stopped).await.is_ok());
    }

    #[tokio::test]
    async fn close_returns_after_the_io_task_stopped() {
        let (session, mut stopped) = session_with_live_task();
        session.close().await;
        assert!(matches!(
            stopped.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
    }

    #[tokio::test]
    async fn remove_hands_back_the_session() {
        let mut manager = TelnetManager::default();
        let (session, stopped) = session_with_live_task();
        manager.register_session("s".to_string(), session).unwrap();

        let removed = manager.remove("s").unwrap();
        assert!(manager.get_write_sender("s").is_err());
        removed.close().await;
        assert!(tokio::time::timeout(STOPPED_WITHIN, stopped).await.is_ok());

        assert_eq!(
            manager.remove("s").err().as_deref(),
            Some("Telnet session s not found")
        );
    }

    #[tokio::test]
    async fn take_all_empties_the_manager() {
        let mut manager = TelnetManager::default();
        let (session, stopped) = session_with_live_task();
        manager.register_session("s".to_string(), session).unwrap();

        let taken = manager.take_all();
        assert_eq!(taken.len(), 1);
        assert!(manager.get_write_sender("s").is_err());
        drop(taken);
        assert!(tokio::time::timeout(STOPPED_WITHIN, stopped).await.is_ok());
    }

    #[tokio::test]
    async fn write_sender_available_while_io_task_runs() {
        let (session, _write_rx) = session_with_receiver();
        let mut manager = TelnetManager::default();
        manager.register_session("s".to_string(), session).unwrap();

        assert!(manager.get_write_sender("s").is_ok());
    }

    #[tokio::test]
    async fn write_sender_rejects_closed_session() {
        let (session, write_rx) = session_with_receiver();
        let mut manager = TelnetManager::default();
        manager.register_session("s".to_string(), session).unwrap();

        drop(write_rx); // the I/O task ended
        assert_eq!(
            manager.get_write_sender("s").err().as_deref(),
            Some(SESSION_CLOSED_ERROR)
        );
    }

    #[test]
    fn write_sender_rejects_unknown_session() {
        let manager = TelnetManager::default();
        let err = manager.get_write_sender("missing").unwrap_err();
        assert_eq!(err, "Telnet session missing not found");
    }
}
