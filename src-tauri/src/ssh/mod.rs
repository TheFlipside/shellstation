use std::collections::HashMap;
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

/// Payload emitted to the frontend when the server key needs user verification.
#[derive(Clone, Serialize)]
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
    pub auth_credential: Zeroizing<String>,
    pub cols: u16,
    pub rows: u16,
    pub app_handle: AppHandle,
    pub jump_hops: Vec<JumpHop>,
}

/// Manages all active SSH sessions.
#[derive(Default)]
pub struct SshManager {
    sessions: HashMap<String, SshSession>,
    /// Pending TOFU verification senders, keyed by session ID.
    host_verify_senders: HashMap<String, oneshot::Sender<bool>>,
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
        } = params;

        info!(session_id = %id, host = %host, port = %port, hops = jump_hops.len(), "Connecting via SSH");

        let (mut handle, bastion_handles) = connect_with_hops(
            &id,
            &host,
            port,
            &jump_hops,
            &app_handle,
            &mut self.host_verify_senders,
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

    /// Resolve a pending host key verification request from the frontend.
    pub fn host_verify_response(&mut self, id: &str, accept: bool) -> Result<(), String> {
        let sender = self
            .host_verify_senders
            .remove(id)
            .ok_or_else(|| format!("No pending verification for session {id}"))?;
        sender
            .send(accept)
            .map_err(|_| "Verification channel closed".to_string())
    }
}

/// Thread-safe wrapper around `SshManager` for use as Tauri managed state.
pub struct SshState(pub Arc<Mutex<SshManager>>);

impl Default for SshState {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(SshManager::default())))
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

/// Validate that a key file path resolves to a location within the user's home directory.
/// Prevents path traversal attacks (e.g. `../../etc/shadow`).
fn validate_key_path(path: &str) -> Result<String, String> {
    let home = dirs::home_dir().ok_or("Cannot determine home directory")?;
    let canonical =
        std::fs::canonicalize(path).map_err(|e| format!("Key file not accessible: {e}"))?;
    if !canonical.starts_with(&home) {
        return Err("Key path must be within your home directory".to_string());
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

/// Build a default SSH client config.
fn ssh_config() -> Arc<client::Config> {
    Arc::new(client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(15)),
        keepalive_max: 3,
        ..Default::default()
    })
}

/// Establish an SSH connection, optionally through a chain of jump hosts.
///
/// Returns the final `Handle` to the target host, plus a list of intermediate
/// bastion handles that must be kept alive for the tunnel to remain open.
async fn connect_with_hops(
    session_id: &str,
    target_host: &str,
    target_port: u16,
    jump_hops: &[JumpHop],
    app_handle: &AppHandle,
    verify_senders: &mut HashMap<String, oneshot::Sender<bool>>,
) -> Result<(client::Handle<SshHandler>, Vec<client::Handle<SshHandler>>), String> {
    if jump_hops.is_empty() {
        // Direct connection — no jump hosts.
        let config = ssh_config();
        let (verify_tx, verify_rx) = oneshot::channel::<bool>();
        verify_senders.insert(session_id.to_string(), verify_tx);

        let handler = SshHandler {
            session_id: session_id.to_string(),
            host: target_host.to_string(),
            port: target_port,
            app_handle: app_handle.clone(),
            verify_rx: Arc::new(Mutex::new(Some(verify_rx))),
        };

        let handle = client::connect(config, (target_host, target_port), handler)
            .await
            .map_err(|e| {
                error!(host = %target_host, port = %target_port, error = %e, "SSH connection failed");
                format!("SSH connection failed: {e}")
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
    verify_senders.insert(hop_id.clone(), verify_tx);

    let handler = SshHandler {
        session_id: hop_id,
        host: first_hop.host.clone(),
        port: first_hop.port,
        app_handle: app_handle.clone(),
        verify_rx: Arc::new(Mutex::new(Some(verify_rx))),
    };

    let mut current_handle =
        client::connect(config, (first_hop.host.as_str(), first_hop.port), handler)
            .await
            .map_err(|e| {
                format!(
                    "SSH hop to {}:{} failed: {e}",
                    first_hop.host, first_hop.port
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
        verify_senders.insert(hop_id.clone(), verify_tx);

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
            .map_err(|e| format!("SSH hop to {}:{} failed: {e}", hop.host, hop.port))?;

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
    verify_senders.insert(session_id.to_string(), verify_tx);

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
        .map_err(|e| format!("SSH connection to {target_host}:{target_port} failed: {e}"))?;

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
            if let Err(e) = russh::keys::learn_known_hosts_path(
                &self.host,
                self.port,
                server_public_key,
                &kh_path,
            ) {
                warn!(session_id = %self.session_id, error = %e, "Failed to save known host");
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
