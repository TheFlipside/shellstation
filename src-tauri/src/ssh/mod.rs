use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use russh::client::KeyboardInteractiveAuthResponse;
use russh::keys::{PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{client, ChannelMsg, Disconnect};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{oneshot, watch, Mutex, Semaphore};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, trace, warn};
use zeroize::Zeroizing;

/// Pending TOFU verification senders, stored separately from `SshManager`
/// so that `ssh_host_verify_response` can resolve a pending verification
/// without acquiring the main `SshManager` lock (which is held by the
/// `connect()` call that is waiting for the verification).
type HostVerifySenders = Arc<std::sync::Mutex<HashMap<String, oneshot::Sender<bool>>>>;

/// Pending keyboard-interactive auth senders. Same pattern as host verify:
/// uses `std::sync::Mutex` to avoid deadlock with the manager's tokio Mutex.
pub type KbdInteractiveSenders =
    Arc<std::sync::Mutex<HashMap<String, oneshot::Sender<Zeroizing<Vec<String>>>>>>;

/// Payload emitted to the frontend when the server key needs user verification.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostVerifyPayload {
    pub session_id: String,
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    pub key_type: String,
    /// What `known_hosts` has on record for the host.
    pub status: HostKeyStatus,
    /// The keys on record for the host: of the offered key's type when
    /// `Changed`, of other types when `OtherType`, none otherwise.
    pub known_keys: Vec<KnownKey>,
}

/// What `known_hosts` has on record for a host whose key the user is asked
/// to trust.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum HostKeyStatus {
    /// Nothing: a first connection.
    New,
    /// A different key of the same type, as when a man in the middle answers.
    Changed,
    /// Only keys of other types. A man in the middle could offer another
    /// type to avoid `Changed`; OpenSSH warns about this as well.
    OtherType,
    /// Unknown: `known_hosts` could not be read, or an entry for the host
    /// could not be parsed, so the key was not compared with what is there.
    Unchecked,
}

/// A key on record for a host, as the prompt shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownKey {
    /// The key's algorithm, such as `ssh-ed25519`.
    pub key_type: String,
    /// The key's SHA-256 fingerprint, as OpenSSH prints it.
    pub fingerprint: String,
}

impl From<&russh::keys::PublicKey> for KnownKey {
    fn from(key: &russh::keys::PublicKey) -> Self {
        Self {
            key_type: key.algorithm().as_str().to_string(),
            fingerprint: key.fingerprint(russh::keys::HashAlg::Sha256).to_string(),
        }
    }
}

/// Payload emitted to the frontend for keyboard-interactive authentication.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KbdInteractivePayload {
    pub session_id: String,
    pub name: String,
    pub instructions: String,
    pub prompts: Vec<KbdInteractivePrompt>,
}

/// A single prompt within a keyboard-interactive authentication request.
#[derive(Clone, Serialize)]
pub struct KbdInteractivePrompt {
    pub prompt: String,
    pub echo: bool,
}

/// Resize request sent to the reader task which owns the channel.
#[derive(Clone, Copy)]
pub struct ResizeRequest {
    pub cols: u32,
    pub rows: u32,
}

/// How long closing a connection chain (the target, then every jump host)
/// waits, in all, for its connections to close. Once it has passed, the rest
/// are still sent `SSH_MSG_DISCONNECT` where it can go out at once, but not
/// waited for.
const DISCONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// The connection under a session: the target, and the jump hosts it runs
/// through, nearest the client first. Shared by the session and its reader
/// task, so whichever ends first — the user closing the tab, or the session
/// ending on its own — closes all of it.
struct SshConnection<H: client::Handler = SshHandler> {
    target: Arc<client::Handle<H>>,
    bastions: Vec<client::Handle<H>>,
}

impl<H: client::Handler> SshConnection<H> {
    fn is_closed(&self) -> bool {
        self.target.is_closed() && self.bastions.iter().all(client::Handle::is_closed)
    }

    /// Disconnect the target, then each jump host back toward the client,
    /// waiting (within [`DISCONNECT_TIMEOUT`]) for every one of them to
    /// close. A reconnect that follows cannot reach the device while it
    /// still counts the old session.
    async fn close(&self) {
        let deadline = tokio::time::Instant::now() + DISCONNECT_TIMEOUT;
        disconnect_handle(&self.target, deadline).await;
        for bastion in self.bastions.iter().rev() {
            disconnect_handle(bastion, deadline).await;
        }
    }
}

/// Send `SSH_MSG_DISCONNECT` and wait until the connection has closed, or
/// `deadline` has passed. russh writes the message out before it closes the
/// handle's channel, so a closed handle means the server has it.
async fn disconnect_handle<H: client::Handler>(
    handle: &client::Handle<H>,
    deadline: tokio::time::Instant,
) {
    if handle.is_closed() {
        return;
    }
    // `timeout_at` tries the send before it looks at the deadline, so it
    // still goes out once the deadline has passed if it can do so at once.
    // A failed send means the session has ended already, which the wait
    // below sees straight away.
    let _ = tokio::time::timeout_at(
        deadline,
        handle.disconnect(Disconnect::ByApplication, "", "en"),
    )
    .await;
    while !handle.is_closed() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

/// The connections of a chain still being set up: the jump hosts nearest the
/// client first, then the target once it is up. Until the session is open,
/// dropping this — setup failed, or its future was dropped part way —
/// disconnects every connection already up, the farthest first, instead of
/// leaving them to close once russh sees their handles go.
struct PendingChain<H: client::Handler + 'static = SshHandler>(Vec<client::Handle<H>>);

impl<H: client::Handler + 'static> Default for PendingChain<H> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<H: client::Handler + 'static> PendingChain<H> {
    /// Add the connection just made, and return it.
    fn push(&mut self, hop: client::Handle<H>) -> &mut client::Handle<H> {
        self.0.push(hop);
        self.last_mut()
    }

    /// The connection farthest from the client.
    fn last(&self) -> &client::Handle<H> {
        self.0.last().expect("only used once a connection is in")
    }

    fn last_mut(&mut self) -> &mut client::Handle<H> {
        self.0
            .last_mut()
            .expect("only used once a connection is in")
    }

    /// Disconnect every connection now, the farthest first, waiting as
    /// [`SshConnection::close`] does. The connections stay in until then, so
    /// should this be dropped part way, `Drop` still closes them.
    async fn close(mut self) {
        disconnect_all(&self.0).await;
        self.0.clear();
    }

    /// The set-up chain as a connection, the last one in being its target.
    fn into_connection(mut self) -> SshConnection<H> {
        let mut bastions = std::mem::take(&mut self.0);
        let target = bastions.pop().expect("only used once the target is in");
        SshConnection {
            target: Arc::new(target),
            bastions,
        }
    }
}

impl<H: client::Handler + 'static> Drop for PendingChain<H> {
    fn drop(&mut self) {
        let hops = std::mem::take(&mut self.0);
        if !hops.is_empty() {
            tauri::async_runtime::spawn(async move { disconnect_all(&hops).await });
        }
    }
}

/// Disconnect a chain listed nearest the client first, the farthest first,
/// within one [`DISCONNECT_TIMEOUT`].
async fn disconnect_all<H: client::Handler>(hops: &[client::Handle<H>]) {
    let deadline = tokio::time::Instant::now() + DISCONNECT_TIMEOUT;
    for hop in hops.iter().rev() {
        disconnect_handle(hop, deadline).await;
    }
}

/// Per-connection SSH session state.
pub(crate) struct SshSession {
    connection: Arc<SshConnection>,
    channel_id: russh::ChannelId,
    reader_task: JoinHandle<()>,
    /// Watch channel carrying the latest pending resize. `None` is the
    /// initial sentinel — see `establish_ssh_connection` for the rationale.
    /// A watch (latest-wins) is the right primitive here: intermediate
    /// resize values during a burst are safe to discard, and the reader
    /// task only ever needs the most recent dimensions.
    resize_tx: watch::Sender<Option<ResizeRequest>>,
    /// Bounds the number of in-flight write tasks per session. Prevents a
    /// runaway frontend from spawning unbounded tasks (each holding up to
    /// `MAX_WRITE_SIZE` bytes) when the SSH socket is congested and russh's
    /// internal channel back-pressures into our spawned tasks.
    write_semaphore: Arc<Semaphore>,
}

impl SshSession {
    /// Close the session: stop the reader and disconnect the whole chain,
    /// waiting until it has closed (see [`SshConnection::close`]).
    pub(crate) async fn close(self) {
        self.reader_task.abort();
        self.connection.close().await;
    }
}

impl Drop for SshSession {
    /// A session dropped without [`SshSession::close`] — refused at
    /// registration, or cleared by a page load — must not stay connected
    /// where nobody can see it. Dropping a russh handle does not close the
    /// connection, and a running reader would keep the channel, so stop the
    /// reader and close the chain in the background.
    fn drop(&mut self) {
        self.reader_task.abort();
        close_in_background(&self.connection);
    }
}

/// Close a connection on a task of its own, unless it is closed already.
fn close_in_background<H: client::Handler + 'static>(connection: &Arc<SshConnection<H>>) {
    if !connection.is_closed() {
        let connection = Arc::clone(connection);
        tauri::async_runtime::spawn(async move { connection.close().await });
    }
}

/// The reader task's share of its connection. Should the task stop before
/// closing the connection itself, by panicking for one, dropping this closes
/// it.
struct ReaderConnection(Arc<SshConnection>);

impl Drop for ReaderConnection {
    fn drop(&mut self) {
        close_in_background(&self.0);
    }
}

/// A single hop in a jump host chain.
#[derive(Clone, serde::Deserialize)]
pub struct JumpHop {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: String,
    pub auth_credential: Zeroizing<String>,
}

/// Parameters for establishing an SSH connection.
pub struct SshConnectParams {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: String,
    pub restrict_private_ips: bool,
    pub connect_timeout_secs: u64,
    pub keepalive_interval_secs: u64,
    pub keepalive_max: usize,
    pub auth_credential: Zeroizing<String>,
    pub cols: u16,
    pub rows: u16,
    pub app_handle: AppHandle,
    pub jump_hops: Vec<JumpHop>,
    /// When true, extend the preferred algorithm list with legacy SSH kex,
    /// ciphers and MACs (group14-sha1, aes*-cbc, hmac-sha1, …). Required to
    /// negotiate with old network gear that doesn't support modern algos.
    ///
    /// Note: since russh 0.60.2 (`Preferred::DEFAULT` no longer includes
    /// SHA-1 MACs), this toggle is the **only** path that re-enables
    /// `hmac-sha1` / `hmac-sha1-etm`. Targets that previously connected on
    /// default settings via SHA-1 MAC negotiation will now need this on.
    pub legacy_algorithms: bool,
    pub logger: Option<std::sync::Arc<std::sync::Mutex<crate::session_logger::SessionLogManager>>>,
    pub kbd_interactive_senders: KbdInteractiveSenders,
    /// Receiver for the "terminal ready" signal. The reader task waits on this
    /// before entering its read loop, preventing early output from being emitted
    /// before the frontend event listener is registered.
    pub ready_rx: oneshot::Receiver<()>,
    pub login_sequence: Option<crate::login_sequence::LoginSequenceConfig>,
}

/// Error returned by `ssh_write` and `telnet_write` once a session's channel
/// or connection has closed. The frontend matches this exact string
/// (`SESSION_CLOSED_ERROR` in `src/terminalRegistry.ts`) to mark the tab
/// exited when the exit event was missed, so the two must change together.
pub const SESSION_CLOSED_ERROR: &str = "Session closed";

/// Manages all active SSH sessions.
#[derive(Default)]
pub struct SshManager {
    sessions: HashMap<String, SshSession>,
}

impl SshManager {
    /// Maximum number of concurrent SSH sessions.
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

    /// Register an established SSH session. Re-checks capacity to prevent
    /// TOCTOU races when multiple connections are established concurrently.
    /// A refused session is dropped, which closes it.
    pub fn register_session(&mut self, id: String, session: SshSession) -> Result<(), String> {
        self.check_capacity()?;
        self.sessions.insert(id, session);
        Ok(())
    }

    /// Get the handle and channel `ID` for writing to a session.
    /// The caller should drop the manager lock before performing the async write.
    ///
    /// Fails with [`SESSION_CLOSED_ERROR`] once the session is dead but still
    /// registered — the reader task has stopped (channel EOF/close, end of the
    /// message stream, or no way left to emit output to the frontend), or the
    /// connection itself is gone. A session stays registered until the
    /// frontend calls `ssh_disconnect`, and the actual send happens on a
    /// detached task, so without this check writes to a dead session would
    /// report success and the frontend would never learn it is gone.
    pub fn get_write_handle(
        &self,
        id: &str,
    ) -> Result<(Arc<client::Handle<SshHandler>>, russh::ChannelId), String> {
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| format!("SSH session {id} not found"))?;
        if session.reader_task.is_finished() || session.connection.target.is_closed() {
            return Err(SESSION_CLOSED_ERROR.to_string());
        }
        Ok((Arc::clone(&session.connection.target), session.channel_id))
    }

    /// Get the resize sender for a session.
    /// The caller should drop the manager lock before sending.
    pub fn get_resize_sender(
        &self,
        id: &str,
    ) -> Result<watch::Sender<Option<ResizeRequest>>, String> {
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| format!("SSH session {id} not found"))?;
        Ok(session.resize_tx.clone())
    }

    /// Get the inflight-writes semaphore for a session.
    /// The caller should drop the manager lock before acquiring a permit.
    pub fn get_write_semaphore(&self, id: &str) -> Result<Arc<Semaphore>, String> {
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| format!("SSH session {id} not found"))?;
        Ok(Arc::clone(&session.write_semaphore))
    }

    /// Unregister a session so the caller can [`SshSession::close`] it after
    /// releasing the manager lock.
    pub fn remove(&mut self, id: &str) -> Result<SshSession, String> {
        self.sessions
            .remove(id)
            .ok_or_else(|| format!("SSH session {id} not found"))
    }

    /// Unregister every session, returning their `ID`s and sessions.
    pub fn take_all(&mut self) -> Vec<(String, SshSession)> {
        self.sessions.drain().collect()
    }
}

/// Establish an SSH connection, authenticate, open a PTY shell, and start
/// streaming output via Tauri events.
///
/// This is intentionally a free function (not a method on `SshManager`) so
/// that callers can perform the expensive TCP handshake and authentication
/// **without** holding the manager lock — preventing one slow or timed-out
/// connection from blocking all other sessions.
pub async fn establish_ssh_connection(
    params: SshConnectParams,
    verify_senders: &HostVerifySenders,
) -> Result<SshSession, String> {
    let SshConnectParams {
        id,
        host,
        port,
        username,
        auth_method,
        auth_credential,
        cols,
        rows,
        app_handle,
        jump_hops,
        legacy_algorithms,
        restrict_private_ips,
        connect_timeout_secs,
        keepalive_interval_secs,
        keepalive_max,
        logger,
        kbd_interactive_senders,
        ready_rx,
        login_sequence,
    } = params;

    info!(session_id = %id, host = %host, port = %port, hops = jump_hops.len(), legacy_algorithms, "Connecting via SSH");

    let mut chain = connect_with_hops(
        &id,
        &host,
        port,
        &jump_hops,
        restrict_private_ips,
        connect_timeout_secs,
        keepalive_interval_secs,
        keepalive_max,
        legacy_algorithms,
        &app_handle,
        verify_senders,
        &kbd_interactive_senders,
    )
    .await?;

    // Authenticate and open a shell. On failure, disconnect the chain that is
    // already up; should this future be dropped part way, the chain's `Drop`
    // does.
    let opened = async {
        authenticate_handle(
            chain.last_mut(),
            &username,
            &auth_method,
            &auth_credential,
            &id,
            &app_handle,
            &kbd_interactive_senders,
        )
        .await?;

        info!(session_id = %id, "SSH authenticated");

        // Open a session channel with PTY.
        debug!(session_id = %id, "Opening session channel");
        let channel = chain
            .last()
            .channel_open_session()
            .await
            .map_err(|e| format!("Failed to open channel: {e}"))?;

        debug!(session_id = %id, term = "xterm-256color", cols, rows, "Requesting PTY");
        channel
            .request_pty(
                false,
                "xterm-256color",
                u32::from(cols),
                u32::from(rows),
                0,
                0,
                &[],
            )
            .await
            .map_err(|e| format!("Failed to request PTY: {e}"))?;

        debug!(session_id = %id, "Requesting shell");
        channel
            .request_shell(false)
            .await
            .map_err(|e| format!("Failed to request shell: {e}"))?;

        Ok::<_, String>(channel)
    }
    .await;
    let channel = match opened {
        Ok(channel) => channel,
        Err(e) => {
            chain.close().await;
            return Err(e);
        }
    };

    let channel_id = channel.id();

    // Watch channel for resize requests. `None` initial state means the
    // reader task does not fire a redundant `window_change` at startup —
    // the PTY was already requested with the connect-time cols/rows above.
    // Latest-wins semantics: under a resize burst the reader picks up only
    // the most recent dimensions on its next iteration; intermediate values
    // are correctly discarded by the watch primitive itself.
    let (resize_tx, mut resize_rx) = watch::channel::<Option<ResizeRequest>>(None);

    // Spawn a reader task that owns the channel exclusively.
    // It streams output to the frontend and handles resize requests.
    let event_name = format!("terminal-output-{id}");
    let exit_event = format!("terminal-exit-{id}");
    let session_id = id.clone();
    let app = app_handle;

    // From here the session owns the chain: its `Drop` and the reader close it.
    let connection = Arc::new(chain.into_connection());
    let reader_connection = ReaderConnection(Arc::clone(&connection));

    let reader_task = tokio::spawn(async move {
        // Wait for the frontend to signal that its event listener is registered.
        // Timeout after 5 seconds as a safety net — better to possibly miss early
        // output than to hang the reader indefinitely if the frontend never signals.
        match tokio::time::timeout(std::time::Duration::from_secs(5), ready_rx).await {
            Ok(Ok(())) => {
                debug!(session_id = %session_id, "Frontend listener ready, starting SSH reader");
            }
            Ok(Err(_)) => {
                debug!(session_id = %session_id, "Ready signal sender dropped, starting SSH reader anyway");
            }
            Err(_) => {
                warn!(session_id = %session_id, "Frontend ready signal timed out after 5s, starting SSH reader anyway");
            }
        }
        let mut channel = channel;

        if let Some(config) = login_sequence {
            let io = crate::login_sequence::LoginSequenceIO {
                config,
                event_name: &event_name,
                app: &app,
                session_id: &session_id,
                logger: logger.as_ref(),
            };
            let mut transport = crate::login_sequence::SshChannelTransport {
                channel: &mut channel,
            };
            crate::login_sequence::run_login_sequence(io, &mut transport).await;
        }

        loop {
            tokio::select! {
                msg = channel.wait() => {
                    match msg {
                        Some(
                            ChannelMsg::Data { ref data }
                            | ChannelMsg::ExtendedData { ref data, ext: 1 },
                        ) => {
                            if let Some(ref lg) = logger {
                                if let Ok(mut mgr) = lg.lock() {
                                    mgr.write_log(&session_id, data.as_ref());
                                }
                            }
                            let payload = base64::prelude::BASE64_STANDARD.encode(data.as_ref());
                            if app.emit(&event_name, &payload).is_err() {
                                break;
                            }
                        }
                        Some(ChannelMsg::ExitStatus { exit_status }) => {
                            info!(session_id = %session_id, exit_status, "SSH process exited");
                        }
                        Some(ChannelMsg::Eof | ChannelMsg::Close) => {
                            info!(session_id = %session_id, "SSH channel closed");
                            if let Some(ref lg) = logger {
                                if let Ok(mut mgr) = lg.lock() {
                                    mgr.close_log(&session_id);
                                }
                            }
                            let _ = app.emit(&exit_event, ());
                            break;
                        }
                        Some(_) => {}
                        None => {
                            if let Some(ref lg) = logger {
                                if let Ok(mut mgr) = lg.lock() {
                                    mgr.close_log(&session_id);
                                }
                            }
                            let _ = app.emit(&exit_event, ());
                            break;
                        }
                    }
                }
                Ok(()) = resize_rx.changed() => {
                    // borrow_and_update marks the change consumed; subsequent
                    // .changed() calls will only fire on the next overwrite.
                    let pending = *resize_rx.borrow_and_update();
                    if let Some(req) = pending {
                        if let Err(e) = channel.window_change(req.cols, req.rows, 0, 0).await {
                            warn!(session_id = %session_id, error = %e, "Failed to resize SSH PTY");
                        }
                    }
                }
            }
        }

        // The session is over: the remote closed the channel, the connection
        // dropped, or output can no longer reach the frontend. Close the
        // connection and its jump hosts now, not when the tab is closed —
        // until then their keepalives would tell the device, and every
        // bastion, that the session is still there. Should the tab be closed
        // meanwhile, ssh_disconnect stops this task part way and its own
        // close finishes the job.
        drop(channel);
        reader_connection.0.close().await;
        if let Some(ref lg) = logger {
            if let Ok(mut mgr) = lg.lock() {
                mgr.close_log(&session_id);
            }
        }
        info!(session_id = %session_id, "SSH connection closed after the session ended");
    });

    info!(session_id = %id, "SSH session established");

    Ok(SshSession {
        connection,
        channel_id,
        reader_task,
        resize_tx,
        write_semaphore: Arc::new(Semaphore::new(crate::limits::MAX_INFLIGHT_WRITES)),
    })
}

