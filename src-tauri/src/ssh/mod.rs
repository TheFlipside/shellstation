use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use russh::client::KeyboardInteractiveAuthResponse;
use russh::keys::{PrivateKeyWithHashAlg, PublicKey};
use russh::{client, ChannelMsg, Disconnect};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot, Mutex};
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
    Arc<std::sync::Mutex<HashMap<String, oneshot::Sender<Vec<String>>>>>;

/// Payload emitted to the frontend when the server key needs user verification.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostVerifyPayload {
    pub session_id: String,
    pub host: String,
    pub port: u16,
    pub fingerprint: String,
    pub key_type: String,
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
pub struct ResizeRequest {
    pub cols: u32,
    pub rows: u32,
}

/// Per-connection SSH session state.
pub(crate) struct SshSession {
    handle: Arc<client::Handle<SshHandler>>,
    channel_id: russh::ChannelId,
    reader_task: JoinHandle<()>,
    resize_tx: mpsc::Sender<ResizeRequest>,
    /// Bastion handles kept alive for jump host chains. Must be retained for
    /// their Drop impl, which closes each hop's connection in reverse order
    /// when the session ends.
    #[allow(dead_code)]
    bastion_handles: Vec<client::Handle<SshHandler>>,
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
    pub legacy_algorithms: bool,
    pub logger: Option<std::sync::Arc<std::sync::Mutex<crate::session_logger::SessionLogManager>>>,
    pub kbd_interactive_senders: KbdInteractiveSenders,
}

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
    pub fn register_session(&mut self, id: String, session: SshSession) -> Result<(), String> {
        self.check_capacity()?;
        self.sessions.insert(id, session);
        Ok(())
    }

    /// Get the handle and channel ID for writing to a session.
    /// The caller should drop the manager lock before performing the async write.
    pub fn get_write_handle(
        &self,
        id: &str,
    ) -> Result<(Arc<client::Handle<SshHandler>>, russh::ChannelId), String> {
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| format!("SSH session {id} not found"))?;
        Ok((Arc::clone(&session.handle), session.channel_id))
    }

    /// Get the resize sender for a session.
    /// The caller should drop the manager lock before sending.
    pub fn get_resize_sender(&self, id: &str) -> Result<mpsc::Sender<ResizeRequest>, String> {
        let session = self
            .sessions
            .get(id)
            .ok_or_else(|| format!("SSH session {id} not found"))?;
        Ok(session.resize_tx.clone())
    }

    /// Disconnect an SSH session and clean up resources.
    pub async fn disconnect(&mut self, id: &str) -> Result<(), String> {
        let session = self
            .sessions
            .remove(id)
            .ok_or_else(|| format!("SSH session {id} not found"))?;
        session.reader_task.abort();
        let _ = session
            .handle
            .disconnect(Disconnect::ByApplication, "", "en")
            .await;
        info!(session_id = %id, "SSH session disconnected");
        Ok(())
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
    } = params;

    info!(session_id = %id, host = %host, port = %port, hops = jump_hops.len(), legacy_algorithms, "Connecting via SSH");

    let (mut handle, bastion_handles) = connect_with_hops(
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

    // Authenticate.
    authenticate_handle(
        &mut handle,
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
    let channel = handle
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

    let channel_id = channel.id();

    // Create a channel for sending resize requests to the reader task.
    let (resize_tx, mut resize_rx) = mpsc::channel::<ResizeRequest>(4);

    // Spawn a reader task that owns the channel exclusively.
    // It streams output to the frontend and handles resize requests.
    let event_name = format!("terminal-output-{id}");
    let exit_event = format!("terminal-exit-{id}");
    let session_id = id.clone();
    let app = app_handle;

    let reader_task = tokio::spawn(async move {
        let mut channel = channel;
        loop {
            tokio::select! {
                msg = channel.wait() => {
                    match msg {
                        Some(ChannelMsg::Data { ref data })
                        | Some(ChannelMsg::ExtendedData { ref data, ext: 1 }) => {
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
                Some(req) = resize_rx.recv() => {
                    if let Err(e) = channel.window_change(req.cols, req.rows, 0, 0).await {
                        warn!(session_id = %session_id, error = %e, "Failed to resize SSH PTY");
                    }
                }
            }
        }
    });

    info!(session_id = %id, "SSH session established");

    Ok(SshSession {
        handle: Arc::new(handle),
        channel_id,
        reader_task,
        resize_tx,
        bastion_handles,
    })
}

/// Thread-safe wrapper around `SshManager` for use as Tauri managed state.
pub struct SshState {
    pub manager: Arc<Mutex<SshManager>>,
    /// Exposed separately so `ssh_host_verify_response` can resolve a pending
    /// verification without acquiring the main manager tokio::Mutex (which is
    /// held by the `connect()` call waiting for the verification result).
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

/// Parse a public key credential string. Format: "key_path" or "key_path\npassphrase".
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

/// Return the path to the user's known_hosts file.
fn known_hosts_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir()
        .ok_or_else(|| "Could not determine home directory for known_hosts".to_string())?;
    Ok(home.join(".ssh").join("known_hosts"))
}

/// Ensure the `~/.ssh` directory (mode 700) and `known_hosts` file (mode 600)
/// exist before russh tries to write, so the file is created with the
/// permissions that OpenSSH expects.
fn ensure_known_hosts_file(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                warn!(path = %parent.display(), error = %e, "Failed to create .ssh directory");
                return;
            }
        }
        // Always enforce correct permissions on the .ssh directory.
        set_restricted_dir_permissions(parent);
    }
    if !path.exists() {
        if let Err(e) = std::fs::File::create(path) {
            warn!(path = %path.display(), error = %e, "Failed to create known_hosts file");
            return;
        }
    }
    // Always enforce correct permissions on the known_hosts file.
    set_restricted_file_permissions(path);
}

/// Set directory permissions to 0700 (Unix) or restricted ACL (Windows).
fn set_restricted_dir_permissions(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
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
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(windows)]
    {
        restrict_windows_acl(path, false);
    }
}

