use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::ssh::ResizeRequest;

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
}

/// Per-connection Telnet session state.
pub(crate) struct TelnetSession {
    reader_task: JoinHandle<()>,
    write_tx: mpsc::Sender<Vec<u8>>,
    resize_tx: mpsc::Sender<ResizeRequest>,
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
    pub fn register_session(&mut self, id: String, session: TelnetSession) -> Result<(), String> {
        self.check_capacity()?;
        self.sessions.insert(id, session);
        Ok(())
    }

    /// Get the write sender for a session.
    pub fn get_write_sender(&self, id: &str) -> Result<mpsc::Sender<Vec<u8>>, String> {
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| format!("Telnet session {id} not found"))?;
        Ok(session.write_tx.clone())
    }

    /// Get the resize sender for a session.
    pub fn get_resize_sender(&self, id: &str) -> Result<mpsc::Sender<ResizeRequest>, String> {
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| format!("Telnet session {id} not found"))?;
        Ok(session.resize_tx.clone())
    }

    /// Disconnect a Telnet session.
    pub async fn disconnect(&mut self, id: &str) -> Result<(), String> {
        let session = self
            .sessions
            .remove(id)
            .ok_or_else(|| format!("Telnet session {id} not found"))?;
        session.reader_task.abort();
        info!(session_id = %id, "Telnet session disconnected");
        Ok(())
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
    let (resize_tx, resize_rx) = mpsc::channel::<ResizeRequest>(4);

    let event_name = format!("terminal-output-{id}");
    let exit_event = format!("terminal-exit-{id}");
    let session_id = id.clone();
    let app = app_handle;

    let reader_task = tokio::spawn(async move {
        match tokio::time::timeout(std::time::Duration::from_secs(5), ready_rx).await {
            Ok(Ok(())) => {
                debug!(session_id = %session_id, "Frontend listener ready, starting Telnet reader")
            }
            Ok(Err(_)) => {
                debug!(session_id = %session_id, "Ready signal sender dropped, starting Telnet reader anyway")
            }
            Err(_) => {
                warn!(session_id = %session_id, "Frontend ready signal timed out after 5s, starting Telnet reader anyway")
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
        )
        .await;
    });

    info!(session_id = %id, "Telnet session established");

    Ok(TelnetSession {
        reader_task,
        write_tx,
        resize_tx,
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
    mut resize_rx: mpsc::Receiver<ResizeRequest>,
    initial_cols: u16,
    initial_rows: u16,
    event_name: &str,
    exit_event: &str,
    session_id: &str,
    app: &AppHandle,
    logger: Option<std::sync::Arc<std::sync::Mutex<crate::session_logger::SessionLogManager>>>,
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
                            match iac_state {
                                IacState::Normal => {
                                    if byte == IAC {
                                        iac_state = IacState::Iac;
                                    } else {
                                        out_buf.push(byte);
                                    }
                                }
                                IacState::Iac => {
                                    match byte {
                                        WILL => iac_state = IacState::Will,
                                        WONT => iac_state = IacState::Wont,
                                        DO => iac_state = IacState::Do,
                                        DONT => iac_state = IacState::Dont,
                                        SB => iac_state = IacState::Sb,
                                        IAC => {
                                            // Escaped 0xFF — emit literal byte.
                                            out_buf.push(IAC);
                                            iac_state = IacState::Normal;
                                        }
                                        _ => {
                                            // Unknown command — ignore.
                                            iac_state = IacState::Normal;
                                        }
                                    }
                                }
                                IacState::Will => {
                                    // Server offers to enable an option.
                                    match byte {
                                        OPT_ECHO | OPT_SUPPRESS_GO_AHEAD => {
                                            responses.extend_from_slice(&[IAC, DO, byte]);
                                        }
                                        _ => {
                                            responses.extend_from_slice(&[IAC, DONT, byte]);
                                        }
                                    }
                                    iac_state = IacState::Normal;
                                }
                                IacState::Wont => {
                                    // Acknowledge.
                                    responses.extend_from_slice(&[IAC, DONT, byte]);
                                    iac_state = IacState::Normal;
                                }
                                IacState::Do => {
                                    // Server asks us to enable an option.
                                    match byte {
                                        OPT_NAWS => {
                                            // Already sent WILL NAWS during init.
                                        }
                                        OPT_SUPPRESS_GO_AHEAD => {
                                            responses.extend_from_slice(&[IAC, WILL, byte]);
                                        }
                                        _ => {
                                            responses.extend_from_slice(&[IAC, WONT, byte]);
                                        }
                                    }
                                    iac_state = IacState::Normal;
                                }
                                IacState::Dont => {
                                    responses.extend_from_slice(&[IAC, WONT, byte]);
                                    iac_state = IacState::Normal;
                                }
                                IacState::Sb => {
                                    // Inside sub-negotiation — skip until IAC SE.
                                    if byte == IAC {
                                        iac_state = IacState::SbIac;
                                    }
                                    // Otherwise stay in Sb, consuming bytes.
                                }
                                IacState::SbIac => {
                                    if byte == SE {
                                        iac_state = IacState::Normal;
                                    } else {
                                        // Not SE — still in sub-negotiation.
                                        iac_state = IacState::Sb;
                                    }
                                }
                            }
                        }

                        // Send option responses back to server.
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
            Some(req) = resize_rx.recv() => {
                let naws = naws_subnegotiation(req.cols as u16, req.rows as u16);
                if writer.write_all(&naws).await.is_err() {
                    warn!(session_id = %session_id, "Failed to send NAWS resize");
                }
            }
        }
    }
}

/// IAC state machine states.
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

/// Build a NAWS sub-negotiation payload: IAC SB NAWS <w_hi> <w_lo> <h_hi> <h_lo> IAC SE.
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