/// Thread-safe wrapper around `SshManager` for use as Tauri managed state.
pub struct SshState {
    pub manager: Arc<Mutex<SshManager>>,
    /// Exposed separately so `ssh_host_verify_response` can resolve a pending
    /// verification without acquiring the main manager `tokio::Mutex`, and so
    /// a page load can drop pending prompts for connects it has orphaned.
    pub host_verify_senders: HostVerifySenders,
    /// Same pattern for keyboard-interactive auth prompts.
    pub kbd_interactive_senders: KbdInteractiveSenders,
}

impl Default for SshState {
    fn default() -> Self {
        Self {
            manager: Arc::new(Mutex::new(SshManager::default())),
            host_verify_senders: Arc::new(std::sync::Mutex::new(HashMap::new())),
            kbd_interactive_senders: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }
}

/// Remove a pending host-verify sender from the map. Called on connection error
/// paths to prevent leaked entries when `check_server_key` was never invoked or
/// timed out before the connection itself failed.
fn cleanup_verify_sender(senders: &HostVerifySenders, id: &str) {
    if let Ok(mut map) = senders.lock() {
        map.remove(id);
    }
}

/// Remove a pending keyboard-interactive sender from the map. Called on error
/// paths (emit failure, timeout, channel drop) to prevent leaked entries.
fn cleanup_kbd_interactive_sender(senders: &KbdInteractiveSenders, id: &str) {
    if let Ok(mut map) = senders.lock() {
        map.remove(id);
    }
}

/// Sanitize SSH error messages to avoid leaking internal network topology.
/// The raw error is already logged server-side; user-facing messages are generic.
fn sanitize_ssh_error(raw: &str) -> String {
    debug!(raw_error = %raw, "Sanitizing SSH error for user display");
    let lower = raw.to_lowercase();
    if lower.contains("connection refused") {
        "SSH connection failed: connection refused".to_string()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "SSH connection failed: connection timed out".to_string()
    } else if lower.contains("no route") || lower.contains("network is unreachable") {
        "SSH connection failed: host unreachable".to_string()
    } else if lower.contains("name or service not known") || lower.contains("resolve") {
        "SSH connection failed: hostname could not be resolved".to_string()
    } else if lower.contains("no common ")
        || lower.contains("key exchange init failed")
        || lower.contains("key exchange failed")
    {
        // russh raises `Error::NoCommonAlgo { kind, ours, theirs }` (displayed
        // as "No common Kex/Cipher/Mac/Key algorithm …") when the negotiated
        // lists don't overlap, and `Error::KexInit` / `Error::Kex` when the
        // resulting exchange can't proceed. All of these are the symptom of
        // old gear that only speaks legacy algorithms.
        "SSH connection failed: no shared algorithm with the server. \
         The remote device likely only supports legacy SSH algorithms \
         (e.g. diffie-hellman-group14-sha1, ssh-rsa, aes-cbc). Enable \
         \"Legacy algorithms\" on this session and reconnect."
            .to_string()
    } else if lower.contains("authentication") {
        "SSH connection failed: authentication error".to_string()
    } else {
        "SSH connection failed".to_string()
    }
}

/// Parse a public key credential string. Format: "`key_path`" or "`key_path`\npassphrase".
/// Expands `~` to the user's home directory and validates the path is within it.
fn parse_key_credential(credential: &str) -> Result<(String, Option<Zeroizing<String>>), String> {
    let (raw_path, passphrase) = match credential.find('\n') {
        Some(idx) => {
            let path = &credential[..idx];
            let pass = &credential[idx + 1..];
            (
                path.to_string(),
                if pass.is_empty() {
                    None
                } else {
                    Some(Zeroizing::new(pass.to_string()))
                },
            )
        }
        None => (credential.to_string(), None),
    };

    let expanded = expand_tilde(&raw_path);
    let key_path = validate_key_path(&expanded)?;
    Ok((key_path, passphrase))
}

/// Expand a leading `~` or `~/` to the user's home directory.
fn expand_tilde(path: &str) -> String {
    if path == "~" || path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home
                .join(path[1..].trim_start_matches('/'))
                .to_string_lossy()
                .to_string();
        }
    }
    path.to_string()
}

/// Validate that a key file path resolves to a location within the user's home
/// directory and is not a known sensitive file. Prevents path traversal attacks.
fn validate_key_path(path: &str) -> Result<String, String> {
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;

    // Reject symlinks before canonicalization to prevent symlink-based
    // attacks that could redirect reads to arbitrary files.
    let metadata =
        std::fs::symlink_metadata(path).map_err(|e| format!("Key file not accessible: {e}"))?;
    if metadata.file_type().is_symlink() {
        return Err("Symbolic links are not allowed for key files".to_string());
    }

    let canonical =
        std::fs::canonicalize(path).map_err(|e| format!("Key file not accessible: {e}"))?;
    // Canonicalize the home directory too so both paths use the same prefix.
    // On Windows, `canonicalize` returns UNC paths (`\\?\C:\...`) while
    // `dirs::home_dir` returns regular paths (`C:\Users\...`), which makes
    // `starts_with` fail even for paths that are genuinely under $HOME.
    let canonical_home =
        std::fs::canonicalize(&home).map_err(|e| format!("Cannot resolve home directory: {e}"))?;
    if !canonical.starts_with(&canonical_home) {
        return Err("Key path must be within your home directory".to_string());
    }

    // Block known sensitive files that are not SSH keys.
    let rel = canonical
        .strip_prefix(&canonical_home)
        .unwrap_or(&canonical)
        .to_string_lossy();
    let blocked = [
        ".bash_history",
        ".zsh_history",
        ".bashrc",
        ".profile",
        ".zshrc",
        ".netrc",
        ".gnupg/",
        ".config/shellstation/config.json",
    ];
    for pattern in &blocked {
        if rel.starts_with(pattern) {
            return Err(format!("Path '{path}' is not an SSH key file"));
        }
    }

    Ok(canonical.to_string_lossy().to_string())
}

/// Return the path to the user's `known_hosts` file.
fn known_hosts_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir()
        .ok_or_else(|| "Could not determine home directory for known_hosts".to_string())?;
    Ok(home.join(".ssh").join("known_hosts"))
}

/// Serializes changes to `known_hosts` within the app. Two connections whose
/// keys are accepted at the same moment would otherwise both read the file
/// before either has written it, and the second write would drop the first
/// key.
static KNOWN_HOSTS_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Record `key` as the one key trusted for `host`:`port` in the `known_hosts`
/// file at `path`, replacing every entry for that host (see
/// [`replace_known_host_entries`]). The `.ssh` directory is created (0700)
/// if missing. The file is replaced atomically and owner-only, so an
/// interrupted write leaves the old one whole. A symlink at `path`, say into
/// a dotfiles repository, is followed: the file it points to is replaced and
/// the link kept. A link that points nowhere, or anything but a regular file
/// (`/dev/null`, which switches `known_hosts` off, a FIFO, a directory), is
/// an error, and the key is not saved. Blocking: file I/O and, on Windows,
/// `icacls`.
fn save_known_host(
    path: &std::path::Path,
    host: &str,
    port: u16,
    key: &russh::keys::PublicKey,
) -> Result<(), String> {
    let name = known_hosts_name(host, port)?;
    let key = key
        .to_openssh()
        .map_err(|e| format!("Cannot encode the host key: {e}"))?;
    if let Some(dir) = path.parent() {
        crate::private_fs::create_private_dir(dir)
            .map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        // Always enforce correct permissions on the .ssh directory.
        set_restricted_dir_permissions(dir);
    }
    let path = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path)
            .map_err(|e| format!("Cannot resolve {}: {e}", path.display()))?,
        _ => path.to_path_buf(),
    };
    match std::fs::metadata(&path) {
        Ok(meta) if !meta.is_file() => {
            return Err(format!("{} is not a regular file", path.display()));
        }
        _ => {}
    }
    let _guard = KNOWN_HOSTS_WRITE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let old = match read_known_hosts(&path) {
        Ok(Some(old)) => old,
        Ok(None) => return Err(format!("{} is larger than the limit", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(format!("Cannot read {}: {e}", path.display())),
    };
    let new = replace_known_host_entries(&old, &name, &key);
    crate::private_fs::write_private_file(&path, &new)
        .map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
    set_restricted_file_permissions(&path);
    Ok(())
}

/// The name `host`:`port` goes by in `known_hosts`, as OpenSSH writes and
/// looks it up: in lower case, an IPv6 address without the brackets it may
/// have been given in, bare for port 22 and `[host]:port` for any other.
fn known_hosts_name(host: &str, port: u16) -> Result<String, String> {
    validate_host_name(host)?;
    // validate_host_name allows brackets only around an IPv6 address.
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
        .to_ascii_lowercase();
    Ok(if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    })
}

/// The contents of the `known_hosts` file at `path`, or `None` when it is
/// larger than [`MAX_KNOWN_HOSTS_SIZE`]. Read through one handle with a
/// bound, so that a file growing after a size check is not read whole.
fn read_known_hosts(path: &std::path::Path) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;
    let mut contents = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_KNOWN_HOSTS_SIZE + 1)
        .read_to_end(&mut contents)?;
    let within = u64::try_from(contents.len()).is_ok_and(|len| len <= MAX_KNOWN_HOSTS_SIZE);
    Ok(within.then_some(contents))
}

/// Refuse a host name holding a character that no host name or address
/// has, but that `known_hosts`, where a trusted host's name is written as
/// is, gives a meaning to: whitespace and control characters (which end a
/// field or a line), `,` (between names), `*`, `?` and `!` (OpenSSH
/// patterns), `[` and `]` (a name with its port), and `#`, `@` and `|` (a
/// comment, a marker, a hashed name). Trusting a key for `x,example.com`
/// would otherwise trust it for `example.com` too.
///
/// Also refused, since the host-key prompt shows the name: the characters
/// OpenSSH's own host name check refuses (quotes, `` ` ``, `$`, `\`, `;`,
/// `&`, `<`, `>` and brackets), and invisible ones (bidi controls,
/// zero-width and filler characters, soft hyphens, variation selectors,
/// tags), with which a name could pass for another.
fn validate_host_name(host: &str) -> Result<(), String> {
    // Windows resolves an IPv6 address in brackets, and in known_hosts
    // `[::1]` (or `[[::1]]:2222`) names no other host.
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .filter(|inner| {
            let addr = inner.split_once('%').map_or(*inner, |(addr, _zone)| addr);
            addr.parse::<Ipv6Addr>().is_ok()
        })
        .unwrap_or(host);
    let invalid = unbracketed.is_empty()
        || unbracketed.chars().any(|c| {
            c.is_whitespace()
                || c.is_control()
                || matches!(c, ',' | '*' | '?' | '!' | '[' | ']' | '#' | '@' | '|')
                || matches!(
                    c,
                    '\'' | '"' | '`' | '$' | '\\' | ';' | '&' | '<' | '>' | '(' | ')' | '{' | '}'
                )
                || matches!(
                    c,
                    '\u{00AD}'
                        | '\u{034F}'
                        | '\u{061C}'
                        | '\u{115F}'..='\u{1160}'
                        | '\u{17B4}'..='\u{17B5}'
                        | '\u{180B}'..='\u{180F}'
                        | '\u{200B}'..='\u{200F}'
                        | '\u{202A}'..='\u{202E}'
                        | '\u{2060}'..='\u{2064}'
                        | '\u{2066}'..='\u{206F}'
                        | '\u{2800}'
                        | '\u{3164}'
                        | '\u{FE00}'..='\u{FE0F}'
                        | '\u{FEFF}'
                        | '\u{FFA0}'
                        | '\u{FFF9}'..='\u{FFFB}'
                        | '\u{E0000}'..='\u{E007F}'
                        | '\u{E0100}'..='\u{E01EF}'
                )
        });
    if invalid {
        // Debug formatting escapes control characters.
        return Err(format!("Invalid host name {host:?}"));
    }
    Ok(())
}

/// The `known_hosts` contents `old` with `name` taken out of every entry (see
/// [`without_name`]) and `name key` added at the end. Plain and hashed names
/// (`|1|…`, as OpenSSH writes them with `HashKnownHosts`) are both found,
/// which covers every entry [`check_known_host`] compares a key with: one
/// left behind would report the newly trusted key as changed on every
/// connection. Other names on a line keep their key, so a host's address
/// that shared it is still reported as changed, not as new. Everything else
/// stays byte for byte, including comments, `@cert-authority` and
/// `@revoked` lines, line endings and lines that are not UTF-8.
fn replace_known_host_entries(old: &[u8], name: &str, key: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(old.len() + name.len() + key.len() + 2);
    for line in old.split_inclusive(|&b| b == b'\n') {
        if let Some(kept) = without_name(line, name) {
            out.extend_from_slice(&kept);
        }
    }
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.extend_from_slice(name.as_bytes());
    out.push(b' ');
    out.extend_from_slice(key.as_bytes());
    out.push(b'\n');
    out
}

/// What OpenSSH reads of a `known_hosts` line: up to a NUL, as a C string
/// ends there, and without the line break (`\n` or `\r\n`); a `\r` anywhere
/// else is part of the text. [`check_known_host`] and [`without_name`] both
/// read lines through this, so that they see the same names.
fn line_content(line: &[u8]) -> &[u8] {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.split(|&b| b == 0).next().unwrap_or_default();
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// A `known_hosts` line without `name` among its names, read as OpenSSH and
/// [`check_known_host`] read them (see [`line_content`]): the line as it is
/// when `name` is not there, and `None` when the line goes. Leading blanks
/// are skipped, a comment or a marker line (`@revoked`, ...) is kept as it
/// is, the names end at a space or a tab, a plain name matches in either
/// case, and a hashed name is one name, commas and all. The line goes when
/// no names are left, or when the new first name would make it read as a
/// hashed entry, a comment or a marker line.
fn without_name<'a>(line: &'a [u8], name: &str) -> Option<std::borrow::Cow<'a, [u8]>> {
    let is_name =
        |entry: &[u8]| entry.eq_ignore_ascii_case(name.as_bytes()) || is_hashed_name(entry, name);
    let content = line_content(line);
    let start = content
        .iter()
        .position(|&b| b != b' ' && b != b'\t')
        .unwrap_or(content.len());
    let lead = &line[..start];
    let body = &content[start..];
    if body.starts_with(b"#") || body.starts_with(b"@") {
        return Some(line.into());
    }
    let end = body
        .iter()
        .position(|&b| b == b' ' || b == b'\t')
        .unwrap_or(body.len());
    let names = &body[..end];
    // The rest of the line, its ending included.
    let rest = &line[start + end..];
    if names.starts_with(b"|1|") {
        return (!is_name(names)).then(|| line.into());
    }
    if !names.split(|&b| b == b',').any(is_name) {
        return Some(line.into());
    }
    // Built in one pass rather than collected entry by entry, so that a
    // line of commas costs no more memory than its own length.
    let (mut kept, mut first, mut all_empty) = (Vec::with_capacity(line.len()), None, true);
    kept.extend_from_slice(lead);
    for entry in names.split(|&b| b == b',').filter(|entry| !is_name(entry)) {
        if first.is_some() {
            kept.push(b',');
        }
        kept.extend_from_slice(entry);
        first.get_or_insert(entry);
        all_empty &= entry.is_empty();
    }
    let read_otherwise = |first: &[u8]| {
        first.starts_with(b"|1|") || first.starts_with(b"#") || first.starts_with(b"@")
    };
    if all_empty || first.is_some_and(read_otherwise) {
        return None;
    }
    kept.extend_from_slice(rest);
    Some(kept.into())
}

/// Base64 for hashed names in entries being replaced, with padding optional
/// and unused bits ignored, so that an entry for the host is not missed
/// over how it was encoded.
const LENIENT_BASE64: base64::engine::GeneralPurpose = base64::engine::GeneralPurpose::new(
    &base64::alphabet::STANDARD,
    base64::engine::GeneralPurposeConfig::new()
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true),
);

