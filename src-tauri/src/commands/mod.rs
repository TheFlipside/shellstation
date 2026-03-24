use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::pty::PtyState;

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
