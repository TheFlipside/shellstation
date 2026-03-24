use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::pty::PtyState;
use crate::ssh::{JumpHop, SshConnectParams, SshState};

/// Maximum terminal dimensions to prevent resource exhaustion.
const MAX_COLS: u16 = 500;
const MAX_ROWS: u16 = 500;

/// Maximum bytes per single write call.
const MAX_WRITE_SIZE: usize = 65536;

/// Maximum number of jump host hops.
const MAX_JUMP_HOPS: usize = 10;

fn validate_dimensions(cols: u16, rows: u16) -> Result<(), String> {
    if cols == 0 || rows == 0 || cols > MAX_COLS || rows > MAX_ROWS {
        return Err(format!(
            "Invalid terminal dimensions: cols and rows must be 1–{MAX_COLS}/{MAX_ROWS}"
        ));
    }
    Ok(())
}

/// Spawn a new local PTY session. Returns the session ID.
#[tauri::command]
pub fn pty_spawn(
    app_handle: AppHandle,
    state: State<'_, PtyState>,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    validate_dimensions(cols, rows)?;
    let id = Uuid::new_v4().to_string();
    let mut manager = state.0.lock().map_err(|e| e.to_string())?;
    manager.spawn(&id, cols, rows, app_handle)?;
    Ok(id)
}

/// Write data to a PTY session's stdin.
#[tauri::command]
pub fn pty_write(state: State<'_, PtyState>, id: String, data: String) -> Result<(), String> {
    if data.len() > MAX_WRITE_SIZE {
        return Err("Write data exceeds maximum size".to_string());
    }
    let mut manager = state.0.lock().map_err(|e| e.to_string())?;
    manager.write(&id, data.as_bytes())
}

/// Resize a PTY session.
#[tauri::command]
pub fn pty_resize(
    state: State<'_, PtyState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    validate_dimensions(cols, rows)?;
    let mut manager = state.0.lock().map_err(|e| e.to_string())?;
    manager.resize(&id, cols, rows)
}

/// Kill a PTY session and free resources.
#[tauri::command]
pub fn pty_kill(state: State<'_, PtyState>, id: String) -> Result<(), String> {
    let mut manager = state.0.lock().map_err(|e| e.to_string())?;
    manager.kill(&id)
}

// ── SSH commands ──────────────────────────────────────────────────────

/// Connect to a remote host via SSH. Returns the session ID.
#[tauri::command]
// Each parameter maps to a JSON field from the frontend invoke() call;
// Tauri requires them as individual arguments rather than a struct.
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
    // Extract handle + channel_id under the lock, then release it before the
    // async network write.  Holding the mutex across handle.data().await was
    // blocking all other SSH operations for the duration of each round-trip,
    // causing visible input lag.
    let (handle, channel_id) = {
        let manager = state.0.lock().await;
        manager.get_write_handle(&id)?
    };
    // Fire-and-forget: spawn the actual network write so the IPC response
    // returns immediately.  Waiting for handle.data() here would block the
    // Tauri command handler for the full SSH round-trip, adding visible
    // latency to every keystroke.
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
