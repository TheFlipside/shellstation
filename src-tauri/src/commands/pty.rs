use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::pty::PtyState;
use crate::session_logger::SessionLogState;

use super::{validate_dimensions, MAX_WRITE_SIZE};

/// Spawn a new local PTY session. Returns the session ID.
#[tauri::command]
pub fn pty_spawn(
    app_handle: AppHandle,
    state: State<'_, PtyState>,
    logger_state: State<'_, SessionLogState>,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    validate_dimensions(cols, rows)?;
    let id = Uuid::new_v4().to_string();

    // Open session log before spawning so output from the start is captured.
    {
        if let Ok(mut mgr) = logger_state.0.lock() {
            if let Err(e) = mgr.open_log(&id, "local") {
                tracing::warn!("Failed to open session log: {e}");
            }
        }
    }

    let logger = Some(std::sync::Arc::clone(&logger_state.0));
    let mut manager = state.0.lock().map_err(|e| e.to_string())?;
    manager.spawn(&id, cols, rows, app_handle, logger)?;
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
pub fn pty_kill(
    state: State<'_, PtyState>,
    logger_state: State<'_, SessionLogState>,
    id: String,
) -> Result<(), String> {
    let mut manager = state.0.lock().map_err(|e| e.to_string())?;
    manager.kill(&id)?;
    if let Ok(mut mgr) = logger_state.0.lock() {
        mgr.close_log(&id);
    }
    Ok(())
}