/// Whether `entry` is `name` hashed the way OpenSSH hashes `known_hosts`
/// names: `|1|`, a base64 salt, `|`, and the base64 HMAC-SHA1 of the name
/// keyed with that salt. Lenient, for entries being replaced: anything after
/// a further `|` is ignored, and so are line breaks within the base64, so
/// that no entry for the host is left behind however it was written.
fn is_hashed_name(entry: &[u8], name: &str) -> bool {
    let Some(rest) = entry.strip_prefix(b"|1|") else {
        return false;
    };
    let mut parts = rest.split(|&b| b == b'|');
    let decode = |part: Option<&[u8]>| {
        let part: Vec<u8> = part?
            .iter()
            .copied()
            .filter(|b| !matches!(b, b'\r' | b'\n'))
            .collect();
        LENIENT_BASE64.decode(part).ok()
    };
    match (decode(parts.next()), decode(parts.next())) {
        (Some(salt), Some(hash)) => hashes_to(&salt, &hash, name),
        _ => false,
    }
}

/// [`is_hashed_name`] for only the form OpenSSH writes and reads: exactly
/// two parts after `|1|`, in padded base64.
fn is_hashed_name_strict(entry: &[u8], name: &str) -> bool {
    let Some(rest) = entry.strip_prefix(b"|1|") else {
        return false;
    };
    let mut parts = rest.split(|&b| b == b'|');
    let decode = |part: Option<&[u8]>| base64::prelude::BASE64_STANDARD.decode(part?).ok();
    match (decode(parts.next()), decode(parts.next()), parts.next()) {
        (Some(salt), Some(hash), None) => hashes_to(&salt, &hash, name),
        _ => false,
    }
}

/// Whether `hash` is the HMAC-SHA1 of `name` keyed with `salt`.
fn hashes_to(salt: &[u8], hash: &[u8], name: &str) -> bool {
    use hmac::{KeyInit, Mac};
    let Ok(mac) = hmac::Hmac::<sha1::Sha1>::new_from_slice(salt) else {
        return false;
    };
    mac.chain_update(name.as_bytes()).verify_slice(hash).is_ok()
}

/// What [`check_known_host`] found for a host's key.
#[derive(Debug, PartialEq)]
enum KnownHost {
    /// The key is on record for the host.
    Trusted,
    /// The key is listed under `@revoked` for the host.
    Revoked,
    /// The user is to decide, told `status`; `known` are the keys on record
    /// that the prompt shows.
    Ask {
        status: HostKeyStatus,
        known: Vec<russh::keys::PublicKey>,
    },
}

impl KnownHost {
    fn ask(status: HostKeyStatus) -> Self {
        Self::Ask {
            status,
            known: Vec::new(),
        }
    }
}

/// The largest `known_hosts` file read or rewritten, far beyond any real
/// one: a bigger file is not loaded into memory, and its hosts are asked
/// about as unchecked.
const MAX_KNOWN_HOSTS_SIZE: u64 = 64 << 20;

/// The most keys on record the host-key prompt shows, of the offered key's
/// type and of other types each. Any more still count for the status.
const MAX_KNOWN_KEYS_SHOWN: usize = 8;

/// Check `key`, offered by `host`:`port`, against the `known_hosts` file at
/// `path`, reading it as OpenSSH does. A line that cannot be read costs only
/// that line (russh gives up on the whole file), and is logged.
///
/// A key is trusted only by an entry naming the host exactly (a plain name
/// in either case, as OpenSSH compares them, or a hashed one; see
/// [`known_hosts_name`]), with a key type that matches the key;
/// patterns (`*`, `?`) never trust one, but a matching negated pattern (`!`)
/// takes a line out, as in OpenSSH. `@revoked` lines count with their
/// patterns, so a pattern can revoke a key, and one for the host that
/// cannot be read leaves the key unchecked rather than trusted.
/// `@cert-authority` lines are skipped: no host certificate is ever
/// accepted. Blocking: file I/O.
fn check_known_host(
    path: &std::path::Path,
    host: &str,
    port: u16,
    key: &russh::keys::PublicKey,
) -> KnownHost {
    let name = match known_hosts_name(host, port) {
        Ok(name) => name,
        Err(e) => {
            warn!(error = %e, "Host name cannot be looked up in known_hosts");
            return KnownHost::ask(HostKeyStatus::Unchecked);
        }
    };
    let unchecked = |error: &dyn std::fmt::Display| {
        warn!(known_hosts = %path.display(), %error, "Cannot read known_hosts");
        KnownHost::ask(HostKeyStatus::Unchecked)
    };
    let contents = match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => match read_known_hosts(path) {
            Ok(Some(contents)) => contents,
            Ok(None) => return unchecked(&"larger than the limit"),
            Err(e) => return unchecked(&e),
        },
        Ok(meta) if meta.is_dir() => return unchecked(&"a directory"),
        // Linked to /dev/null, which switches known_hosts off, or no file
        // yet: nothing is on record. A FIFO is not read, which would block.
        Ok(_) => Vec::new(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return unchecked(&e),
    };

    // Trust and revocation are settled line by line, and only the keys the
    // prompt shows are kept, so that a file full of keys for the host costs
    // no more than reading it.
    let (mut trusted, mut revoked) = (false, false);
    let (mut same_type, mut other_types) = (Vec::new(), Vec::new());
    let (mut unreadable, mut unreadable_revocation) = (false, false);
    for (index, line) in contents.split(|&b| b == b'\n').enumerate() {
        let mut fields = line_content(line)
            .split(|&b| b == b' ' || b == b'\t')
            .filter(|field| !field.is_empty());
        let Some(first) = fields.next() else {
            continue;
        };
        let (names, is_revocation) = match first {
            _ if first.starts_with(b"#") => continue,
            b"@revoked" => match fields.next() {
                Some(names) => (names, true),
                None => continue,
            },
            // `@cert-authority`, or a marker OpenSSH does not know.
            _ if first.starts_with(b"@") => continue,
            _ => (first, false),
        };
        let applies = if is_revocation {
            pattern_list_matches(names, &name)
        } else {
            names_entry_for(names, &name)
        };
        if !applies {
            continue;
        }
        // A key type, then the key. As in OpenSSH, the two must agree.
        let (key_type, blob) = (fields.next(), fields.next());
        let parsed = blob
            .and_then(|blob| std::str::from_utf8(blob).ok())
            .and_then(|blob| russh::keys::parse_public_key_base64(blob).ok())
            .filter(|recorded| key_type == Some(recorded.algorithm().as_str().as_bytes()));
        match parsed {
            Some(recorded) if is_revocation => revoked |= recorded.key_data() == key.key_data(),
            Some(recorded) if recorded.key_data() == key.key_data() => trusted = true,
            Some(recorded) => {
                let shown = if recorded.algorithm() == key.algorithm() {
                    &mut same_type
                } else {
                    &mut other_types
                };
                // The same key on two lines, say hashed and plain, is shown once.
                let again = shown
                    .iter()
                    .any(|k: &russh::keys::PublicKey| k.key_data() == recorded.key_data());
                if !again && shown.len() < MAX_KNOWN_KEYS_SHOWN {
                    shown.push(recorded);
                }
            }
            None => {
                warn!(
                    known_hosts = %path.display(),
                    line = index + 1,
                    host = %name,
                    "known_hosts entry for the host cannot be read — skipped"
                );
                if is_revocation {
                    unreadable_revocation = true;
                } else {
                    unreadable = true;
                }
            }
        }
    }

    if revoked {
        return KnownHost::Revoked;
    }
    // A revocation that cannot be read might be of this very key.
    let unreadable = unreadable || unreadable_revocation;
    if trusted {
        return if unreadable_revocation {
            KnownHost::ask(HostKeyStatus::Unchecked)
        } else {
            KnownHost::Trusted
        };
    }
    if !same_type.is_empty() {
        KnownHost::Ask {
            status: HostKeyStatus::Changed,
            known: same_type,
        }
    } else if !other_types.is_empty() {
        KnownHost::Ask {
            status: HostKeyStatus::OtherType,
            known: other_types,
        }
    } else if unreadable {
        KnownHost::ask(HostKeyStatus::Unchecked)
    } else {
        KnownHost::ask(HostKeyStatus::New)
    }
}

/// Whether a `known_hosts` entry's names (its first field) name `name`, to
/// trust its key: exactly, or hashed in the form OpenSSH writes, and with
/// no negated pattern on the line matching it. A hashed name is one name,
/// commas and all.
fn names_entry_for(names: &[u8], name: &str) -> bool {
    if names.starts_with(b"|1|") {
        return is_hashed_name_strict(names, name);
    }
    let excluded = names.split(|&b| b == b',').any(|entry| {
        entry
            .strip_prefix(b"!")
            .is_some_and(|pattern| pattern_matches(pattern, name.as_bytes()))
    });
    !excluded
        && names
            .split(|&b| b == b',')
            .any(|entry| entry.eq_ignore_ascii_case(name.as_bytes()))
}

/// Whether OpenSSH would apply a line with these names to `name`, patterns
/// included: a negated pattern that matches rules the line out, otherwise
/// any matching pattern or hashed name applies it. Used for `@revoked`
/// lines, where matching more only refuses more.
fn pattern_list_matches(names: &[u8], name: &str) -> bool {
    if names.starts_with(b"|1|") {
        return is_hashed_name(names, name);
    }
    let mut applies = false;
    for entry in names.split(|&b| b == b',') {
        if let Some(pattern) = entry.strip_prefix(b"!") {
            if pattern_matches(pattern, name.as_bytes()) {
                return false;
            }
        } else if pattern_matches(entry, name.as_bytes()) || is_hashed_name(entry, name) {
            applies = true;
        }
    }
    applies
}

/// Whether `text` matches an OpenSSH pattern: `*` stands for any run of
/// characters, `?` for one, and letters match in either case. Matched with
/// one step back to the last `*`, so a pattern full of `*` cannot take
/// exponential time. Each byte but `*` takes one of `text`, so a pattern
/// with more of them cannot match; refusing it first keeps a long line's
/// pattern from being matched over and over against the name.
fn pattern_matches(pattern: &[u8], text: &[u8]) -> bool {
    if pattern.iter().filter(|&&c| c != b'*').count() > text.len() {
        return false;
    }
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == b'?' || c.eq_ignore_ascii_case(&text[t]) => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

/// Set directory permissions to 0700 (Unix) or restricted ACL (Windows).
fn set_restricted_dir_permissions(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)) {
            warn!(path = %path.display(), error = %e, "Failed to restrict directory permissions");
        }
    }
    #[cfg(windows)]
    {
        restrict_windows_acl(path, true);
    }
}

/// Set file permissions to 0600 (Unix) or restricted ACL (Windows).
fn set_restricted_file_permissions(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            warn!(path = %path.display(), error = %e, "Failed to restrict file permissions");
        }
    }
    #[cfg(windows)]
    {
        restrict_windows_acl(path, false);
    }
}

/// On Windows, use icacls to remove inherited permissions and grant full
/// control only to the current user, as OpenSSH expects; SYSTEM and the
/// administrators lose their inherited access too. The user is named by SID:
/// a bare user name can resolve to another account of the same name (a local
/// one on a domain-joined machine), and with inheritance removed the user
/// would then be locked out of their own `.ssh` directory.
#[cfg(windows)]
fn restrict_windows_acl(path: &std::path::Path, is_dir: bool) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    let Some(sid) = current_user_sid() else {
        warn!(path = %path.display(), "Could not determine the current user's SID; permissions left as they are");
        return;
    };
    let grant = if is_dir {
        format!("*{sid}:(OI)(CI)F")
    } else {
        format!("*{sid}:F")
    };
    match std::process::Command::new(windows_tool("icacls.exe"))
        .arg(path)
        .args(["/inheritance:r", "/grant:r", grant.as_str()])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            warn!(path = %path.display(), status = %out.status, "icacls could not restrict permissions");
        }
        Err(e) => warn!(path = %path.display(), error = %e, "Failed to run icacls"),
    }
}

/// The current user's SID, read once from `whoami /user`.
#[cfg(windows)]
fn current_user_sid() -> Option<&'static str> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    static SID: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    SID.get_or_init(|| {
        let out = std::process::Command::new(windows_tool("whoami.exe"))
            .args(["/user", "/fo", "csv", "/nh"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()
            .filter(|out| out.status.success())?;
        parse_whoami_sid(&String::from_utf8_lossy(&out.stdout)).map(str::to_string)
    })
    .as_deref()
}

/// The SID in `whoami /user /fo csv /nh` output, `"domain\user","S-1-…"`.
/// Only the last field is read, so a comma or quote in the user name cannot
/// shift it, and only a well-formed SID is accepted.
#[cfg(any(windows, test))]
fn parse_whoami_sid(output: &str) -> Option<&str> {
    let sid = output
        .trim()
        .rsplit(',')
        .next()?
        .strip_prefix('"')?
        .strip_suffix('"')?;
    let parts = sid.strip_prefix("S-1-")?;
    parts
        .split('-')
        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        .then_some(sid)
}

/// A program in the Windows system directory, by full path so that `PATH`
/// cannot put another program of the same name first.
#[cfg(windows)]
fn windows_tool(name: &str) -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    std::path::Path::new(&root).join("System32").join(name)
}

/// Build an SSH client config with the given keepalive parameters.
///
/// `TCP_NODELAY` is enabled unconditionally: interactive terminal sessions
/// send small keystroke-sized packets where Nagle's algorithm adds 40+ms
/// of latency for no real bandwidth savings. OpenSSH sets this by default.
///
/// A `keepalive_interval_secs` of 0 disables SSH-level keepalives entirely.
fn ssh_config(
    keepalive_interval_secs: u64,
    keepalive_max: usize,
    legacy_algorithms: bool,
) -> Arc<client::Config> {
    let keepalive_interval = if keepalive_interval_secs == 0 {
        None
    } else {
        Some(std::time::Duration::from_secs(keepalive_interval_secs))
    };
    let mut config = client::Config {
        keepalive_interval,
        keepalive_max,
        nodelay: true,
        ..Default::default()
    };
    config.preferred = if legacy_algorithms {
        legacy_preferred()
    } else {
        base_preferred()
    };
    log_algorithm_proposal(&config.preferred, legacy_algorithms);
    Arc::new(config)
}

/// Log the full algorithm proposal at debug level so SSH negotiation failures
/// can be diagnosed by comparing the client's offer against the server's.
fn log_algorithm_proposal(pref: &russh::Preferred, legacy: bool) {
    fn join_names<'a>(items: impl Iterator<Item = &'a str>) -> String {
        items.collect::<Vec<_>>().join(", ")
    }
    debug!(
        legacy,
        kex = %join_names(pref.kex.iter().map(AsRef::as_ref)),
        key = %join_names(pref.key.iter().map(russh::keys::Algorithm::as_str)),
        cipher = %join_names(pref.cipher.iter().map(AsRef::as_ref)),
        mac = %join_names(pref.mac.iter().map(AsRef::as_ref)),
        compression = %join_names(pref.compression.iter().map(AsRef::as_ref)),
        "SSH algorithm proposal"
    );
}

/// Extend the defaults with secure algorithms that russh implements but
/// excludes from `Preferred::DEFAULT`. Devices like Cisco IOS XE may only
/// offer ecdh-sha2-nistp384 for kex or aes128-gcm for cipher.
///
/// No host certificates are asked for: they are trusted through a certificate
/// authority, and the app has no list of those, so a server is always asked
/// for a plain host key that `known_hosts` or the user can vouch for.
fn base_preferred() -> russh::Preferred {
    use std::borrow::Cow;
    let base = russh::Preferred::DEFAULT;

    let mut kex: Vec<russh::kex::Name> = base.kex.iter().copied().collect();
    for extra in [
        russh::kex::ECDH_SHA2_NISTP256,
        russh::kex::ECDH_SHA2_NISTP384,
        russh::kex::ECDH_SHA2_NISTP521,
    ] {
        if !kex.contains(&extra) {
            kex.push(extra);
        }
    }

    let mut cipher: Vec<russh::cipher::Name> = base.cipher.iter().copied().collect();
    if !cipher.contains(&russh::cipher::AES_128_GCM) {
        cipher.push(russh::cipher::AES_128_GCM);
    }

    russh::Preferred {
        kex: Cow::Owned(kex),
        cipher: Cow::Owned(cipher),
        host_key_certificates: Cow::Borrowed(&[]),
        ..base
    }
}

/// Build a `Preferred` algorithm set that appends legacy kex, ciphers and
/// MACs to the modern defaults. Secure algorithms remain first in the list so
/// a legacy-capable switch won't downgrade a modern server, but old gear that
/// only speaks group14-sha1 / aes-cbc / hmac-sha1 can now be reached.
///
/// SHA-1 MACs (`hmac-sha1`, `hmac-sha1-etm`) are added only on this path —
/// since russh 0.60.2 they are no longer part of `Preferred::DEFAULT.mac`.
fn legacy_preferred() -> russh::Preferred {
    use std::borrow::Cow;
    let base = base_preferred();

    let mut kex: Vec<russh::kex::Name> = base.kex.iter().copied().collect();
    // Remove DH group-exchange algorithms: devices with missing/empty moduli
    // files (e.g. Cisco Small Business) negotiate GEX then hang. DH_GEX_SHA1
    // also fails because old Cisco SSH-1.99 sends RFC 2409 old-format messages
    // while russh uses RFC 4419, causing "early eof".
    kex.retain(|k| *k != russh::kex::DH_GEX_SHA256 && *k != russh::kex::DH_GEX_SHA1);
    for extra in [russh::kex::DH_G14_SHA1, russh::kex::DH_G1_SHA1] {
        if !kex.contains(&extra) {
            kex.push(extra);
        }
    }

    let mut cipher: Vec<russh::cipher::Name> = base.cipher.iter().copied().collect();
    for extra in [
        russh::cipher::AES_256_CBC,
        russh::cipher::AES_192_CBC,
        russh::cipher::AES_128_CBC,
        russh::cipher::TRIPLE_DES_CBC,
    ] {
        if !cipher.contains(&extra) {
            cipher.push(extra);
        }
    }

    let mut mac: Vec<russh::mac::Name> = base.mac.iter().copied().collect();
    for extra in [russh::mac::HMAC_SHA1_ETM, russh::mac::HMAC_SHA1] {
        if !mac.contains(&extra) {
            mac.push(extra);
        }
    }

    russh::Preferred {
        kex: Cow::Owned(kex),
        key: base.key.clone(),
        cipher: Cow::Owned(cipher),
        mac: Cow::Owned(mac),
        compression: base.compression.clone(),
        host_key_certificates: base.host_key_certificates.clone(),
    }
}

/// Check whether an IP address belongs to a restricted (non-routable) range:
/// one that reaches this machine or its local network rather than the
/// internet, or that is no single host on the internet at all (multicast,
/// reserved and special-purpose ranges).
fn is_restricted_ip(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V4(ip) => is_restricted_ipv4(*ip),
        IpAddr::V6(ip) => is_restricted_ipv6(*ip),
    }
}