/// On Windows, use icacls to remove inherited permissions and grant full
/// control only to the current user.
#[cfg(windows)]
fn restrict_windows_acl(path: &std::path::Path, is_dir: bool) {
    let path_str = path.to_string_lossy().to_string();
    let username = std::env::var("USERNAME").unwrap_or_default();
    if username.is_empty()
        || !username
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == ' ')
    {
        return;
    }
    let grant = if is_dir {
        format!("{username}:(OI)(CI)F")
    } else {
        format!("{username}:F")
    };
    let _ = std::process::Command::new("icacls")
        .args([&path_str, "/inheritance:r", "/grant:r", &grant])
        .output();
}

/// Remove all existing known_hosts entries for the given host+port,
/// then strip blank lines.  This ensures that `learn_known_hosts_path`
/// (which only appends) does not create duplicates and that a changed
/// server key cleanly replaces the old entry.
fn remove_known_host_entries(path: &std::path::Path, host: &str, port: u16) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };

    let host_port = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };

    let kept: Vec<&str> = content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return true;
            }
            // The first space-separated field is the host pattern.
            let entry_host = trimmed.split(' ').next().unwrap_or("");
            // An entry can list multiple hosts separated by commas.
            // Remove the entry only if the host matches.
            !entry_host.split(',').any(|h| h == host_port)
        })
        .collect();

    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    let _ = std::fs::write(path, out);
}

/// Build an SSH client config with the given keepalive parameters.
///
/// TCP_NODELAY is enabled unconditionally: interactive terminal sessions
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
    if legacy_algorithms {
        config.preferred = legacy_preferred();
    }
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
        kex = %join_names(pref.kex.iter().map(|n| n.as_ref())),
        key = %join_names(pref.key.iter().map(|a| a.as_str())),
        cipher = %join_names(pref.cipher.iter().map(|n| n.as_ref())),
        mac = %join_names(pref.mac.iter().map(|n| n.as_ref())),
        compression = %join_names(pref.compression.iter().map(|n| n.as_ref())),
        "SSH algorithm proposal"
    );
}

/// Build a `Preferred` algorithm set that appends legacy kex, ciphers and
/// MACs to the modern defaults. Secure algorithms remain first in the list so
/// a legacy-capable switch won't downgrade a modern server, but old gear that
/// only speaks group14-sha1 / aes-cbc / hmac-sha1 can now be reached.
fn legacy_preferred() -> russh::Preferred {
    use std::borrow::Cow;
    let base = russh::Preferred::DEFAULT;

    let mut kex: Vec<russh::kex::Name> = base.kex.iter().copied().collect();
    for extra in [
        russh::kex::DH_GEX_SHA1,
        russh::kex::DH_G14_SHA1,
        russh::kex::DH_G1_SHA1,
    ] {
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
    }
}

