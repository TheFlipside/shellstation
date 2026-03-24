use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::pty::PtyState;
use crate::ssh::{JumpHop, SshConnectParams, SshState};

/// Spawn a new local PTY session. Returns the session ID.
#[tauri::command]
pub fn pty_spawn(
    app_handle: AppHandle,
    state: State<'_, PtyState>,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    let id = Uuid::new_v4().to_string();
    let mut manager = state.0.lock().map_err(|e| e.to_string())?;
    manager.spawn(&id, cols, rows, app_handle)?;
    Ok(id)
}

/// Write data to a PTY session's stdin.
#[tauri::command]
pub fn pty_write(state: State<'_, PtyState>, id: String, data: String) -> Result<(), String> {
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
            jump_hops: jump_hops.unwrap_or_default(),
        })
        .await?;
    Ok(id)
}

/// Write data to an SSH session's channel.
#[tauri::command]
pub async fn ssh_write(state: State<'_, SshState>, id: String, data: String) -> Result<(), String> {
    let manager = state.0.lock().await;
    manager.write(&id, data.as_bytes()).await
}

/// Resize the PTY on an SSH session.
#[tauri::command]
pub async fn ssh_resize(
    state: State<'_, SshState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let manager = state.0.lock().await;
    manager.resize(&id, cols, rows).await
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