/// Besides the local ranges, 198.18.0.0/15 is where a local proxy in
/// "fake-IP" mode answers names it then forwards itself, so a name resolving
/// there can reach a service on this machine.
fn is_restricted_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_loopback() // 127.0.0.0/8
        || ip.is_private() // 10/8, 172.16/12, 192.168/16
        || ip.is_link_local() // 169.254.0.0/16
        || ip.is_multicast() // 224.0.0.0/4
        || a >= 240 // 240.0.0.0/4, reserved, with 255.255.255.255 broadcast
        || a == 0 // 0.0.0.0/8, "this network"
        || (a == 100 && (64..=127).contains(&b)) // 100.64.0.0/10 (CGNAT)
        || (a == 192 && b == 0 && c == 0) // 192.0.0.0/24, IETF protocol assignments
        || (a == 198 && (b == 18 || b == 19)) // 198.18.0.0/15, benchmarking
}

/// Besides its own ranges, an IPv6 address is restricted when it stands for
/// a restricted IPv4 address, or for one that only the local network can
/// tell (the local-use NAT64 prefix). A NAT64 network-specific prefix looks
/// like any other address and cannot be recognised here.
fn is_restricted_ipv6(ip: Ipv6Addr) -> bool {
    let first = ip.segments()[0];
    ip.is_loopback() // ::1
        || ip.is_unspecified() // ::
        || ip.is_multicast() // ff00::/8
        || first & 0xffc0 == 0xfe80 // fe80::/10, link-local
        || first & 0xffc0 == 0xfec0 // fec0::/10, site-local (deprecated)
        || first & 0xfe00 == 0xfc00 // fc00::/7, unique local
        || matches!(ip.segments(), [0x64, 0xff9b, 1, ..]) // 64:ff9b:1::/48, local-use NAT64
        || embedded_ipv4(ip).into_iter().flatten().any(is_restricted_ipv4)
}

/// The IPv4 addresses an IPv6 address carries, and which a connection to it
/// may reach: the system connects to an IPv4-mapped, IPv4-translated or
/// IPv4-compatible address over IPv4, a NAT64 gateway forwards to the
/// address under its well-known prefix, and 6to4 and Teredo send IPv6
/// inside IPv4 packets to the address they carry. Teredo carries two: its
/// server's, and its client's with every bit inverted.
fn embedded_ipv4(ip: Ipv6Addr) -> [Option<Ipv4Addr>; 2] {
    let v4 = |hi: u16, lo: u16| Some(Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo)));
    match ip.segments() {
        [0, 0, 0, 0, 0, 0xffff | 0, hi, lo] // ::ffff:0:0/96 mapped, ::/96 compatible
        | [0, 0, 0, 0, 0xffff, 0, hi, lo] // ::ffff:0:0:0/96 translated
        | [0x64, 0xff9b, 0, 0, 0, 0, hi, lo] // 64:ff9b::/96, NAT64
        | [0x2002, hi, lo, ..] => [v4(hi, lo), None], // 2002::/16, 6to4
        // 2001::/32, Teredo
        [0x2001, 0, server_hi, server_lo, _, _, client_hi, client_lo] => {
            [v4(server_hi, server_lo), v4(!client_hi, !client_lo)]
        }
        _ => [None, None],
    }
}

/// Resolve a hostname and refuse it when any address it resolves to is in a
/// restricted range. This prevents SSRF attacks through tunnels targeting
/// internal services.
///
/// The caller must connect to the addresses returned, not to the name: a
/// second lookup could get a different answer (DNS rebinding) and reach an
/// address that was never checked. Also used by Telnet.
pub async fn resolve_unrestricted(host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    // A literal IP needs no lookup.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_restricted_ip(&ip) {
            return Err(format!(
                "Connection to restricted address {host} is not allowed"
            ));
        }
        return Ok(vec![SocketAddr::new(ip, port)]);
    }

    // Resolve hostname and check all resulting addresses. The resolver's
    // error and the restricted address go to the log only: in the message,
    // they would let a compromised webview read DNS answers.
    let addrs = lookup(host, port).await.map_err(|e| {
        warn!(host, error = %e, "Cannot resolve host");
        format!("Cannot resolve host '{host}'")
    })?;

    if addrs.is_empty() {
        return Err(format!("Host '{host}' did not resolve to any address"));
    }

    if let Some(addr) = addrs.iter().find(|addr| is_restricted_ip(&addr.ip())) {
        warn!(host, address = %addr.ip(), "Host resolves to a restricted address");
        return Err(format!(
            "Host '{host}' resolves to a restricted address — connection not allowed"
        ));
    }

    Ok(addrs)
}

/// The most lookups [`lookup`] runs at once.
const MAX_CONCURRENT_LOOKUPS: usize = 16;
static LOOKUPS: Semaphore = Semaphore::const_new(MAX_CONCURRENT_LOOKUPS);

/// Resolve `host` on Tokio's blocking thread pool. A lookup keeps its
/// thread until the resolver answers, even once the connection it was for
/// has timed out, so lookups hold a [`LOOKUPS`] permit until then: names
/// that never answer cannot tie up the threads the rest of the app shares.
async fn lookup(host: &str, port: u16) -> std::io::Result<Vec<SocketAddr>> {
    let host = host.to_string();
    run_lookup(&LOOKUPS, move || {
        (host.as_str(), port)
            .to_socket_addrs()
            .map(Iterator::collect)
    })
    .await
}

/// Run `resolve` on Tokio's blocking thread pool with a permit from
/// `permits`, held until `resolve` returns.
async fn run_lookup(
    permits: &'static Semaphore,
    resolve: impl FnOnce() -> std::io::Result<Vec<SocketAddr>> + Send + 'static,
) -> std::io::Result<Vec<SocketAddr>> {
    let permit = permits.acquire().await.map_err(std::io::Error::other)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        resolve()
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e)))
}

/// The addresses the next host in a chain was checked at, or `None` to
/// connect by name when nothing is checked. While checking, running out is
/// an error, never a fallback to the name.
fn next_checked(
    checked: &mut Option<VecDeque<Vec<SocketAddr>>>,
) -> Result<Option<Vec<SocketAddr>>, String> {
    match checked {
        None => Ok(None),
        Some(queue) => queue.pop_front().map(Some).ok_or_else(|| {
            "A host in the chain was not checked against restricted addresses".to_string()
        }),
    }
}

/// Connect to `host`: to the addresses it was checked at when given, so
/// nothing resolves it again, and by name otherwise.
async fn connect_checked<H: client::Handler + Send + 'static>(
    config: Arc<client::Config>,
    host: &str,
    port: u16,
    checked: Option<Vec<SocketAddr>>,
    handler: H,
) -> Result<client::Handle<H>, H::Error> {
    match checked {
        Some(addrs) => client::connect(config, &addrs[..], handler).await,
        None => client::connect(config, (host, port), handler).await,
    }
}

/// The most addresses a bastion is asked to open one tunnel to, so a name
/// with a long list of addresses cannot make it try them all.
const MAX_TUNNEL_ATTEMPTS: usize = 8;

/// Open a direct-tcpip channel from `via` to `host`. Given the addresses
/// `host` was checked at, the channel goes to the first of them the bastion
/// can reach, so the bastion has no name left to resolve. Without, the
/// bastion resolves `host` itself, which also reaches names only it knows.
///
/// All attempts share `timeout`. As when the bastion tries a name's
/// addresses itself, one it cannot get an answer from can use it all up
/// before the next is tried.
async fn open_tunnel(
    via: &client::Handle<impl client::Handler>,
    host: &str,
    port: u16,
    checked: Option<Vec<SocketAddr>>,
    timeout: std::time::Duration,
) -> Result<russh::Channel<client::Msg>, String> {
    let destinations = match checked {
        Some(addrs) => addrs
            .iter()
            .take(MAX_TUNNEL_ATTEMPTS)
            .map(|addr| addr.ip().to_string())
            .collect(),
        None => vec![host.to_string()],
    };
    let deadline = tokio::time::Instant::now() + timeout;
    let mut failure = None;
    for destination in destinations {
        match tokio::time::timeout_at(
            deadline,
            via.channel_open_direct_tcpip(destination.as_str(), port.into(), "127.0.0.1", 0),
        )
        .await
        {
            Ok(Ok(channel)) => return Ok(channel),
            Ok(Err(e)) => {
                // Debug, so text from the bastion cannot break the log line.
                warn!(host, destination, error = ?e, "Tunnel not opened");
                failure = Some(e);
            }
            Err(_) => return Err(format!("Tunnel to {host}:{port} timed out")),
        }
    }
    let reason = failure.map_or_else(|| "no address to try".to_string(), |e| tunnel_failure(&e));
    Err(format!("Failed to open tunnel to {host}:{port}: {reason}"))
}

/// Why a tunnel was not opened, in words of our own: a refusal can carry a
/// reason text from the bastion, which goes to the log only.
fn tunnel_failure(error: &russh::Error) -> String {
    use russh::ChannelOpenFailure as Refused;
    match error {
        russh::Error::ChannelOpenFailure(reason) => match reason {
            Refused::AdministrativelyProhibited => "the jump host does not allow it",
            Refused::ConnectFailed => "the jump host could not connect",
            Refused::UnknownChannelType => "the jump host does not support tunnels",
            Refused::ResourceShortage => "the jump host is out of resources",
            Refused::Other { .. } => "the jump host refused it",
        }
        .to_string(),
        other => sanitize_ssh_error(&other.to_string()),
    }
}

/// Establish an SSH connection, optionally through a chain of jump hosts.
///
/// Returns the chain with the target last; the jump hosts before it must be
/// kept alive for the tunnel to remain open.
#[allow(clippy::too_many_arguments)]
async fn connect_with_hops(
    session_id: &str,
    target_host: &str,
    target_port: u16,
    jump_hops: &[JumpHop],
    restrict_private_ips: bool,
    connect_timeout_secs: u64,
    keepalive_interval_secs: u64,
    keepalive_max: usize,
    legacy_algorithms: bool,
    app_handle: &AppHandle,
    verify_senders: &HostVerifySenders,
    kbd_interactive_senders: &KbdInteractiveSenders,
) -> Result<PendingChain, String> {
    // A trusted host's name is written into known_hosts as is, so a name
    // that file would read differently is refused before connecting.
    for host in jump_hops
        .iter()
        .map(|hop| hop.host.as_str())
        .chain([target_host])
    {
        validate_host_name(host)?;
    }

    let connect_timeout = std::time::Duration::from_secs(connect_timeout_secs);
    // The lookups of every host in the chain and the first connection share
    // one timeout, as the lookup and the connection do when a name is
    // connected to. Later hops get a timeout of their own.
    let first_deadline = tokio::time::Instant::now() + connect_timeout;

    // Check every host against restricted IP ranges (SSRF prevention) before
    // connecting to any. Each connection then goes to the addresses checked,
    // in chain order: the jump hosts first, the target last.
    let mut checked = None;
    if restrict_private_ips {
        let mut queue = VecDeque::new();
        let hosts = jump_hops.iter().map(|hop| (hop.host.as_str(), hop.port));
        for (host, port) in hosts.chain([(target_host, target_port)]) {
            let addrs = tokio::time::timeout_at(first_deadline, resolve_unrestricted(host, port))
                .await
                .map_err(|_| format!("Cannot resolve host '{host}': timed out"))??;
            queue.push_back(addrs);
        }
        checked = Some(queue);
    }

    if jump_hops.is_empty() {
        // Direct connection — no jump hosts.
        debug!(
            session_id,
            host = target_host,
            port = target_port,
            "Direct connection (no jump hosts)"
        );
        let target_checked = next_checked(&mut checked)?;
        let config = ssh_config(keepalive_interval_secs, keepalive_max, legacy_algorithms);
        let (verify_tx, verify_rx) = oneshot::channel::<bool>();
        verify_senders
            .lock()
            .map_err(|e| format!("Verify sender lock poisoned: {e}"))?
            .insert(session_id.to_string(), verify_tx);

        let handler = SshHandler {
            session_id: session_id.to_string(),
            host: target_host.to_string(),
            port: target_port,
            app_handle: app_handle.clone(),
            verify_rx: Arc::new(Mutex::new(Some(verify_rx))),
        };

        trace!(
            session_id,
            host = target_host,
            port = target_port,
            timeout_secs = connect_timeout_secs,
            "TCP connect attempt"
        );
        let handle = tokio::time::timeout_at(
            first_deadline,
            connect_checked(config, target_host, target_port, target_checked, handler),
        )
        .await
        .map_err(|_| {
            cleanup_verify_sender(verify_senders, session_id);
            format!("SSH connection failed: connection to {target_host}:{target_port} timed out")
        })?
        .map_err(|e| {
            cleanup_verify_sender(verify_senders, session_id);
            error!(
                host = %target_host,
                port = %target_port,
                error = %e,
                error_debug = ?e,
                "SSH connection/handshake failed"
            );
            sanitize_ssh_error(&e.to_string())
        })?;

        debug!(
            session_id,
            host = target_host,
            port = target_port,
            "TCP connected, handshake complete"
        );
        let mut chain = PendingChain::default();
        chain.push(handle);
        return Ok(chain);
    }

    // Jump host chain: connect to each hop in order, then tunnel to target.
    debug!(
        session_id,
        hops = jump_hops.len(),
        "Starting jump host chain"
    );
    // Every hop is added as soon as it connects, so a failure further along
    // disconnects the hops already up.
    let mut chain = PendingChain::default();

    // Connect to the first hop directly.
    let first_hop = &jump_hops[0];
    let first_checked = next_checked(&mut checked)?;
    let config = ssh_config(keepalive_interval_secs, keepalive_max, legacy_algorithms);
    let hop_id = format!("{session_id}-hop0");
    let (verify_tx, verify_rx) = oneshot::channel::<bool>();
    verify_senders
        .lock()
        .map_err(|e| format!("Verify sender lock poisoned: {e}"))?
        .insert(hop_id.clone(), verify_tx);

    let handler = SshHandler {
        session_id: hop_id.clone(),
        host: first_hop.host.clone(),
        port: first_hop.port,
        app_handle: app_handle.clone(),
        verify_rx: Arc::new(Mutex::new(Some(verify_rx))),
    };

    trace!(session_id, hop = 0, host = %first_hop.host, port = first_hop.port, timeout_secs = connect_timeout_secs, "TCP connect attempt to first hop");
    let first_handle = tokio::time::timeout_at(
        first_deadline,
        connect_checked(
            config,
            &first_hop.host,
            first_hop.port,
            first_checked,
            handler,
        ),
    )
    .await
    .map_err(|_| {
        cleanup_verify_sender(verify_senders, &hop_id);
        format!(
            "SSH jump host connection failed: connection to {}:{} timed out",
            first_hop.host, first_hop.port
        )
    })?
    .map_err(|e| {
        cleanup_verify_sender(verify_senders, &hop_id);
        error!(host = %first_hop.host, port = %first_hop.port, error = %e, error_debug = ?e, "SSH first hop handshake failed");
        format!(
            "SSH jump host connection failed: {}",
            sanitize_ssh_error(&e.to_string())
        )
    })?;
    debug!(session_id, hop = 0, host = %first_hop.host, "First hop connected");

    // Authenticate the first hop.
    {
        let hop_id = format!("{session_id}-hop0");
        authenticate_handle(
            chain.push(first_handle),
            &first_hop.username,
            &first_hop.auth_method,
            &first_hop.auth_credential,
            &hop_id,
            app_handle,
            kbd_interactive_senders,
        )
        .await?;
    }

    info!(hop = 0, host = %first_hop.host, "Jump host authenticated");

    // Chain through remaining hops (if any).
    for (i, hop) in jump_hops.iter().enumerate().skip(1) {
        debug!(session_id, hop = i, host = %hop.host, port = hop.port, "Opening tunnel to next hop");
        let tunnel = open_tunnel(
            chain.last(),
            &hop.host,
            hop.port,
            next_checked(&mut checked)?,
            connect_timeout,
        )
        .await?;

        let stream = tunnel.into_stream();
        let config = ssh_config(keepalive_interval_secs, keepalive_max, legacy_algorithms);
        let hop_id = format!("{session_id}-hop{i}");
        let (verify_tx, verify_rx) = oneshot::channel::<bool>();
        verify_senders
            .lock()
            .map_err(|e| format!("Verify sender lock poisoned: {e}"))?
            .insert(hop_id.clone(), verify_tx);

        let handler = SshHandler {
            session_id: hop_id.clone(),
            host: hop.host.clone(),
            port: hop.port,
            app_handle: app_handle.clone(),
            verify_rx: Arc::new(Mutex::new(Some(verify_rx))),
        };

        let hop_handle = tokio::time::timeout(
            connect_timeout,
            client::connect_stream(config, stream, handler),
        )
        .await
        .map_err(|_| {
            cleanup_verify_sender(verify_senders, &hop_id);
            format!(
                "SSH handshake with {}:{} through tunnel timed out",
                hop.host, hop.port
            )
        })?
        .map_err(|e| {
            cleanup_verify_sender(verify_senders, &hop_id);
            error!(host = %hop.host, port = %hop.port, hop = i, error = %e, error_debug = ?e, "SSH hop handshake failed");
            format!(
                "SSH jump host connection failed: {}",
                sanitize_ssh_error(&e.to_string())
            )
        })?;

        authenticate_handle(
            chain.push(hop_handle),
            &hop.username,
            &hop.auth_method,
            &hop.auth_credential,
            &hop_id,
            app_handle,
            kbd_interactive_senders,
        )
        .await?;

        info!(hop = i, host = %hop.host, "Jump host authenticated");
    }

    // Finally, tunnel from the last hop to the target.
    debug!(
        session_id,
        host = target_host,
        port = target_port,
        "Opening tunnel to final target"
    );

    let tunnel = open_tunnel(
        chain.last(),
        target_host,
        target_port,
        next_checked(&mut checked)?,
        connect_timeout,
    )
    .await?;

    let stream = tunnel.into_stream();
    let config = ssh_config(keepalive_interval_secs, keepalive_max, legacy_algorithms);
    let (verify_tx, verify_rx) = oneshot::channel::<bool>();
    verify_senders
        .lock()
        .map_err(|e| format!("Verify sender lock poisoned: {e}"))?
        .insert(session_id.to_string(), verify_tx);

    let handler = SshHandler {
        session_id: session_id.to_string(),
        host: target_host.to_string(),
        port: target_port,
        app_handle: app_handle.clone(),
        verify_rx: Arc::new(Mutex::new(Some(verify_rx))),
    };

    let target_handle = tokio::time::timeout(
        connect_timeout,
        client::connect_stream(config, stream, handler),
    )
    .await
    .map_err(|_| {
        cleanup_verify_sender(verify_senders, session_id);
        format!("SSH handshake with {target_host}:{target_port} through tunnel timed out")
    })?
    .map_err(|e| {
        cleanup_verify_sender(verify_senders, session_id);
        error!(host = %target_host, port = %target_port, error = %e, error_debug = ?e, "SSH target handshake through tunnel failed");
        sanitize_ssh_error(&e.to_string())
    })?;

    debug!(
        session_id,
        host = target_host,
        port = target_port,
        "Target connected through tunnel"
    );
    chain.push(target_handle);
    Ok(chain)
}