/// Check whether an IP address belongs to a restricted (non-routable) range.
fn is_restricted_ip(addr: &IpAddr) -> bool {
    match addr {
        IpAddr::V4(ip) => {
            ip.is_loopback()                                     // 127.0.0.0/8
                || ip.is_link_local()                            // 169.254.0.0/16
                || ip.octets()[0] == 10                          // 10.0.0.0/8
                || (ip.octets()[0] == 172
                    && (16..=31).contains(&ip.octets()[1]))      // 172.16.0.0/12
                || (ip.octets()[0] == 192 && ip.octets()[1] == 168) // 192.168.0.0/16
                || ip.is_unspecified()                           // 0.0.0.0
                || ip.is_broadcast()                             // 255.255.255.255
                || *ip == Ipv4Addr::new(100, 64, 0, 0)          // shared address (100.64/10 start)
                || (ip.octets()[0] == 100
                    && (64..=127).contains(&ip.octets()[1])) // 100.64.0.0/10 (CGNAT)
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()                                     // ::1
                || ip.is_unspecified()                           // ::
                || *ip == Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0) // simplified link-local check
                || ip.segments()[0] == 0xfe80                    // fe80::/10
                || ip.segments()[0] == 0xfc00                    // fc00::/7 (ULA)
                || ip.segments()[0] == 0xfd00 // fd00::/8 (ULA)
        }
    }
}

/// Validate that a hostname does not resolve to a restricted IP range.
/// This prevents SSRF attacks through tunnels targeting internal services.
///
/// Public alias used by other connection modules (e.g. Telnet).
pub fn validate_ssh_target_public(host: &str, port: u16) -> Result<(), String> {
    validate_ssh_target(host, port)
}

/// Validate that a hostname does not resolve to a restricted IP range.
/// This prevents SSRF attacks through SSH tunnels targeting internal services.
fn validate_ssh_target(host: &str, port: u16) -> Result<(), String> {
    // Try parsing as a literal IP first.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_restricted_ip(&ip) {
            return Err(format!(
                "Connection to restricted address {host} is not allowed"
            ));
        }
        return Ok(());
    }

    // Resolve hostname and check all resulting addresses.
    let addrs: Vec<_> = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|e| format!("Cannot resolve host '{host}': {e}"))?
        .collect();

    if addrs.is_empty() {
        return Err(format!("Host '{host}' did not resolve to any address"));
    }

    for addr in &addrs {
        if is_restricted_ip(&addr.ip()) {
            return Err(format!(
                "Host '{host}' resolves to restricted address {} — connection not allowed",
                addr.ip()
            ));
        }
    }

    Ok(())
}

