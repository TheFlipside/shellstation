use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use russh::keys::key;
use russh::{client, ChannelMsg, Disconnect};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};
use zeroize::Zeroizing;

/// Pending TOFU verification senders, stored separately from `SshManager`
/// so that `ssh_host_verify_response` can resolve a pending verification
/// without acquiring the main `SshManager` lock (which is held by the
/// `connect()` call that is waiting for the verification).
type HostVerifySenders = Arc<std::sync::Mutex<HashMap<String, oneshot::Sender<bool>>>>;

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

/// Resize request sent to the reader task which owns the channel.
pub struct ResizeRequest {
    pub cols: u32,
    pub rows: u32,
}

/// Per-connection SSH session state.
struct SshSession {
    handle: Arc<client::Handle<SshHandler>>,
    channel_id: russh::ChannelId,
    reader_task: JoinHandle<()>,
    resize_tx: mpsc::Sender<ResizeRequest>,
    /// Bastion handles kept alive for jump host chains.
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
    pub auth_credential: Zeroizing<String>,
    pub cols: u16,
    pub rows: u16,
    pub app_handle: AppHandle,
    pub jump_hops: Vec<JumpHop>,
}

/// Manages all active SSH sessions.
pub struct SshManager {
    sessions: HashMap<String, SshSession>,
    /// Pending TOFU verification senders — behind a separate std::sync::Mutex
    /// so the verify response command can resolve without the main manager lock.
    host_verify_senders: HostVerifySenders,
}

impl Default for SshManager {
    fn default() -> Self {
        Self {
            sessions: HashMap::new(),
            host_verify_senders: Arc::new(std::sync::Mutex::new(HashMap::new())),
        }
    }
}

impl SshManager {
    /// Maximum number of concurrent SSH sessions.
    const MAX_SESSIONS: usize = 100;

    /// Connect to a remote SSH host, authenticate, open a PTY shell, and start
    /// streaming output via Tauri events.
    pub async fn connect(&mut self, params: SshConnectParams) -> Result<(), String> {
        if self.sessions.len() >= Self::MAX_SESSIONS {
            return Err(format!(
                "Session limit reached (max {})",
                Self::MAX_SESSIONS
            ));
        }

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
            restrict_private_ips,
            connect_timeout_secs,
        } = params;

        info!(session_id = %id, host = %host, port = %port, hops = jump_hops.len(), "Connecting via SSH");

        let verify_senders = Arc::clone(&self.host_verify_senders);
        let (mut handle, bastion_handles) = connect_with_hops(
            &id,
            &host,
            port,
            &jump_hops,
            restrict_private_ips,
            connect_timeout_secs,
            &app_handle,
            &verify_senders,
        )
        .await?;

        // Authenticate.
        authenticate_handle(&mut handle, &username, &auth_method, &auth_credential).await?;

        info!(session_id = %id, "SSH authenticated");