/// Authenticate an SSH handle using the specified method.
#[allow(clippy::too_many_arguments)]
async fn authenticate_handle(
    handle: &mut client::Handle<SshHandler>,
    username: &str,
    auth_method: &str,
    auth_credential: &str,
    session_id: &str,
    app_handle: &AppHandle,
    kbd_interactive_senders: &KbdInteractiveSenders,
) -> Result<(), String> {
    debug!(session_id = %session_id, auth_method = %auth_method, "Authenticating SSH session");

    // RFC 4252 §5.2: try "none" first. Some devices (e.g. Cisco SG350, Catalyst 1300)
    // grant access via "none" and reject all other methods.
    match handle.authenticate_none(username).await {
        Ok(res) if res.success() => {
            warn!(session_id = %session_id, "Authenticated via 'none' method — server did not require credentials");
            return Ok(());
        }
        Ok(_) => {
            debug!(session_id = %session_id, auth_method = %auth_method, "Server rejected 'none' auth, proceeding with configured method");
        }
        Err(e) => {
            debug!(session_id = %session_id, auth_method = %auth_method, error = %e, "Error during 'none' auth probe, proceeding with configured method");
        }
    }

    let auth_ok = match auth_method {
        "password" => {
            debug!(session_id = %session_id, user = %username, "Attempting password authentication");
            let pw_ok = handle
                .authenticate_password(username, auth_credential)
                .await
                .map(|res| {
                    debug!(session_id = %session_id, success = res.success(), "Password auth result");
                    res.success()
                })
                .map_err(|e| {
                    debug!(session_id = %session_id, error = %e, error_debug = ?e, "Password auth error");
                    format!("Auth failed: {e}")
                })?;
            if !pw_ok {
                debug!(session_id = %session_id, "Password auth rejected, falling back to keyboard-interactive");
                return authenticate_keyboard_interactive_with_password(
                    handle,
                    username,
                    auth_credential,
                    session_id,
                )
                .await;
            }
            true
        }
        "publickey" => {
            let (key_path, passphrase) = parse_key_credential(auth_credential)?;
            debug!(session_id = %session_id, user = %username, key = %key_path, has_passphrase = passphrase.is_some(), "Attempting public key authentication");
            let key_pair =
                russh::keys::load_secret_key(&key_path, passphrase.as_ref().map(|s| s.as_str()))
                    .map_err(|e| format!("Failed to load SSH key: {e}"))?;
            let hash_alg = handle
                .best_supported_rsa_hash()
                .await
                .ok()
                .flatten()
                .flatten();
            debug!(session_id = %session_id, rsa_hash_alg = ?hash_alg, "RSA hash algorithm negotiated for public key auth");
            let key_with_hash = PrivateKeyWithHashAlg::new(Arc::new(key_pair), hash_alg);
            handle
                .authenticate_publickey(username, key_with_hash)
                .await
                .map(|res| {
                    debug!(session_id = %session_id, success = res.success(), "Public key auth result");
                    res.success()
                })
                .map_err(|e| {
                    debug!(session_id = %session_id, error = %e, error_debug = ?e, "Public key auth error");
                    format!("Auth failed: {e}")
                })?
        }
        "keyboard-interactive" => {
            return authenticate_keyboard_interactive(
                handle,
                username,
                session_id,
                app_handle,
                kbd_interactive_senders,
            )
            .await;
        }
        other => return Err(format!("Unsupported auth method: {other}")),
    };

    if !auth_ok {
        return Err("Authentication rejected by server".to_string());
    }

    Ok(())
}

/// Keyboard-interactive authentication loop.
///
/// Emits `ssh-kbd-interactive` events with prompt details to the frontend,
/// awaits user responses via oneshot channels, and relays them back to the
/// server. Supports multiple prompt rounds.
async fn authenticate_keyboard_interactive(
    handle: &mut client::Handle<SshHandler>,
    username: &str,
    session_id: &str,
    app_handle: &AppHandle,
    kbd_interactive_senders: &KbdInteractiveSenders,
) -> Result<(), String> {
    debug!(session_id = %session_id, "Starting keyboard-interactive authentication");

    let mut response = handle
        .authenticate_keyboard_interactive_start(username, None::<String>)
        .await
        .map_err(|e| {
            debug!(session_id = %session_id, error = %e, "keyboard-interactive start failed");
            format!("Keyboard-interactive auth failed: {e}")
        })?;

    debug!(session_id = %session_id, response = ?std::mem::discriminant(&response), "keyboard-interactive initial response");

    loop {
        match response {
            KeyboardInteractiveAuthResponse::Success => {
                debug!(session_id = %session_id, "Keyboard-interactive authentication succeeded");
                return Ok(());
            }
            KeyboardInteractiveAuthResponse::Failure { .. } => {
                debug!(session_id = %session_id, "Server rejected keyboard-interactive auth (Failure response)");
                return Err("Server rejected keyboard-interactive authentication. \
                     The server may not support this method — assign a \
                     credential profile with password or key authentication instead."
                    .to_string());
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                debug!(
                    session_id = %session_id,
                    prompt_count = prompts.len(),
                    "Keyboard-interactive prompt round received"
                );

                let payload = KbdInteractivePayload {
                    session_id: session_id.to_string(),
                    name: name.clone(),
                    instructions: instructions.clone(),
                    prompts: prompts
                        .iter()
                        .map(|p| KbdInteractivePrompt {
                            prompt: p.prompt.clone(),
                            echo: p.echo,
                        })
                        .collect(),
                };

                // Create a oneshot channel for the frontend response.
                let (tx, rx) = oneshot::channel::<Zeroizing<Vec<String>>>();
                kbd_interactive_senders
                    .lock()
                    .map_err(|e| format!("Kbd-interactive sender lock poisoned: {e}"))?
                    .insert(session_id.to_string(), tx);

                // Emit event to frontend.
                app_handle
                    .emit("ssh-kbd-interactive", &payload)
                    .map_err(|e| {
                        cleanup_kbd_interactive_sender(kbd_interactive_senders, session_id);
                        format!("Failed to emit kbd-interactive event: {e}")
                    })?;

                // Await user input with 60-second timeout.
                let responses = tokio::time::timeout(std::time::Duration::from_mins(1), rx)
                    .await
                    .map_err(|_| {
                        cleanup_kbd_interactive_sender(kbd_interactive_senders, session_id);
                        "Keyboard-interactive authentication timed out (60s)".to_string()
                    })?
                    .map_err(|_| {
                        cleanup_kbd_interactive_sender(kbd_interactive_senders, session_id);
                        "Keyboard-interactive prompt cancelled".to_string()
                    })?;

                // Empty responses = user cancelled.
                if responses.is_empty() && !prompts.is_empty() {
                    return Err("Keyboard-interactive authentication cancelled".to_string());
                }

                debug!(
                    session_id = %session_id,
                    response_count = responses.len(),
                    "Sending keyboard-interactive responses"
                );

                let mut responses = responses;
                response = handle
                    .authenticate_keyboard_interactive_respond(std::mem::take(&mut *responses))
                    .await
                    .map_err(|e| format!("Keyboard-interactive auth failed: {e}"))?;
            }
        }
    }
}

/// Keyboard-interactive fallback that auto-responds with a stored password.
///
/// Some devices (e.g. Cisco Small Business) reject `userauth_password` and only
/// accept `keyboard-interactive`. When password auth fails, this function
/// retries via keyboard-interactive and answers every prompt with the password.
async fn authenticate_keyboard_interactive_with_password(
    handle: &mut client::Handle<SshHandler>,
    username: &str,
    password: &str,
    session_id: &str,
) -> Result<(), String> {
    const MAX_ROUNDS: u8 = 3;
    debug!(session_id = %session_id, "Attempting keyboard-interactive fallback with stored password");

    let mut response = handle
        .authenticate_keyboard_interactive_start(username, None::<String>)
        .await
        .map_err(|e| format!("Keyboard-interactive fallback failed: {e}"))?;

    let mut round = 0u8;
    loop {
        match response {
            KeyboardInteractiveAuthResponse::Success => {
                debug!(session_id = %session_id, "Keyboard-interactive fallback succeeded");
                return Ok(());
            }
            KeyboardInteractiveAuthResponse::Failure { .. } => {
                return Err(
                    "Authentication rejected by server (password and keyboard-interactive fallback both failed)"
                        .to_string(),
                );
            }
            KeyboardInteractiveAuthResponse::InfoRequest { prompts, .. } => {
                round += 1;
                if round > MAX_ROUNDS {
                    return Err(
                        "Keyboard-interactive fallback exceeded maximum prompt rounds".to_string(),
                    );
                }
                let answers: Vec<String> = prompts.iter().map(|_| password.to_string()).collect();
                debug!(
                    session_id = %session_id,
                    round,
                    prompt_count = prompts.len(),
                    "Auto-responding to keyboard-interactive prompts"
                );
                response = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(|e| format!("Keyboard-interactive fallback failed: {e}"))?;
            }
        }
    }
}

/// SSH client handler implementing TOFU host key verification.
pub(crate) struct SshHandler {
    session_id: String,
    host: String,
    port: u16,
    app_handle: AppHandle,
    /// Shared receiver for the TOFU verification response from the frontend.
    verify_rx: Arc<Mutex<Option<oneshot::Receiver<bool>>>>,
}

