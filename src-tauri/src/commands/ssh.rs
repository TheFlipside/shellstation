use tauri::{AppHandle, State};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::ssh::{JumpHop, SshConnectParams, SshState};

use super::{validate_dimensions, MAX_JUMP_HOPS, MAX_WRITE_SIZE};

/// Connect to a remote host via SSH. Returns the session ID.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn ssh_connect(
    app_handle: AppHandle,
    state: State<'_, SshState>,
    host: String,
    port: u16,
    username: String,
    auth_method: String,
    auth_credential: String,
    cols: u16,
    rows: u16,
    jump_hops: Option<Vec<JumpHop>>,
    restrict_private_ips: Option<bool>,
) -> Result<String, String> {
    validate_dimensions(cols, rows)?;
    let hops = jump_hops.unwrap_or_default();
    if hops.len() > MAX_JUMP_HOPS {
        return Err(format!("Too many jump hops (max {MAX_JUMP_HOPS})"));
    }
    let id = Uuid::new_v4().to_string();
    let mut manager = state.manager.lock().await;
    manager
        .connect(SshConnectParams {
            id: id.clone(),
            host,
            port,
            username,
            auth_method,
            auth_credential: Zeroizing::new(auth_credential),
            cols,
            rows,
            app_handle,
            jump_hops: hops,
            restrict_private_ips: restrict_private_ips.unwrap_or(false),
        })
        .await?;
    Ok(id)
}

/// Write data to an SSH session's channel.
#[tauri::command]
pub async fn ssh_write(state: State<'_, SshState>, id: String, data: String) -> Result<(), String> {
    if data.len() > MAX_WRITE_SIZE {
        return Err("Write data exceeds maximum size".to_string());
    }
    let (handle, channel_id) = {
        let manager = state.manager.lock().await;
        manager.get_write_handle(&id)?
    };
    tokio::spawn(async move {
        let _ = handle
            .data(channel_id, russh::CryptoVec::from_slice(data.as_bytes()))
            .await;
    });
    Ok(())
}

/// Resize the PTY on an SSH session.
#[tauri::command]
pub async fn ssh_resize(
    state: State<'_, SshState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    validate_dimensions(cols, rows)?;
    let resize_tx = {
        let manager = state.manager.lock().await;
        manager.get_resize_sender(&id)?
    };
    resize_tx
        .send(crate::ssh::ResizeRequest {
            cols: u32::from(cols),
            rows: u32::from(rows),
        })
        .await
        .map_err(|_| "Failed to send resize request".to_string())
}

/// Disconnect an SSH session.
#[tauri::command]
pub async fn ssh_disconnect(state: State<'_, SshState>, id: String) -> Result<(), String> {
    let mut manager = state.manager.lock().await;
    manager.disconnect(&id).await
}

/// Respond to a pending host key verification request.
///
/// This intentionally does NOT acquire the main `SshManager` tokio::Mutex.
/// The `connect()` call holds that lock while awaiting the verification
/// response — if we also locked it here we would deadlock.
#[tauri::command]
pub async fn ssh_host_verify_response(
    state: State<'_, SshState>,
    id: String,
    accept: bool,
) -> Result<(), String> {
    let mut senders = state
        .host_verify_senders
        .lock()
        .map_err(|e| format!("Verify sender lock poisoned: {e}"))?;
    let sender = senders
        .remove(&id)
        .ok_or_else(|| format!("No pending verification for session {id}"))?;
    sender
        .send(accept)
        .map_err(|_| "Verification channel closed".to_string())
}