/// Establish an SSH connection, optionally through a chain of jump hosts.
///
/// Returns the final `Handle` to the target host, plus a list of intermediate
/// bastion handles that must be kept alive for the tunnel to remain open.
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
) -> Result<(client::Handle<SshHandler>, Vec<client::Handle<SshHandler>>), String> {
    // Validate all targets against restricted IP ranges (SSRF prevention).
    if restrict_private_ips {
        validate_ssh_target(target_host, target_port)?;
        for hop in jump_hops {
            validate_ssh_target(&hop.host, hop.port)?;
        }
    }

    if jump_hops.is_empty() {
        // Direct connection — no jump hosts.
        debug!(
            session_id,
            host = target_host,
            port = target_port,
            "Direct connection (no jump hosts)"
        );
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
        let handle = tokio::time::timeout(
            std::time::Duration::from_secs(connect_timeout_secs),
            client::connect(config, (target_host, target_port), handler),
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
        return Ok((handle, Vec::new()));
    }

    // Jump host chain: connect to each hop in order, then tunnel to target.
    debug!(
        session_id,
        hops = jump_hops.len(),
        "Starting jump host chain"
    );
    let mut bastion_handles: Vec<client::Handle<SshHandler>> = Vec::new();

    // Connect to the first hop directly.
    let first_hop = &jump_hops[0];
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
    let mut current_handle = tokio::time::timeout(
        std::time::Duration::from_secs(connect_timeout_secs),
        client::connect(config, (first_hop.host.as_str(), first_hop.port), handler),
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
            &mut current_handle,
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
        let tunnel = current_handle
            .channel_open_direct_tcpip(&hop.host, hop.port.into(), "127.0.0.1", 0)
            .await
            .map_err(|e| format!("Failed to open tunnel to {}:{}: {e}", hop.host, hop.port))?;

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

        bastion_handles.push(current_handle);

        current_handle = client::connect_stream(config, stream, handler)
            .await
            .map_err(|e| {
                cleanup_verify_sender(verify_senders, &hop_id);
                error!(host = %hop.host, port = %hop.port, hop = i, error = %e, error_debug = ?e, "SSH hop handshake failed");
                format!(
                    "SSH jump host connection failed: {}",
                    sanitize_ssh_error(&e.to_string())
                )
            })?;

        authenticate_handle(
            &mut current_handle,
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
    let tunnel = current_handle
        .channel_open_direct_tcpip(target_host, target_port.into(), "127.0.0.1", 0)
        .await
        .map_err(|e| format!("Failed to open tunnel to {target_host}:{target_port}: {e}"))?;

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

    bastion_handles.push(current_handle);

    let target_handle = client::connect_stream(config, stream, handler)
        .await
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
    Ok((target_handle, bastion_handles))
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

    let auth_ok = match auth_method {
        "password" => {
            debug!(session_id = %session_id, user = %username, "Attempting password authentication");
            handle
                .authenticate_password(username, auth_credential)
                .await
                .map(|res| {
                    debug!(session_id = %session_id, success = res.success(), "Password auth result");
                    res.success()
                })
                .map_err(|e| {
                    debug!(session_id = %session_id, error = %e, error_debug = ?e, "Password auth error");
                    format!("Auth failed: {e}")
                })?
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
                let (tx, rx) = oneshot::channel::<Vec<String>>();
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
                let responses = tokio::time::timeout(std::time::Duration::from_secs(60), rx)
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

                response = handle
                    .authenticate_keyboard_interactive_respond(responses)
                    .await
                    .map_err(|e| format!("Keyboard-interactive auth failed: {e}"))?;
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
        server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
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
        if let Some(ref kh) = kh_path {
            trace!(
                session_id = %self.session_id,
                known_hosts = %kh.display(),
                host = %self.host,
                port = self.port,
                key_type = server_public_key.algorithm().as_str(),
                "Checking known_hosts"
            );
            match russh::keys::check_known_hosts_path(&self.host, self.port, server_public_key, kh)
            {
                Ok(true) => {
                    trace!(session_id = %self.session_id, "Host key found in known_hosts — trusted");
                    return Ok(true);
                }
                Ok(false) => {
                    // No matching key type found — could be a first-time connection
                    // or a key algorithm change. Fall through to prompt.
                }
                Err(russh::keys::Error::KeyChanged { line }) => {
                    warn!(
                        session_id = %self.session_id,
                        host = %self.host,
                        port = self.port,
                        line,
                        "Server key CHANGED — possible MITM"
                    );
                    // Fall through to prompt — if accepted, the old entry
                    // will be replaced below.
                }
                Err(e) => {
                    debug!(
                        session_id = %self.session_id,
                        host = %self.host,
                        port = self.port,
                        error = %e,
                        "known_hosts check error — treating as unknown host"
                    );
                }
            }
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

        let accepted = match rx {
            Some(rx) => match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
                Ok(Ok(answer)) => answer,
                Ok(Err(_)) => {
                    warn!(session_id = %self.session_id, "Host verification channel dropped");
                    false
                }
                Err(_) => {
                    warn!(session_id = %self.session_id, "Host verification timed out after 60s");
                    false
                }
            },
            None => {
                error!(session_id = %self.session_id, "No verification receiver available");
                false
            }
        };

        if accepted {
            if let Some(ref kh) = kh_path {
                // Ensure ~/.ssh dir and known_hosts file exist with correct
                // permissions (0o700 / 0o600) before russh writes to them.
                ensure_known_hosts_file(kh);

                // Remove any existing entries for this host+port to prevent
                // duplicates and ensure changed keys replace the old entry.
                remove_known_host_entries(kh, &self.host, self.port);

                if let Err(e) = russh::keys::known_hosts::learn_known_hosts_path(
                    &self.host,
                    self.port,
                    server_public_key,
                    kh,
                ) {
                    warn!(session_id = %self.session_id, error = %e, "Failed to save known host");
                }
            } else {
                warn!(session_id = %self.session_id, "Skipping known_hosts save — home directory unknown");
            }
        }

        Ok(accepted)
    }

    async fn data(
        &mut self,
        _channel: russh::ChannelId,
        _data: &[u8],
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        // Data is consumed via channel.wait() in the reader task.
        Ok(())
    }
}
