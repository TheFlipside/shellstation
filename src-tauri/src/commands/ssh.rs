use tauri::{AppHandle, State};
use uuid::Uuid;

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
) -> Result<String, String> {
    validate_dimensions(cols, rows)?;
    let hops = jump_hops.unwrap_or_default();
    if hops.len() > MAX_JUMP_HOPS {
        return Err(format!("Too many jump hops (max {MAX_JUMP_HOPS})"));
    }
    let id = Uuid::new_v4().to_string();
    let mut manager = state.0.lock().await;
    manager
        .connect(SshConnectParams {
            id: id.clone(),
            host,
            port,
            username,
            auth_method,
            auth_credential,
            cols,
            rows,
            app_handle,
            jump_hops: hops,
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
        let manager = state.0.lock().await;
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
        let manager = state.0.lock().await;
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
    let mut manager = state.0.lock().await;
    manager.disconnect(&id).await
}

/// Respond to a pending host key verification request.
#[tauri::command]
pub async fn ssh_host_verify_response(
    state: State<'_, SshState>,
    id: String,
    accept: bool,
) -> Result<(), String> {
    let mut manager = state.0.lock().await;
    manager.host_verify_response(&id, accept)
}