impl client::Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        // `base_preferred` asks for no host certificates, so russh never
        // negotiates one; should one arrive anyway, it is refused rather than
        // trusted for the plain key inside it.
        let PublicKeyOrCertificate::PublicKey {
            key: server_public_key,
            ..
        } = server_key
        else {
            warn!(
                session_id = %self.session_id,
                host = %self.host,
                port = self.port,
                "Server sent a host certificate that was not asked for — refused"
            );
            return Ok(false);
        };

        let kh_path = match known_hosts_path() {
            Ok(p) => Some(p),
            Err(e) => {
                warn!(session_id = %self.session_id, error = %e, "Cannot resolve known_hosts path");
                None
            }
        };

        // Check if we already know this host.
        debug!(
            session_id = %self.session_id,
            host = %self.host,
            port = self.port,
            key_type = server_public_key.algorithm().as_str(),
            fingerprint = %server_public_key.fingerprint(russh::keys::HashAlg::Sha256),
            "Server offered host key"
        );
        let checked = match kh_path.clone() {
            Some(kh) => {
                trace!(
                    session_id = %self.session_id,
                    known_hosts = %kh.display(),
                    host = %self.host,
                    port = self.port,
                    key_type = server_public_key.algorithm().as_str(),
                    "Checking known_hosts"
                );
                let (host, port, key) = (self.host.clone(), self.port, server_public_key.clone());
                tokio::task::spawn_blocking(move || check_known_host(&kh, &host, port, &key))
                    .await
                    .unwrap_or_else(|e| {
                        warn!(session_id = %self.session_id, error = %e, "known_hosts check failed");
                        KnownHost::ask(HostKeyStatus::Unchecked)
                    })
            }
            None => KnownHost::ask(HostKeyStatus::Unchecked),
        };
        let (status, known) = match checked {
            KnownHost::Trusted => {
                trace!(session_id = %self.session_id, "Host key found in known_hosts — trusted");
                return Ok(true);
            }
            KnownHost::Revoked => {
                warn!(
                    session_id = %self.session_id,
                    host = %self.host,
                    port = self.port,
                    "Server key is REVOKED in known_hosts — refused"
                );
                return Ok(false);
            }
            KnownHost::Ask { status, known } => (status, known),
        };
        match status {
            HostKeyStatus::New => {}
            // Possibly a man in the middle. If accepted, the old entries are
            // replaced below.
            HostKeyStatus::Changed | HostKeyStatus::OtherType => warn!(
                session_id = %self.session_id,
                host = %self.host,
                port = self.port,
                ?status,
                "Server key does not match known_hosts — asking the user"
            ),
            HostKeyStatus::Unchecked => warn!(
                session_id = %self.session_id,
                host = %self.host,
                port = self.port,
                "Server key could not be checked against known_hosts — asking the user"
            ),
        }

        // Emit verification request to the frontend.
        let payload = HostVerifyPayload {
            session_id: self.session_id.clone(),
            host: self.host.clone(),
            port: self.port,
            fingerprint: server_public_key
                .fingerprint(russh::keys::HashAlg::Sha256)
                .to_string(),
            key_type: server_public_key.algorithm().as_str().to_string(),
            status,
            known_keys: known.iter().map(KnownKey::from).collect(),
        };

        self.app_handle
            .emit("ssh-host-verify", &payload)
            .map_err(|e| {
                error!(session_id = %self.session_id, error = %e, "Failed to emit host verify event");
                russh::Error::Disconnect
            })?;

        // Await user response via the oneshot channel, with a 60-second timeout.
        let rx = {
            let mut guard = self.verify_rx.lock().await;
            guard.take()
        };

        let accepted = if let Some(rx) = rx {
            match tokio::time::timeout(std::time::Duration::from_mins(1), rx).await {
                Ok(Ok(answer)) => answer,
                Ok(Err(_)) => {
                    warn!(session_id = %self.session_id, "Host verification channel dropped");
                    false
                }
                Err(_) => {
                    warn!(session_id = %self.session_id, "Host verification timed out after 60s");
                    false
                }
            }
        } else {
            error!(session_id = %self.session_id, "No verification receiver available");
            false
        };

        if accepted {
            if let Some(kh) = kh_path {
                let (host, port, key) = (self.host.clone(), self.port, server_public_key.clone());
                let saved =
                    tokio::task::spawn_blocking(move || save_known_host(&kh, &host, port, &key))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()));
                if let Err(e) = saved {
                    warn!(session_id = %self.session_id, error = %e, "Failed to save known host");
                }
            } else {
                warn!(session_id = %self.session_id, "Skipping known_hosts save — home directory unknown");
            }
        }

        Ok(accepted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn v4(a: u8, b: u8, c: u8, d: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(a, b, c, d))
    }

    fn v6(s: &str) -> IpAddr {
        IpAddr::V6(s.parse::<Ipv6Addr>().unwrap())
    }

    // ── parse_whoami_sid ─────────────────────────────────────────────────
    // The SID names the user the .ssh ACL is granted to, so a misread one
    // would lock the user out; anything not shaped like a SID is refused.

    #[test]
    fn parse_whoami_sid_reads_the_last_field() {
        let sid = "S-1-5-21-2123663517-4207914079-1984504764-1001";
        assert_eq!(
            parse_whoami_sid(&format!("\"dev02\\max\",\"{sid}\"\r\n")),
            Some(sid)
        );
        // A comma or quote in the name does not move the SID.
        assert_eq!(
            parse_whoami_sid(&format!("\"corp\\a,\"\"b\",\"{sid}\"")),
            Some(sid)
        );
    }

    #[test]
    fn parse_whoami_sid_refuses_anything_else() {
        for bad in [
            "",
            "\"dev02\\max\"",
            "\"dev02\\max\",S-1-5-21-1",
            "\"dev02\\max\",\"S-1-\"",
            "\"dev02\\max\",\"S-1-5--1\"",
            "\"dev02\\max\",\"S-1-5-21-1 /grant:r Everyone:F\"",
            "\"dev02\\max\",\"S-2-5-21-1\"",
        ] {
            assert_eq!(parse_whoami_sid(bad), None, "{bad:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn current_user_sid_is_found() {
        // S-1-5-21-… for local and domain accounts, S-1-12-1-… for Entra ID.
        let sid = current_user_sid().expect("whoami should report a SID");
        assert!(
            parse_whoami_sid(&format!("\"x\",\"{sid}\"")).is_some(),
            "{sid}"
        );
    }

    /// The real icacls grant: once inherited access is gone, the user must
    /// still be able to use the directory and the file, or a wrong grant
    /// would lock them out of their own `.ssh`.
    #[cfg(windows)]
    #[test]
    fn restricted_acl_leaves_the_user_access() {
        let dir = std::env::temp_dir().join(format!("ss-acl-{}", uuid::Uuid::new_v4()));
        crate::private_fs::create_private_dir(&dir).unwrap();
        restrict_windows_acl(&dir, true);
        let file = dir.join("known_hosts");
        crate::private_fs::create_private_file(&file).unwrap();
        restrict_windows_acl(&file, false);
        std::fs::write(&file, b"example.com ssh-ed25519 AAAA\n").unwrap();
        assert_eq!(
            std::fs::read(&file).unwrap(),
            b"example.com ssh-ed25519 AAAA\n"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    // ── known_hosts ──────────────────────────────────────────────────────
    // Two Ed25519 host keys, and hashed names written by OpenSSH's
    // `ssh-keygen -H` (its salts are random, so these are fixed copies).

    const KEY_A: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFCV6PCBXf1zkdTnyO+RdSE7kXEJZEYMbP/cwwg2gCVE";
    const KEY_B: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKeZzUm6CXIeRbTFZRJ2neOWvtw+zp58HRv8gEjYYJRT";
    /// `example.com`, hashed.
    const HASHED_22: &str = "|1|e8vrqZzf2k7cmqsRrXJyLpxbkP0=|KqlbjLXsSrr9DPv7M3NleZEq20s=";
    /// `[example.com]:2222`, hashed.
    const HASHED_2222: &str = "|1|nLAFhVAn6D4V6+5ysjLLGE4p1Hg=|iXaEwOI28TrRgrPcPdwqIFTrCfg=";

    fn public_key(entry: &str) -> russh::keys::PublicKey {
        russh::keys::parse_public_key_base64(entry.split(' ').nth(1).unwrap()).unwrap()
    }

    fn scratch_known_hosts() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ss-kh-{}", uuid::Uuid::new_v4()));
        let file = dir.join(".ssh").join("known_hosts");
        (dir, file)
    }

    #[test]
    fn hashed_names_match_what_openssh_wrote() {
        assert!(is_hashed_name(HASHED_22.as_bytes(), "example.com"));
        assert!(is_hashed_name(HASHED_2222.as_bytes(), "[example.com]:2222"));
        assert!(!is_hashed_name(HASHED_22.as_bytes(), "example.org"));
        assert!(!is_hashed_name(HASHED_22.as_bytes(), "[example.com]:2222"));
        assert!(!is_hashed_name(HASHED_2222.as_bytes(), "example.com"));
        assert!(is_hashed_name_strict(HASHED_22.as_bytes(), "example.com"));
        // Matched when replacing entries, but not as OpenSSH writes them,
        // so they trust no key.
        let with_junk = format!("{HASHED_22}|junk");
        let with_break = HASHED_22.replacen("e8vr", "e8\r\nvr", 1);
        let unpadded = HASHED_22.replace('=', "");
        for lenient in [&with_junk, &with_break, &unpadded] {
            assert!(
                is_hashed_name(lenient.as_bytes(), "example.com"),
                "{lenient:?}"
            );
            assert!(
                !is_hashed_name_strict(lenient.as_bytes(), "example.com"),
                "{lenient:?}"
            );
        }
        for bad in [
            "",
            "example.com",
            "|1|",
            "|1|e8vrqZzf2k7cmqsRrXJyLpxbkP0=",
            "|1|e8vrqZzf2k7cmqsRrXJyLpxbkP0=|",
            "|1|not base64!|KqlbjLXsSrr9DPv7M3NleZEq20s=",
            "|2|e8vrqZzf2k7cmqsRrXJyLpxbkP0=|KqlbjLXsSrr9DPv7M3NleZEq20s=",
        ] {
            assert!(!is_hashed_name(bad.as_bytes(), "example.com"), "{bad:?}");
        }
    }

    #[test]
    fn replacing_takes_the_host_out_of_every_entry_and_nothing_else() {
        let unpadded = HASHED_22.trim_end_matches('=');
        // Each line before, and after; empty when it goes.
        let lines: Vec<(Vec<u8>, Vec<u8>)> = [
            ("# example.com ssh-ed25519 AAAA\n", None),
            (&format!("example.com {KEY_A}\n"), Some("")),
            (&format!("[example.com]:2222 {KEY_A}\n"), None),
            (
                &format!("other.org,example.com {KEY_A}\n"),
                Some(&format!("other.org {KEY_A}\n")),
            ),
            (&format!("{HASHED_2222} {KEY_A}\n"), None),
            (&format!("{HASHED_22} {KEY_A}\n"), Some("")),
            (&format!("{unpadded} {KEY_A}\n"), Some("")),
            (&format!("example.com.evil {KEY_A}\n"), None),
            (
                &format!("other.org,{HASHED_22},example.com,10.0.0.1 {KEY_A}\n"),
                Some(&format!("other.org,10.0.0.1 {KEY_A}\n")),
            ),
            (&format!("@revoked example.com {KEY_A}\n"), None),
            (&format!("@cert-authority example.com {KEY_A}\n"), None),
            (&format!("  example.com\t{KEY_A}\n"), Some("")),
            // A key that cannot be parsed, or none, is still this host's.
            ("example.com ssh-ed25519 not-a-key\n", Some("")),
            ("example.com\r\n", Some("")),
            // A `\r` other than before the line break is part of a name.
            (
                &format!("x\r,example.com {KEY_A}\n"),
                Some(&format!("x\r {KEY_A}\n")),
            ),
            // A line ends at a NUL, and a `\r` before it goes with the end.
            ("example.com\r\0junk\n", Some("")),
            ("x,example.com\r\0\n", Some("x\r\0\n")),
            (&format!("other.org {KEY_A}\r\n"), None),
            (
                &format!("  other.org,example.com {KEY_A}\r\n"),
                Some(&format!("  other.org {KEY_A}\r\n")),
            ),
            // The names end at a tab: these are `a` and `a\x0c,example.com`.
            (&format!("a\t,example.com {KEY_A}\n"), None),
            (
                &format!("a\x0c,example.com {KEY_A}\n"),
                Some(&format!("a\x0c {KEY_A}\n")),
            ),
            // Comments and marker lines, after blanks too, and with any marker.
            (&format!("  # example.com {KEY_A}\n"), None),
            (&format!("\t#x,example.com {KEY_A}\n"), None),
            (&format!("@revoked\tx,example.com {KEY_A}\n"), None),
            (&format!("@x,example.com {KEY_A}\n"), None),
            // Taking out the first name would make the next read as a
            // hashed entry, a marker or a comment.
            (&format!("example.com,{HASHED_2222} {KEY_A}\n"), Some("")),
            (
                &format!("example.com,@cert-authority *.corp {KEY_A}\n"),
                Some(""),
            ),
            (&format!("example.com,#x {KEY_A}\n"), Some("")),
            // A hashed name is one name, commas and all.
            (&format!("{HASHED_2222},example.com {KEY_A}\n"), None),
            (&format!("{HASHED_22}|junk {KEY_A}\n"), Some("")),
            ("\n", None),
        ]
        .into_iter()
        .map(|(before, after)| {
            let before = before.as_bytes().to_vec();
            let after = after.map_or_else(|| before.clone(), |a| a.as_bytes().to_vec());
            (before, after)
        })
        .chain([(
            [b"caf\xe9.org ".as_slice(), KEY_A.as_bytes(), b"\n"].concat(),
            [b"caf\xe9.org ".as_slice(), KEY_A.as_bytes(), b"\n"].concat(),
        )])
        .collect();
        let old: Vec<u8> = lines
            .iter()
            .flat_map(|(before, _)| before.clone())
            .collect();

        let new = replace_known_host_entries(&old, "example.com", KEY_B);

        let expected: Vec<u8> = lines
            .iter()
            .flat_map(|(_, after)| after.clone())
            .chain(format!("example.com {KEY_B}\n").into_bytes())
            .collect();
        assert_eq!(
            String::from_utf8_lossy(&new),
            String::from_utf8_lossy(&expected)
        );
        assert_eq!(new, expected);
    }

    /// `without_name` slices the original line at offsets into what
    /// `line_content` returns, which is safe only while that is a prefix.
    #[test]
    fn line_content_is_a_prefix_of_the_line() {
        for line in [
            b"".as_slice(),
            b"\n",
            b"\r\n",
            b"\r",
            b"\0",
            b"\r\0x\r\n",
            b"  example.com\r\0junk\n",
            b"example.com ssh-ed25519 AAAA\r\n",
            b"a\r\r\n",
        ] {
            assert!(line.starts_with(line_content(line)), "{line:?}");
        }
        assert_eq!(line_content(b"example.com\r\0junk\n"), b"example.com");
        assert_eq!(line_content(b"x\r,y z\r\n"), b"x\r,y z");
    }

    #[test]
    fn replacing_ends_the_last_line_before_adding_the_entry() {
        assert_eq!(
            replace_known_host_entries(b"", "example.com", KEY_B),
            format!("example.com {KEY_B}\n").into_bytes()
        );
        assert_eq!(
            replace_known_host_entries(
                format!("other.org {KEY_A}").as_bytes(),
                "example.com",
                KEY_B
            ),
            format!("other.org {KEY_A}\nexample.com {KEY_B}\n").into_bytes()
        );
    }

    #[test]
    fn saved_key_replaces_a_hashed_entry() {
        let (dir, file) = scratch_known_hosts();
        crate::private_fs::create_private_dir(file.parent().unwrap()).unwrap();
        let revocation = format!("@revoked\tx,example.com {KEY_A}\n");
        std::fs::write(
            &file,
            format!(
                "{HASHED_22} {KEY_A}\nother.org {KEY_A}\n10.0.0.1,example.com {KEY_A}\n\
                 {revocation}"
            ),
        )
        .unwrap();
        let (a, b) = (public_key(KEY_A), public_key(KEY_B));
        assert_eq!(
            check_known_host(&file, "example.com", 22, &b),
            ask(HostKeyStatus::Changed, &[KEY_A])
        );

        save_known_host(&file, "example.com", 22, &b).unwrap();

        assert_eq!(
            check_known_host(&file, "example.com", 22, &b),
            KnownHost::Trusted
        );
        assert_eq!(
            check_known_host(&file, "example.com", 22, &a),
            KnownHost::Revoked
        );
        assert_eq!(
            check_known_host(&file, "other.org", 22, &a),
            KnownHost::Trusted
        );
        // The address that shared the old key keeps it.
        assert_eq!(
            check_known_host(&file, "10.0.0.1", 22, &a),
            KnownHost::Trusted
        );
        // The revocation stays as it was.
        assert!(std::fs::read_to_string(&file)
            .unwrap()
            .contains(&format!("\n{revocation}")));
        // The temporary file was renamed into place, not left beside it.
        assert_eq!(
            std::fs::read_dir(file.parent().unwrap()).unwrap().count(),
            1
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_creates_the_directory_and_the_file() {
        let (dir, file) = scratch_known_hosts();
        save_known_host(&file, "example.com", 2222, &public_key(KEY_B)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("[example.com]:2222 {KEY_B}\n")
        );
        #[cfg(unix)]
        for (path, label) in [
            (file.parent().unwrap(), "directory"),
            (file.as_path(), "file"),
        ] {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{label}: group/other bits set: {mode:o}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_follows_a_symlink() {
        let (dir, file) = scratch_known_hosts();
        let target = dir.join("dotfiles").join("known_hosts");
        crate::private_fs::create_private_dir(target.parent().unwrap()).unwrap();
        crate::private_fs::create_private_dir(file.parent().unwrap()).unwrap();
        std::fs::write(&target, format!("other.org {KEY_A}\n")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &file).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_file(&target, &file).is_err() {
            // Creating a symlink takes Developer Mode or an elevated process.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        save_known_host(&file, "example.com", 22, &public_key(KEY_B)).unwrap();

        let link = std::fs::symlink_metadata(&file).unwrap();
        assert!(link.file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            format!("other.org {KEY_A}\nexample.com {KEY_B}\n")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_refuses_anything_but_a_regular_file() {
        let (dir, file) = scratch_known_hosts();
        crate::private_fs::create_private_dir(&file).unwrap();
        assert!(save_known_host(&file, "example.com", 22, &public_key(KEY_B)).is_err());
        assert!(file.is_dir());
        #[cfg(unix)]
        {
            // `/dev/null` there switches known_hosts off.
            let link = file.with_file_name("known_hosts2");
            std::os::unix::fs::symlink("/dev/null", &link).unwrap();
            assert!(save_known_host(&link, "example.com", 22, &public_key(KEY_B)).is_err());
            let meta = std::fs::symlink_metadata(&link).unwrap();
            assert!(meta.file_type().is_symlink());
            // A link that points nowhere.
            let dangling = file.with_file_name("known_hosts3");
            std::os::unix::fs::symlink(dir.join("missing"), &dangling).unwrap();
            assert!(save_known_host(&dangling, "example.com", 22, &public_key(KEY_B)).is_err());
            assert!(!dir.join("missing").exists());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_refuses_a_name_known_hosts_would_misread() {
        let (dir, file) = scratch_known_hosts();
        assert!(save_known_host(&file, "x,example.com", 22, &public_key(KEY_B)).is_err());
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_saves_keep_every_key() {
        let (dir, file) = scratch_known_hosts();
        let key = public_key(KEY_B);
        let hosts: Vec<String> = (0..8).map(|i| format!("host{i}.example.com")).collect();
        std::thread::scope(|scope| {
            for host in &hosts {
                let (file, key) = (&file, &key);
                scope.spawn(move || save_known_host(file, host, 22, key).unwrap());
            }
        });
        for host in &hosts {
            assert_eq!(
                check_known_host(&file, host, 22, &key),
                KnownHost::Trusted,
                "{host} was dropped"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An ECDSA host key, of another type than `KEY_A` and `KEY_B`.
    const KEY_C: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBIuGk2LkL9Zhk/SYkH7TJb74L5w4QlkaMucdUOlqsuERs42bHnnQF6FW78EUHwrKSskQsVkyhbguJ50oqLvh6ko=";

    /// `check_known_host` for `key` offered by `host`:`port`, against a
    /// `known_hosts` file holding `contents`.
    fn check(contents: &[u8], host: &str, port: u16, key: &str) -> KnownHost {
        let (dir, file) = scratch_known_hosts();
        crate::private_fs::create_private_dir(file.parent().unwrap()).unwrap();
        std::fs::write(&file, contents).unwrap();
        let found = check_known_host(&file, host, port, &public_key(key));
        std::fs::remove_dir_all(&dir).unwrap();
        found
    }

    fn ask(status: HostKeyStatus, known: &[&str]) -> KnownHost {
        KnownHost::Ask {
            status,
            known: known.iter().map(|key| public_key(key)).collect(),
        }
    }

    #[test]
    fn a_key_on_record_is_trusted() {
        for (contents, port) in [
            (format!("example.com {KEY_A}\n"), 22),
            (format!("{HASHED_22} {KEY_A}\n"), 22),
            (format!("{HASHED_2222} {KEY_A}\n"), 2222),
            (format!("[example.com]:2222 {KEY_A}\n"), 2222),
            (format!("other.org,example.com\t{KEY_A} a comment\r\n"), 22),
            // Leading blanks, and no line break at the end.
            (format!("  example.com {KEY_A}"), 22),
        ] {
            assert_eq!(
                check(contents.as_bytes(), "example.com", port, KEY_A),
                KnownHost::Trusted,
                "{contents:?}"
            );
        }
    }

    #[test]
    fn one_bad_line_costs_only_that_line() {
        // russh gave up on the whole file over either of the first two.
        let contents = [
            b"caf\xe9.org ssh-ed25519 AAAA\n".as_slice(),
            b"example.com ssh-ed25519 not-a-key\n",
            format!("example.com {KEY_A}\n").as_bytes(),
        ]
        .concat();
        assert_eq!(
            check(&contents, "example.com", 22, KEY_A),
            KnownHost::Trusted
        );
        // With nothing else on record for the host, the key is unchecked.
        assert_eq!(
            check(
                b"example.com ssh-ed25519 not-a-key\n",
                "example.com",
                22,
                KEY_A
            ),
            ask(HostKeyStatus::Unchecked, &[])
        );
        // A bad line for another host does not count.
        assert_eq!(
            check(
                b"other.org ssh-ed25519 not-a-key\n",
                "example.com",
                22,
                KEY_A
            ),
            ask(HostKeyStatus::New, &[])
        );
    }

    #[test]
    fn another_key_is_changed_or_of_another_type() {
        let contents = format!("example.com {KEY_A}\n");
        assert_eq!(
            check(contents.as_bytes(), "example.com", 22, KEY_B),
            ask(HostKeyStatus::Changed, &[KEY_A])
        );
        assert_eq!(
            check(contents.as_bytes(), "example.com", 22, KEY_C),
            ask(HostKeyStatus::OtherType, &[KEY_A])
        );
        // A key of the same type on record as well makes it changed.
        let both = format!("example.com {KEY_C}\nexample.com {KEY_A}\n");
        assert_eq!(
            check(both.as_bytes(), "example.com", 22, KEY_B),
            ask(HostKeyStatus::Changed, &[KEY_A])
        );
        // Another port is another host.
        assert_eq!(
            check(contents.as_bytes(), "example.com", 2222, KEY_B),
            ask(HostKeyStatus::New, &[])
        );
    }

    #[test]
    fn only_an_exact_name_trusts_a_key() {
        for contents in [
            format!("*.com {KEY_A}\n"),
            format!("example.co? {KEY_A}\n"),
            format!("example.com,!example.com {KEY_A}\n"),
            format!("example.com,!*.com {KEY_A}\n"),
            format!("# example.com {KEY_A}\n"),
            format!("@cert-authority example.com {KEY_A}\n"),
            format!("@x example.com {KEY_A}\n"),
            format!("{HASHED_22}|junk {KEY_A}\n"),
        ] {
            assert_eq!(
                check(contents.as_bytes(), "example.com", 22, KEY_A),
                ask(HostKeyStatus::New, &[]),
                "{contents:?}"
            );
        }
    }

    #[test]
    fn a_revoked_key_is_refused() {
        for contents in [
            format!("example.com {KEY_A}\n@revoked example.com {KEY_A}\n"),
            format!("@revoked * {KEY_A}\nexample.com {KEY_A}\n"),
            format!("@revoked\t*.COM\t{KEY_A}\n"),
            format!("@revoked {HASHED_22} {KEY_A}\n"),
        ] {
            assert_eq!(
                check(contents.as_bytes(), "example.com", 22, KEY_A),
                KnownHost::Revoked,
                "{contents:?}"
            );
        }
        // Not for a host the pattern rules out, nor for another key.
        let contents =
            format!("example.com {KEY_A}\n@revoked *,!example.com {KEY_A}\n@revoked * {KEY_B}\n");
        assert_eq!(
            check(contents.as_bytes(), "example.com", 22, KEY_A),
            KnownHost::Trusted
        );
    }

    #[test]
    fn names_are_looked_up_as_openssh_writes_them() {
        assert_eq!(known_hosts_name("Example.COM", 22).unwrap(), "example.com");
        assert_eq!(known_hosts_name("[::1]", 22).unwrap(), "::1");
        assert_eq!(known_hosts_name("[::1]", 2222).unwrap(), "[::1]:2222");
        assert_eq!(known_hosts_name("::1", 2222).unwrap(), "[::1]:2222");
        assert_eq!(
            known_hosts_name("[FE80::1%ETH0]", 22).unwrap(),
            "fe80::1%eth0"
        );
        // A plain name matches in either case; a hashed one is of the
        // lower-case name, as OpenSSH hashes it.
        for (contents, host) in [
            (format!("example.com {KEY_A}\n"), "Example.COM"),
            (format!("EXAMPLE.com {KEY_A}\n"), "example.com"),
            (format!("{HASHED_22} {KEY_A}\n"), "Example.COM"),
            (format!("::1 {KEY_A}\n"), "[::1]"),
        ] {
            assert_eq!(
                check(contents.as_bytes(), host, 22, KEY_A),
                KnownHost::Trusted,
                "{contents:?} for {host}"
            );
        }
        for (contents, host) in [
            (format!("@revoked {HASHED_22} {KEY_A}\n"), "Example.COM"),
            (format!("@revoked ::1 {KEY_A}\n"), "[::1]"),
        ] {
            assert_eq!(
                check(contents.as_bytes(), host, 22, KEY_A),
                KnownHost::Revoked,
                "{contents:?} for {host}"
            );
        }
        // Saving writes the name so, and replaces an entry in another case.
        let (dir, file) = scratch_known_hosts();
        crate::private_fs::create_private_dir(file.parent().unwrap()).unwrap();
        std::fs::write(&file, format!("EXAMPLE.com {KEY_A}\n")).unwrap();
        save_known_host(&file, "Example.COM", 22, &public_key(KEY_B)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("example.com {KEY_B}\n")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_prompt_shows_a_few_keys_on_record() {
        use russh::keys::ssh_key::public::{Ed25519PublicKey, KeyData};
        let contents = (0u8..20)
            .map(|i| {
                let key =
                    russh::keys::PublicKey::new(KeyData::Ed25519(Ed25519PublicKey([i; 32])), "");
                format!("example.com {}\n", key.to_openssh().unwrap())
            })
            .collect::<Vec<_>>()
            .concat();
        let KnownHost::Ask { status, known } = check(contents.as_bytes(), "example.com", 22, KEY_B)
        else {
            panic!("not asked");
        };
        assert_eq!(status, HostKeyStatus::Changed);
        assert_eq!(known.len(), MAX_KNOWN_KEYS_SHOWN);
    }

    #[test]
    fn no_file_is_nothing_on_record_and_a_directory_is_unchecked() {
        let (dir, file) = scratch_known_hosts();
        let key = public_key(KEY_A);
        assert_eq!(
            check_known_host(&file, "example.com", 22, &key),
            ask(HostKeyStatus::New, &[])
        );
        crate::private_fs::create_private_dir(&file).unwrap();
        assert_eq!(
            check_known_host(&file, "example.com", 22, &key),
            ask(HostKeyStatus::Unchecked, &[])
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_over_the_limit_is_neither_read_nor_rewritten() {
        let (dir, file) = scratch_known_hosts();
        crate::private_fs::create_private_dir(file.parent().unwrap()).unwrap();
        std::fs::File::create(&file)
            .unwrap()
            .set_len(MAX_KNOWN_HOSTS_SIZE + 1)
            .unwrap();
        let key = public_key(KEY_A);
        assert_eq!(
            check_known_host(&file, "example.com", 22, &key),
            ask(HostKeyStatus::Unchecked, &[])
        );
        assert!(save_known_host(&file, "example.com", 22, &key).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_line_is_read_as_openssh_reads_it() {
        let blob = KEY_A.split(' ').nth(1).unwrap();
        // The key type must match the key.
        assert_eq!(
            check(
                format!("example.com ssh-rsa {blob}\n").as_bytes(),
                "example.com",
                22,
                KEY_A
            ),
            ask(HostKeyStatus::Unchecked, &[])
        );
        // A line ends at a NUL, so this one names only `x`.
        assert_eq!(
            check(
                format!("x\0,example.com {KEY_A}\n").as_bytes(),
                "example.com",
                22,
                KEY_A
            ),
            ask(HostKeyStatus::New, &[])
        );
        // A bad line beside a readable one still shows the change.
        let contents = format!("example.com ssh-ed25519 not-a-key\nexample.com {KEY_A}\n");
        assert_eq!(
            check(contents.as_bytes(), "example.com", 22, KEY_B),
            ask(HostKeyStatus::Changed, &[KEY_A])
        );
        // The same key on two lines is shown once.
        let contents = format!("example.com {KEY_A}\n{HASHED_22} {KEY_A}\n");
        assert_eq!(
            check(contents.as_bytes(), "example.com", 22, KEY_B),
            ask(HostKeyStatus::Changed, &[KEY_A])
        );
    }

    #[test]
    fn an_unreadable_revocation_leaves_the_key_unchecked() {
        let revocation = "@revoked example.com ssh-ed25519 not-a-key\n";
        let contents = format!("example.com {KEY_A}\n{revocation}");
        assert_eq!(
            check(contents.as_bytes(), "example.com", 22, KEY_A),
            ask(HostKeyStatus::Unchecked, &[])
        );
        assert_eq!(
            check(contents.as_bytes(), "example.com", 22, KEY_B),
            ask(HostKeyStatus::Changed, &[KEY_A])
        );
        assert_eq!(
            check(revocation.as_bytes(), "example.com", 22, KEY_A),
            ask(HostKeyStatus::Unchecked, &[])
        );
        // One for another host does not count.
        let contents = format!("example.com {KEY_A}\n@revoked other.org ssh-ed25519 x\n");
        assert_eq!(
            check(contents.as_bytes(), "example.com", 22, KEY_A),
            KnownHost::Trusted
        );
    }

    /// Whatever `check_known_host` counts for a host, saving a new key for
    /// it replaces: otherwise the new key would show as changed for good.
    #[test]
    fn saving_replaces_every_entry_the_check_counts() {
        for line in [
            format!("example.com {KEY_A}\n"),
            format!("other.org,example.com {KEY_A}\r\n"),
            format!("x\r,example.com {KEY_A}\n"),
            "example.com\r\0junk\n".to_string(),
            format!("\t example.com\t{KEY_A} comment\n"),
            format!("{HASHED_22} {KEY_A}\n"),
            format!("example.com,!other.org {KEY_A}\n"),
            format!("example.com {KEY_C}\n"),
            "example.com ssh-ed25519 not-a-key\n".to_string(),
            format!("example.com {KEY_A}"),
        ] {
            let (dir, file) = scratch_known_hosts();
            crate::private_fs::create_private_dir(file.parent().unwrap()).unwrap();
            std::fs::write(&file, &line).unwrap();
            let key = public_key(KEY_B);
            assert_ne!(
                check_known_host(&file, "example.com", 22, &key),
                ask(HostKeyStatus::New, &[]),
                "{line:?} is not counted"
            );
            save_known_host(&file, "example.com", 22, &key).unwrap();
            assert_eq!(
                check_known_host(&file, "example.com", 22, &key),
                KnownHost::Trusted,
                "{line:?} was left behind"
            );
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn patterns_match_as_in_openssh() {
        for (pattern, text) in [
            ("*", "example.com"),
            ("*", ""),
            ("*.example.com", "a.example.com"),
            ("host?.com", "host1.com"),
            ("EXAMPLE.com", "example.COM"),
            ("a*b*c", "aXbYc"),
            ("*a*", "banana"),
        ] {
            assert!(
                pattern_matches(pattern.as_bytes(), text.as_bytes()),
                "{pattern} {text}"
            );
        }
        for (pattern, text) in [
            ("*.example.com", "example.com"),
            ("host?.com", "host.com"),
            ("a*b", "ac"),
            ("", "a"),
        ] {
            assert!(
                !pattern_matches(pattern.as_bytes(), text.as_bytes()),
                "{pattern} {text}"
            );
        }
        // Many stars against a long name that does not match end at once.
        let pattern = "*a".repeat(60);
        let text = format!("{}b", "a".repeat(400));
        assert!(!pattern_matches(pattern.as_bytes(), text.as_bytes()));
        // So does a line-long pattern: it has more letters than the name.
        let pattern = "*a".repeat(1_000_000);
        assert!(!pattern_matches(pattern.as_bytes(), text.as_bytes()));
        let pattern = format!("{}b", "*".repeat(1_000_000));
        assert!(pattern_matches(pattern.as_bytes(), text.as_bytes()));
    }

    #[test]
    fn host_names_and_addresses_are_valid() {
        for host in [
            "example.com",
            "my-host_1.local",
            "10.0.0.1",
            "::1",
            "fe80::1%eth0",
            "bücher.example",
            // Windows resolves these.
            "[::1]",
            "[2001:db8::1]",
            "[fe80::1%eth0]",
        ] {
            assert_eq!(validate_host_name(host), Ok(()), "{host:?}");
        }
    }

    #[test]
    fn host_names_known_hosts_would_misread_are_refused() {
        for host in [
            "",
            "x,example.com",
            "example.com ssh-ed25519",
            "x\nexample.com",
            "x\texample.com",
            "*",
            "*.example.com",
            "host?",
            "!example.com",
            "[example.com]:2222",
            "[example.com]",
            "[10.0.0.1]",
            "[::1]:2222",
            "[[::1]]",
            "[::1",
            "::1]",
            "[]",
            "[%eth0]",
            "[::1%a,example.com]",
            // OpenSSH's own refusals, which could also shape the prompt.
            "x<br>y",
            "x'y",
            "x\"y",
            "x`y",
            "x$y",
            "x\\y",
            "x;y",
            "x&y",
            "x(y)",
            "x{y}",
            // Invisible characters with which a name could pass for another.
            "moc.elpmaxe\u{202E}",
            "exa\u{200B}mple.com",
            "\u{FEFF}example.com",
            "x\u{2066}y\u{2069}",
            "exam\u{00AD}ple.com",
            "e\u{034F}xample.com",
            "example\u{FE0F}.com",
            "example.com\u{E0041}",
            "exa\u{206A}mple.com",
            "\u{2800}example.com",
            "example\u{FFF9}.com",
            "\u{3164}example.com",
            "#example.com",
            "@revoked",
            "|1|abc|def",
            "x\u{0}y",
            "x\u{85}y",
            "x\u{2028}y",
        ] {
            let err = validate_host_name(host).unwrap_err();
            assert!(
                !err.contains([
                    '\n', '\t', '\u{0}', '\u{85}', '\u{2028}', '\u{202E}', '\u{200B}', '\u{FEFF}',
                    '\u{2066}', '\u{2069}'
                ]),
                "{err:?}"
            );
        }
    }

    // ── Algorithm preferences ────────────────────────────────────────────
    // Offering a host certificate algorithm would let the server answer
    // with a certificate, which `check_server_key` refuses: the connection
    // would fail where a plain host key would have worked.

    #[test]
    fn no_host_certificates_are_asked_for() {
        assert!(base_preferred().host_key_certificates.is_empty());
        assert!(legacy_preferred().host_key_certificates.is_empty());
    }

    // ── is_restricted_ip ─────────────────────────────────────────────────
    // Security-critical SSRF guard. Every branch corresponds to a CIDR
    // range that must not be reachable via SSH tunnel; adding a branch
    // without a matching test is how the guard silently regresses.

    #[test]
    fn restricted_ipv4_loopback() {
        assert!(is_restricted_ip(&v4(127, 0, 0, 1)));
        assert!(is_restricted_ip(&v4(127, 255, 255, 254)));
    }

    #[test]
    fn restricted_ipv4_private_ranges() {
        assert!(is_restricted_ip(&v4(10, 0, 0, 1)));
        assert!(is_restricted_ip(&v4(10, 255, 255, 254)));
        assert!(is_restricted_ip(&v4(172, 16, 0, 1)));
        assert!(is_restricted_ip(&v4(172, 31, 255, 254)));
        assert!(is_restricted_ip(&v4(192, 168, 0, 1)));
        assert!(is_restricted_ip(&v4(192, 168, 255, 254)));
    }

    #[test]
    fn restricted_ipv4_link_local_and_specials() {
        assert!(is_restricted_ip(&v4(169, 254, 0, 1)));
        assert!(is_restricted_ip(&v4(0, 0, 0, 0)));
        assert!(is_restricted_ip(&v4(0, 255, 255, 255))); // all of 0.0.0.0/8
        assert!(is_restricted_ip(&v4(255, 255, 255, 255)));
    }

    #[test]
    fn restricted_ipv4_cgnat() {
        assert!(is_restricted_ip(&v4(100, 64, 0, 1)));
        assert!(is_restricted_ip(&v4(100, 127, 255, 254)));
    }

    #[test]
    fn restricted_ipv4_multicast_reserved_and_special_purpose() {
        assert!(is_restricted_ip(&v4(224, 0, 0, 1))); // 224.0.0.0/4
        assert!(is_restricted_ip(&v4(239, 255, 255, 255)));
        assert!(is_restricted_ip(&v4(240, 0, 0, 1))); // 240.0.0.0/4
        assert!(is_restricted_ip(&v4(254, 255, 255, 255)));
        assert!(is_restricted_ip(&v4(192, 0, 0, 1))); // 192.0.0.0/24
        assert!(is_restricted_ip(&v4(192, 0, 0, 255)));
        assert!(is_restricted_ip(&v4(198, 18, 0, 1))); // 198.18.0.0/15
        assert!(is_restricted_ip(&v4(198, 19, 255, 254)));
        // Carried inside an IPv6 address too.
        assert!(is_restricted_ip(&v6("::ffff:198.18.0.1")));
    }

    #[test]
    fn allowed_ipv4_public() {
        // Anything outside the restricted CIDRs must pass — including
        // edge addresses just outside private blocks.
        assert!(!is_restricted_ip(&v4(8, 8, 8, 8)));
        assert!(!is_restricted_ip(&v4(1, 1, 1, 1)));
        assert!(!is_restricted_ip(&v4(1, 0, 0, 0))); // just outside 0/8
        assert!(!is_restricted_ip(&v4(172, 15, 255, 254))); // just outside 172.16/12
        assert!(!is_restricted_ip(&v4(172, 32, 0, 1))); // just outside 172.16/12
        assert!(!is_restricted_ip(&v4(192, 167, 255, 254))); // just outside 192.168/16
        assert!(!is_restricted_ip(&v4(11, 0, 0, 1))); // just outside 10/8
        assert!(!is_restricted_ip(&v4(100, 63, 255, 254))); // just outside CGNAT
        assert!(!is_restricted_ip(&v4(100, 128, 0, 1))); // just outside CGNAT
        assert!(!is_restricted_ip(&v4(223, 255, 255, 255))); // just below multicast
        assert!(!is_restricted_ip(&v4(192, 0, 1, 1))); // just outside 192.0.0.0/24
        assert!(!is_restricted_ip(&v4(191, 255, 255, 255))); // just below 192.0.0.0/24
        assert!(!is_restricted_ip(&v4(198, 17, 255, 254))); // just below 198.18/15
        assert!(!is_restricted_ip(&v4(198, 20, 0, 1))); // just past 198.18/15
    }

    #[test]
    fn restricted_ipv6_loopback_and_unspecified() {
        assert!(is_restricted_ip(&v6("::1")));
        assert!(is_restricted_ip(&v6("::")));
    }

    #[test]
    fn restricted_ipv6_multicast() {
        assert!(is_restricted_ip(&v6("ff02::1"))); // ff00::/8
        assert!(is_restricted_ip(&v6("ff05::1:3")));
        assert!(is_restricted_ip(&v6("ffff:ffff::1")));
        // Below ff00::/8 lies site-local space, restricted in its own right,
        // so there is no allowed edge there; a public address stands in.
        assert!(!is_restricted_ip(&v6("2001:4860:4860::8888")));
    }

    #[test]
    fn restricted_ipv6_link_local_and_ula() {
        assert!(is_restricted_ip(&v6("fe80::1")));
        assert!(is_restricted_ip(&v6("fc00::1")));
        assert!(is_restricted_ip(&v6("fd00::1")));
    }

    #[test]
    fn restricted_ipv6_ranges_are_matched_by_prefix() {
        // The whole of each range, not just its first address.
        assert!(is_restricted_ip(&v6("fe80:1234::1"))); // fe80::/10
        assert!(is_restricted_ip(&v6("febf:ffff::1"))); // end of fe80::/10
        assert!(is_restricted_ip(&v6("fec0::1"))); // fec0::/10
        assert!(is_restricted_ip(&v6("feff:ffff::1"))); // end of fec0::/10
        assert!(is_restricted_ip(&v6("fc12:3456::1"))); // fc00::/7
        assert!(is_restricted_ip(&v6("fd12:3456::1"))); // fc00::/7
        assert!(is_restricted_ip(&v6("fdff:ffff::1"))); // end of fc00::/7
        assert!(is_restricted_ip(&v6("64:ff9b:1::808:808"))); // local-use NAT64

        assert!(!is_restricted_ip(&v6("fbff:ffff::1"))); // just below fc00::/7
        assert!(!is_restricted_ip(&v6("fe7f:ffff::1"))); // just below fe80::/10
        assert!(!is_restricted_ip(&v6("64:ff9b:2::1"))); // just past 64:ff9b:1::/48
    }

    #[test]
    fn restricted_ipv6_carrying_a_restricted_ipv4() {
        assert!(is_restricted_ip(&v6("::ffff:10.0.0.1"))); // IPv4-mapped
        assert!(is_restricted_ip(&v6("::ffff:127.0.0.1")));
        assert!(is_restricted_ip(&v6("::ffff:192.168.1.1")));
        assert!(is_restricted_ip(&v6("::ffff:0:172.16.0.1"))); // IPv4-translated
        assert!(is_restricted_ip(&v6("::10.0.0.1"))); // IPv4-compatible
        assert!(is_restricted_ip(&v6("::0.0.0.2"))); // IPv4-compatible, 0.0.0.0/8
        assert!(is_restricted_ip(&v6("64:ff9b::10.0.0.1"))); // NAT64
        assert!(is_restricted_ip(&v6("64:ff9b::100.64.0.1")));
        assert!(is_restricted_ip(&v6("2002:a00:1::1"))); // 6to4 of 10.0.0.1
        assert!(is_restricted_ip(&v6("2002:7f00:1::1"))); // 6to4 of 127.0.0.1
                                                          // Teredo, client 10.0.0.1 (inverted: f5ff:fffe) behind a public server.
        assert!(is_restricted_ip(&v6(
            "2001:0:4136:e378:8000:63bf:f5ff:fffe"
        )));
        // Teredo, server 10.0.0.1 for a public client.
        assert!(is_restricted_ip(&v6("2001:0:a00:1:8000:63bf:3fff:fdd2")));
    }

    #[test]
    fn allowed_ipv6_carrying_a_public_ipv4() {
        assert!(!is_restricted_ip(&v6("::ffff:8.8.8.8")));
        assert!(!is_restricted_ip(&v6("::ffff:0:8.8.8.8")));
        assert!(!is_restricted_ip(&v6("::8.8.8.8")));
        assert!(!is_restricted_ip(&v6("64:ff9b::8.8.8.8")));
        assert!(!is_restricted_ip(&v6("2002:808:808::1")));
        // RFC 4380's example: server 65.54.227.120, client 192.0.2.45.
        assert!(!is_restricted_ip(&v6(
            "2001:0:4136:e378:8000:63bf:3fff:fdd2"
        )));
    }

    #[test]
    fn allowed_ipv6_public() {
        // GUA addresses (2000::/3) must be allowed.
        assert!(!is_restricted_ip(&v6("2001:db8::1"))); // documentation range, but in GUA
        assert!(!is_restricted_ip(&v6("2606:4700:4700::1111"))); // 1.1.1.1 v6
    }

    // ── resolve_unrestricted ─────────────────────────────────────────────
    // The connection goes to exactly the addresses returned, so they must be
    // the addresses checked.

    #[tokio::test]
    async fn resolve_unrestricted_returns_a_public_literal_as_given() {
        assert_eq!(
            resolve_unrestricted("203.0.113.7", 2222).await.unwrap(),
            ["203.0.113.7:2222".parse::<SocketAddr>().unwrap()]
        );
        assert_eq!(
            resolve_unrestricted("2606:4700:4700::1111", 22)
                .await
                .unwrap(),
            ["[2606:4700:4700::1111]:22".parse::<SocketAddr>().unwrap()]
        );
    }

    #[tokio::test]
    async fn resolve_unrestricted_refuses_restricted_addresses() {
        for host in ["10.0.0.1", "::1", "::ffff:192.168.1.1"] {
            assert!(resolve_unrestricted(host, 22).await.is_err(), "{host}");
        }
        // A name is refused by the addresses it resolves to, without the
        // message naming them. Every system the app runs on resolves
        // `localhost` to loopback without asking a DNS server.
        let error = resolve_unrestricted("localhost", 22).await.unwrap_err();
        assert!(error.contains("restricted address"), "{error}");
        assert!(
            !error.contains("127.0.0.1") && !error.contains("::1"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn resolve_unrestricted_keeps_the_resolver_error_out_of_the_message() {
        // `.invalid` never resolves (RFC 6761).
        assert_eq!(
            resolve_unrestricted("server.invalid", 22)
                .await
                .unwrap_err(),
            "Cannot resolve host 'server.invalid'"
        );
    }

    #[tokio::test]
    async fn lookup_keeps_its_permit_until_the_resolver_answers() {
        static PERMITS: Semaphore = Semaphore::const_new(1);
        let (answer, answered) = std::sync::mpsc::channel::<()>();
        let slow = run_lookup(&PERMITS, move || {
            let _ = answered.recv();
            Ok(Vec::new())
        });

        // The caller gives up while the resolver is still busy ...
        let gave_up = tokio::time::timeout(std::time::Duration::from_millis(100), slow).await;
        assert!(gave_up.is_err());
        // ... and the resolver keeps the permit, so the next lookup waits.
        assert_eq!(PERMITS.available_permits(), 0);

        // Once the resolver answers, the permit comes back.
        answer.send(()).unwrap();
        let back = tokio::time::timeout(std::time::Duration::from_secs(5), PERMITS.acquire()).await;
        assert!(back.is_ok(), "the permit did not come back");
    }

    #[test]
    fn next_checked_never_falls_back_to_the_name() {
        let addr: SocketAddr = "203.0.113.7:22".parse().unwrap();
        let mut checked = Some(VecDeque::from([vec![addr]]));
        assert_eq!(next_checked(&mut checked), Ok(Some(vec![addr])));
        // Running out while checking is an error, not `None` (by name).
        assert!(next_checked(&mut checked).is_err());
        // Nothing checked: by name.
        assert_eq!(next_checked(&mut None), Ok(None));
    }

    #[test]
    fn tunnel_failure_leaves_the_bastion_text_out() {
        let refused = russh::Error::ChannelOpenFailure(russh::ChannelOpenFailure::Other {
            code: 42,
            reason: "\u{202e}internal-db.corp: connection refused".to_string(),
        });
        assert_eq!(tunnel_failure(&refused), "the jump host refused it");
        let failed = russh::Error::ChannelOpenFailure(russh::ChannelOpenFailure::ConnectFailed);
        assert_eq!(tunnel_failure(&failed), "the jump host could not connect");
    }

    // ── sanitize_ssh_error ──────────────────────────────────────────────
    // Maps raw russh / std::io errors to user-facing messages; the goal is
    // that nothing leaking internal paths, file descriptors, or stack
    // traces reaches the UI, and that the legacy-algorithm hint fires on
    // every variant of "no common algo" russh produces.

    #[test]
    fn ssh_error_connection_refused() {
        assert!(
            sanitize_ssh_error("Connection refused (os error 111)").contains("connection refused")
        );
    }

    #[test]
    fn ssh_error_timeout() {
        assert!(sanitize_ssh_error("connection timed out").contains("timed out"));
        assert!(sanitize_ssh_error("read timeout after 30s").contains("timed out"));
    }

    #[test]
    fn ssh_error_unreachable() {
        assert!(sanitize_ssh_error("No route to host").contains("unreachable"));
        assert!(sanitize_ssh_error("Network is unreachable").contains("unreachable"));
    }

    #[test]
    fn ssh_error_dns() {
        assert!(sanitize_ssh_error("Name or service not known").contains("resolved"));
        assert!(sanitize_ssh_error("failed to resolve hostname foo").contains("resolved"));
    }

    #[test]
    fn ssh_error_no_common_algo_variants() {
        // All three russh variants must produce the legacy-algorithm hint.
        assert!(sanitize_ssh_error("No common Kex algorithm").contains("Legacy algorithms"));
        assert!(sanitize_ssh_error("key exchange init failed").contains("Legacy algorithms"));
        assert!(sanitize_ssh_error("key exchange failed").contains("Legacy algorithms"));
    }

    #[test]
    fn ssh_error_authentication() {
        assert!(
            sanitize_ssh_error("authentication failed for user root").contains("authentication")
        );
    }

    #[test]
    fn ssh_error_fallback() {
        // Anything we don't recognize collapses to a generic message —
        // critical so internal paths / FDs / OS error codes never reach
        // the toast.
        let s = sanitize_ssh_error("/home/user/.ssh/id_rsa: permission denied (oserror 13)");
        assert_eq!(s, "SSH connection failed");
    }

    // ── Closing connections ──────────────────────────────────────────────
    // An in-process russh server stands in for the remote device. Its
    // session future finishing is the device seeing the connection end; until
    // then the device counts the session as in use.

    struct TestServer;

    impl russh::server::Handler for TestServer {
        type Error = russh::Error;

        fn auth_none(
            &mut self,
            _: &str,
        ) -> impl std::future::Future<Output = Result<russh::server::Auth, Self::Error>> + Send
        {
            std::future::ready(Ok(russh::server::Auth::Accept))
        }

        async fn channel_open_session(
            &mut self,
            _: russh::Channel<russh::server::Msg>,
            reply: russh::server::ChannelOpenHandle,
            _: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            reply.accept().await;
            Ok(())
        }
    }

    struct TestClient;

    impl client::Handler for TestClient {
        type Error = russh::Error;

        fn check_server_key(
            &mut self,
            _: &PublicKeyOrCertificate,
        ) -> impl std::future::Future<Output = Result<bool, Self::Error>> + Send {
            std::future::ready(Ok(true))
        }
    }

    /// An authenticated client connected to a new in-process server, and a
    /// receiver that resolves once the server's side of the connection ends.
    async fn test_connection() -> (client::Handle<TestClient>, oneshot::Receiver<()>) {
        test_connection_to(TestServer).await
    }

    /// [`test_connection`], to a server run by `server`.
    async fn test_connection_to(
        server: impl russh::server::Handler<Error = russh::Error> + Send + 'static,
    ) -> (client::Handle<TestClient>, oneshot::Receiver<()>) {
        let (addr, ended_rx) = spawn_test_server(server).await;
        let mut handle = client::connect(Arc::new(client::Config::default()), addr, TestClient)
            .await
            .unwrap();
        assert!(handle.authenticate_none("test").await.unwrap().success());
        (handle, ended_rx)
    }

    /// The address of a new in-process server run by `server`, which takes
    /// one connection, and a receiver that resolves once that connection
    /// ends on the server's side.
    async fn spawn_test_server(
        server: impl russh::server::Handler<Error = russh::Error> + Send + 'static,
    ) -> (SocketAddr, oneshot::Receiver<()>) {
        use russh::keys::ssh_key::private::Ed25519Keypair;

        let mut config = russh::server::Config {
            inactivity_timeout: None,
            auth_rejection_time: std::time::Duration::ZERO,
            ..Default::default()
        };
        config
            .keys
            .push(russh::keys::PrivateKey::from(Ed25519Keypair::from_seed(
                &[7; 32],
            )));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (ended_tx, ended_rx) = oneshot::channel();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let running = russh::server::run_stream(Arc::new(config), socket, server)
                .await
                .unwrap();
            let _ = running.await;
            let _ = ended_tx.send(());
        });
        (addr, ended_rx)
    }

    const ENDED_WITHIN: std::time::Duration = std::time::Duration::from_secs(5);

    #[tokio::test]
    async fn held_channel_keeps_connection_open_after_handle_drop() {
        // The leak `Drop for SshSession` guards against: dropping the handle
        // alone does not close the connection while a reader task still
        // holds the channel.
        let (handle, mut ended) = test_connection().await;
        let channel = handle.channel_open_session().await.unwrap();
        let reader = tokio::spawn(async move {
            let _channel = channel;
            std::future::pending::<()>().await;
        });
        drop(handle);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(500), &mut ended)
                .await
                .is_err(),
            "the connection closed although the channel is still held"
        );

        // Stopping the reader, as the `Drop` impl does, closes it.
        reader.abort();
        tokio::time::timeout(ENDED_WITHIN, ended)
            .await
            .expect("the connection is still open")
            .unwrap();
    }

    #[tokio::test]
    async fn close_disconnects_target_and_bastions() {
        let (target, target_ended) = test_connection().await;
        let (bastion, bastion_ended) = test_connection().await;
        let target = Arc::new(target);
        // Held, as the reader task would hold it.
        let _channel = target.channel_open_session().await.unwrap();
        let connection = SshConnection {
            target: Arc::clone(&target),
            bastions: vec![bastion],
        };

        connection.close().await;

        // close() returns only once every connection in the chain is closed.
        assert!(connection.is_closed());
        tokio::time::timeout(ENDED_WITHIN, target_ended)
            .await
            .expect("the target connection is still open")
            .unwrap();
        tokio::time::timeout(ENDED_WITHIN, bastion_ended)
            .await
            .expect("the bastion connection is still open")
            .unwrap();
    }

    #[tokio::test]
    async fn close_of_closed_connection_returns_at_once() {
        let (handle, _ended) = test_connection().await;
        let connection = SshConnection {
            target: Arc::new(handle),
            bastions: Vec::new(),
        };
        connection.close().await;

        let started = std::time::Instant::now();
        connection.close().await;
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }

    #[tokio::test]
    async fn failed_chain_disconnects_the_hops_already_up() {
        let (first, first_ended) = test_connection().await;
        let (second, second_ended) = test_connection().await;
        let mut chain = PendingChain::default();
        chain.push(first);
        chain.push(second);

        // As a `?` part way through `connect_with_hops` would. The close runs
        // on Tauri's own global runtime, which works inside a test as well.
        drop(chain);

        tokio::time::timeout(ENDED_WITHIN, first_ended)
            .await
            .expect("the first hop is still connected")
            .unwrap();
        tokio::time::timeout(ENDED_WITHIN, second_ended)
            .await
            .expect("the second hop is still connected")
            .unwrap();
    }

    #[tokio::test]
    async fn chain_handed_to_the_session_stays_connected() {
        let (bastion, mut bastion_ended) = test_connection().await;
        let (target, mut target_ended) = test_connection().await;
        let mut chain = PendingChain::default();
        chain.push(bastion);
        chain.push(target);

        let connection = chain.into_connection();
        assert_eq!(connection.bastions.len(), 1);
        let settle = std::time::Duration::from_millis(300);
        assert!(
            tokio::time::timeout(settle, &mut bastion_ended)
                .await
                .is_err(),
            "handing over the chain closed the bastion"
        );
        assert!(
            tokio::time::timeout(settle, &mut target_ended)
                .await
                .is_err(),
            "handing over the chain closed the target"
        );
        drop(connection);
    }

    #[tokio::test]
    async fn closing_a_chain_returns_once_all_are_closed() {
        let (bastion, bastion_ended) = test_connection().await;
        let (target, target_ended) = test_connection().await;
        let mut chain = PendingChain::default();
        chain.push(bastion);
        chain.push(target);

        chain.close().await;

        tokio::time::timeout(ENDED_WITHIN, target_ended)
            .await
            .expect("the target is still connected")
            .unwrap();
        tokio::time::timeout(ENDED_WITHIN, bastion_ended)
            .await
            .expect("the bastion is still connected")
            .unwrap();
    }

    // ── Connecting to checked addresses ──────────────────────────────────
    // With Restrict private IPs on, the connection must go to the addresses
    // checked; a second lookup of the name could get a different answer.

    #[tokio::test]
    async fn checked_connection_goes_to_the_checked_addresses_not_the_name() {
        let (addr, _ended) = spawn_test_server(TestServer).await;

        // `.invalid` never resolves (RFC 6761), so this only connects if
        // the name is left alone.
        let config = Arc::new(client::Config::default());
        let mut handle =
            connect_checked(config, "server.invalid", 22, Some(vec![addr]), TestClient)
                .await
                .expect("the checked address should connect");

        assert!(handle.authenticate_none("test").await.unwrap().success());
    }

    #[tokio::test]
    async fn unchecked_connection_goes_by_name() {
        let (addr, _ended) = spawn_test_server(TestServer).await;

        // `localhost` may resolve to `::1` first, where nothing listens; as
        // with any name, each address is tried in turn.
        let config = Arc::new(client::Config::default());
        let mut handle = connect_checked(config, "localhost", addr.port(), None, TestClient)
            .await
            .expect("the name should connect");

        assert!(handle.authenticate_none("test").await.unwrap().success());
    }

    // ── Tunnels through a jump host ──────────────────────────────────────
    // With Restrict private IPs on, the bastion must be sent the addresses
    // checked, never the name: it would resolve the name itself.

    /// What a test bastion does with a tunnel request.
    #[derive(Clone, Copy)]
    enum TunnelAnswer {
        /// Opens tunnels to this host and refuses the rest.
        OpensOnly(&'static str),
        RefusesAll,
        /// Refuses every tunnel, each after this long.
        RefusesAfter(std::time::Duration),
        NeverAnswers,
    }

    /// A bastion that records the host each tunnel asks for, and answers as
    /// `answer` says.
    struct TunnelServer {
        asked: Arc<std::sync::Mutex<Vec<String>>>,
        answer: TunnelAnswer,
        /// Requests left unanswered, since dropping one refuses it.
        unanswered: Vec<russh::server::ChannelOpenHandle>,
    }

    impl russh::server::Handler for TunnelServer {
        type Error = russh::Error;

        fn auth_none(
            &mut self,
            _: &str,
        ) -> impl std::future::Future<Output = Result<russh::server::Auth, Self::Error>> + Send
        {
            std::future::ready(Ok(russh::server::Auth::Accept))
        }

        async fn channel_open_direct_tcpip(
            &mut self,
            _: russh::Channel<russh::server::Msg>,
            host_to_connect: &str,
            _: u32,
            _: &str,
            _: u32,
            reply: russh::server::ChannelOpenHandle,
            _: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            self.asked.lock().unwrap().push(host_to_connect.to_string());
            match self.answer {
                TunnelAnswer::OpensOnly(host) if host == host_to_connect => {
                    reply.accept().await;
                }
                TunnelAnswer::NeverAnswers => self.unanswered.push(reply),
                // Dropping `reply` refuses the tunnel.
                TunnelAnswer::RefusesAfter(delay) => tokio::time::sleep(delay).await,
                TunnelAnswer::OpensOnly(_) | TunnelAnswer::RefusesAll => {}
            }
            Ok(())
        }
    }

    const OPENED_WITHIN: std::time::Duration = std::time::Duration::from_secs(5);

    async fn tunnel_test_bastion(
        answer: TunnelAnswer,
    ) -> (
        client::Handle<TestClient>,
        Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let server = TunnelServer {
            asked: Arc::clone(&asked),
            answer,
            unanswered: Vec::new(),
        };
        let (bastion, _ended) = test_connection_to(server).await;
        (bastion, asked)
    }

    #[tokio::test]
    async fn tunnel_goes_to_the_checked_addresses_not_the_name() {
        let (bastion, asked) = tunnel_test_bastion(TunnelAnswer::OpensOnly("203.0.113.8")).await;
        let checked = vec![
            "203.0.113.7:22".parse().unwrap(),
            "203.0.113.8:22".parse().unwrap(),
        ];

        open_tunnel(&bastion, "target.example", 22, Some(checked), OPENED_WITHIN)
            .await
            .expect("the second address should open");

        assert_eq!(*asked.lock().unwrap(), ["203.0.113.7", "203.0.113.8"]);
    }

    #[tokio::test]
    async fn tunnel_fails_once_every_checked_address_is_refused() {
        let (bastion, asked) = tunnel_test_bastion(TunnelAnswer::RefusesAll).await;
        let checked = vec!["[2001:db8::7]:22".parse().unwrap()];

        let error = open_tunnel(&bastion, "target.example", 22, Some(checked), OPENED_WITHIN)
            .await
            .unwrap_err();

        // A dropped request is refused as administratively prohibited.
        assert_eq!(
            error,
            "Failed to open tunnel to target.example:22: the jump host does not allow it"
        );
        assert_eq!(*asked.lock().unwrap(), ["2001:db8::7"]);
    }

    #[tokio::test]
    async fn unchecked_tunnel_leaves_the_name_to_the_bastion() {
        let (bastion, asked) = tunnel_test_bastion(TunnelAnswer::OpensOnly("target.example")).await;

        open_tunnel(&bastion, "target.example", 22, None, OPENED_WITHIN)
            .await
            .expect("the name should open");

        assert_eq!(*asked.lock().unwrap(), ["target.example"]);
    }

    #[tokio::test]
    async fn tunnel_attempts_stop_at_the_timeout() {
        let (bastion, asked) = tunnel_test_bastion(TunnelAnswer::NeverAnswers).await;
        let checked = vec![
            "203.0.113.7:22".parse().unwrap(),
            "203.0.113.8:22".parse().unwrap(),
        ];
        let timeout = std::time::Duration::from_millis(300);

        let error = open_tunnel(&bastion, "target.example", 22, Some(checked), timeout)
            .await
            .unwrap_err();

        assert_eq!(error, "Tunnel to target.example:22 timed out");
        // The first attempt used up the time, so the second was never sent.
        assert_eq!(*asked.lock().unwrap(), ["203.0.113.7"]);
    }

    #[tokio::test]
    async fn tunnel_attempts_share_one_timeout() {
        // Each refusal takes 400 ms: the second would end within a 700 ms
        // timeout of its own, but not within the 700 ms the two share.
        let refusal = std::time::Duration::from_millis(400);
        let (bastion, asked) = tunnel_test_bastion(TunnelAnswer::RefusesAfter(refusal)).await;
        let checked = vec![
            "203.0.113.7:22".parse().unwrap(),
            "203.0.113.8:22".parse().unwrap(),
        ];
        let timeout = std::time::Duration::from_millis(700);

        let error = open_tunnel(&bastion, "target.example", 22, Some(checked), timeout)
            .await
            .unwrap_err();

        assert_eq!(error, "Tunnel to target.example:22 timed out");
        assert_eq!(*asked.lock().unwrap(), ["203.0.113.7", "203.0.113.8"]);
    }

    #[tokio::test]
    async fn tunnel_tries_a_limited_number_of_addresses() {
        let (bastion, asked) = tunnel_test_bastion(TunnelAnswer::RefusesAll).await;
        let checked = (1..=20)
            .map(|i| SocketAddr::from(([203, 0, 113, i], 22)))
            .collect();

        open_tunnel(&bastion, "target.example", 22, Some(checked), OPENED_WITHIN)
            .await
            .unwrap_err();

        assert_eq!(asked.lock().unwrap().len(), MAX_TUNNEL_ATTEMPTS);
    }
}