        // Open a session channel with PTY.
        let channel = handle
            .channel_open_session()
            .await
            .map_err(|e| format!("Failed to open channel: {e}"))?;

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
                                let _ = app.emit(&exit_event, ());
                                break;
                            }
                            Some(_) => {}
                            None => {
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

        self.sessions.insert(
            id.clone(),
            SshSession {
                handle: Arc::new(handle),
                channel_id,
                reader_task,
                resize_tx,
                bastion_handles,
            },
        );

        info!(session_id = %id, "SSH session established");
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

/// Thread-safe wrapper around `SshManager` for use as Tauri managed state.
pub struct SshState {
    pub manager: Arc<Mutex<SshManager>>,
    /// Exposed separately so `ssh_host_verify_response` can resolve a pending
    /// verification without acquiring the main manager tokio::Mutex (which is
    /// held by the `connect()` call waiting for the verification result).
    pub host_verify_senders: HostVerifySenders,
}

impl Default for SshState {
    fn default() -> Self {
        let senders: HostVerifySenders = Arc::new(std::sync::Mutex::new(HashMap::new()));
        Self {
            manager: Arc::new(Mutex::new(SshManager {
                sessions: HashMap::new(),
                host_verify_senders: Arc::clone(&senders),
            })),
            host_verify_senders: senders,
        }
    }
}

/// Sanitize SSH error messages to avoid leaking internal network topology.
/// The raw error is already logged server-side; user-facing messages are generic.
fn sanitize_ssh_error(raw: &str) -> String {
    let lower = raw.to_lowercase();
    if lower.contains("connection refused") {
        "SSH connection failed: connection refused".to_string()
    } else if lower.contains("timed out") || lower.contains("timeout") {
        "SSH connection failed: connection timed out".to_string()
    } else if lower.contains("no route") || lower.contains("network is unreachable") {
        "SSH connection failed: host unreachable".to_string()
    } else if lower.contains("name or service not known") || lower.contains("resolve") {
        "SSH connection failed: hostname could not be resolved".to_string()
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
    if !canonical.starts_with(&home) {
        return Err("Key path must be within your home directory".to_string());
    }

    // Block known sensitive files that are not SSH keys.
    let rel = canonical
        .strip_prefix(&home)
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
fn known_hosts_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ssh")
        .join("known_hosts")
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
    if username.is_empty() {
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

/// Remove blank lines from a known_hosts file so entries are contiguous,
/// matching the format OpenSSH produces.
fn strip_blank_lines(path: &std::path::Path) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let cleaned: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut out = cleaned.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    let _ = std::fs::write(path, out);
}

/// Build a default SSH client config.
fn ssh_config() -> Arc<client::Config> {
    Arc::new(client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(15)),
        keepalive_max: 3,
        ..Default::default()
    })
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
    app_handle: &AppHandle,
    verify_senders: &HostVerifySenders,
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
        let config = ssh_config();
        let (verify_tx, verify_rx) = oneshot::channel::<bool>();
        verify_senders
            .lock()
            .unwrap()
            .insert(session_id.to_string(), verify_tx);

        let handler = SshHandler {
            session_id: session_id.to_string(),
            host: target_host.to_string(),
            port: target_port,
            app_handle: app_handle.clone(),
            verify_rx: Arc::new(Mutex::new(Some(verify_rx))),
        };

        let handle = tokio::time::timeout(
            std::time::Duration::from_secs(connect_timeout_secs),
            client::connect(config, (target_host, target_port), handler),
        )
        .await
        .map_err(|_| format!("SSH connection failed: connection to {target_host}:{target_port} timed out"))?
        .map_err(|e| {
            error!(host = %target_host, port = %target_port, error = %e, "SSH connection failed");
            sanitize_ssh_error(&e.to_string())
        })?;

        return Ok((handle, Vec::new()));
    }

    // Jump host chain: connect to each hop in order, then tunnel to target.
    let mut bastion_handles: Vec<client::Handle<SshHandler>> = Vec::new();

    // Connect to the first hop directly.
    let first_hop = &jump_hops[0];
    let config = ssh_config();
    let hop_id = format!("{session_id}-hop0");
    let (verify_tx, verify_rx) = oneshot::channel::<bool>();
    verify_senders
        .lock()
        .unwrap()
        .insert(hop_id.clone(), verify_tx);

    let handler = SshHandler {
        session_id: hop_id,
        host: first_hop.host.clone(),
        port: first_hop.port,
        app_handle: app_handle.clone(),
        verify_rx: Arc::new(Mutex::new(Some(verify_rx))),
    };

    let mut current_handle = tokio::time::timeout(
        std::time::Duration::from_secs(connect_timeout_secs),
        client::connect(
            config,
            (first_hop.host.as_str(), first_hop.port),
            handler,
        ),
    )
    .await
    .map_err(|_| format!("SSH jump host connection failed: connection to {}:{} timed out", first_hop.host, first_hop.port))?
    .map_err(|e| {
        error!(host = %first_hop.host, port = %first_hop.port, error = %e, "SSH hop failed");
        format!(
            "SSH jump host connection failed: {}",
            sanitize_ssh_error(&e.to_string())
        )
    })?;

    // Authenticate the first hop.
    authenticate_handle(
        &mut current_handle,
        &first_hop.username,
        &first_hop.auth_method,
        &first_hop.auth_credential,
    )
    .await?;

    info!(hop = 0, host = %first_hop.host, "Jump host authenticated");

    // Chain through remaining hops (if any).
    for (i, hop) in jump_hops.iter().enumerate().skip(1) {
        let tunnel = current_handle
            .channel_open_direct_tcpip(&hop.host, hop.port.into(), "127.0.0.1", 0)
            .await
            .map_err(|e| format!("Failed to open tunnel to {}:{}: {e}", hop.host, hop.port))?;

        let stream = tunnel.into_stream();
        let config = ssh_config();
        let hop_id = format!("{session_id}-hop{i}");
        let (verify_tx, verify_rx) = oneshot::channel::<bool>();
        verify_senders
            .lock()
            .unwrap()
            .insert(hop_id.clone(), verify_tx);

        let handler = SshHandler {
            session_id: hop_id,
            host: hop.host.clone(),
            port: hop.port,
            app_handle: app_handle.clone(),
            verify_rx: Arc::new(Mutex::new(Some(verify_rx))),
        };

        bastion_handles.push(current_handle);

        current_handle = client::connect_stream(config, stream, handler)
            .await
            .map_err(|e| {
                error!(host = %hop.host, port = %hop.port, error = %e, "SSH hop failed");
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
        )
        .await?;

        info!(hop = i, host = %hop.host, "Jump host authenticated");
    }

    // Finally, tunnel from the last hop to the target.
    let tunnel = current_handle
        .channel_open_direct_tcpip(target_host, target_port.into(), "127.0.0.1", 0)
        .await
        .map_err(|e| format!("Failed to open tunnel to {target_host}:{target_port}: {e}"))?;

    let stream = tunnel.into_stream();
    let config = ssh_config();
    let (verify_tx, verify_rx) = oneshot::channel::<bool>();
    verify_senders
        .lock()
        .unwrap()
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
            error!(host = %target_host, port = %target_port, error = %e, "SSH target connection failed");
            sanitize_ssh_error(&e.to_string())
        })?;

    Ok((target_handle, bastion_handles))
}

/// Authenticate an SSH handle using the specified method.
async fn authenticate_handle(
    handle: &mut client::Handle<SshHandler>,
    username: &str,
    auth_method: &str,
    auth_credential: &str,
) -> Result<(), String> {
    let auth_ok = match auth_method {
        "password" => handle
            .authenticate_password(username, auth_credential)
            .await
            .map_err(|e| format!("Auth failed: {e}"))?,
        "publickey" => {
            let (key_path, passphrase) = parse_key_credential(auth_credential)?;
            let key_pair =
                russh::keys::load_secret_key(&key_path, passphrase.as_ref().map(|s| s.as_str()))
                    .map_err(|e| format!("Failed to load SSH key: {e}"))?;
            handle
                .authenticate_publickey(username, Arc::new(key_pair))
                .await
                .map_err(|e| format!("Auth failed: {e}"))?
        }
        other => return Err(format!("Unsupported auth method: {other}")),
    };

    if !auth_ok {
        return Err("Authentication rejected by server".to_string());
    }

    Ok(())
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

#[async_trait]
impl client::Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let kh_path = known_hosts_path();

        // Check if we already know this host.
        match russh::keys::check_known_hosts_path(
            &self.host,
            self.port,
            server_public_key,
            &kh_path,
        ) {
            Ok(true) => return Ok(true),
            Ok(false) => {
                warn!(
                    session_id = %self.session_id,
                    host = %self.host,
                    "Server key CHANGED — possible MITM"
                );
            }
            Err(_) => {
                // Key not in known_hosts or file doesn't exist yet.
            }
        }

        // Emit verification request to the frontend.
        let payload = HostVerifyPayload {
            session_id: self.session_id.clone(),
            host: self.host.clone(),
            port: self.port,
            fingerprint: server_public_key.fingerprint(),
            key_type: server_public_key.name().to_string(),
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
            // Ensure ~/.ssh dir and known_hosts file exist with correct
            // permissions (0o700 / 0o600) before russh writes to them.
            ensure_known_hosts_file(&kh_path);

            if let Err(e) = russh::keys::learn_known_hosts_path(
                &self.host,
                self.port,
                server_public_key,
                &kh_path,
            ) {
                warn!(session_id = %self.session_id, error = %e, "Failed to save known host");
            }

            // russh inserts blank lines between entries; strip them to match
            // the format OpenSSH produces.
            strip_blank_lines(&kh_path);
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
