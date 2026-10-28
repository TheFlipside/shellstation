use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::pty::PtyState;
use crate::session_logger::SessionLogState;

use super::{
    lock_or_recover, release_session, validate_dimensions, ConnectSetup, TerminalReadyState,
    MAX_WRITE_SIZE,
};

/// Spawn a new local PTY session. Returns the session `ID`.
#[tauri::command]
pub fn pty_spawn(
    app_handle: AppHandle,
    state: State<'_, PtyState>,
    ready_state: State<'_, TerminalReadyState>,
    logger_state: State<'_, SessionLogState>,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    let page_load = ready_state.page_load();
    validate_dimensions(cols, rows)?;
    let id = Uuid::new_v4().to_string();

    let setup = ConnectSetup::begin(&id, "local", page_load, &ready_state, &logger_state);

    // Create ready signal so the reader thread waits for the frontend listener.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    setup.add_ready_signal(move || {
        let _ = ready_tx.send(());
    });

    let logger = Some(std::sync::Arc::clone(&logger_state.0));
    {
        let mut manager = state.0.lock().map_err(|e| e.to_string())?;
        setup.check_page_unchanged()?;
        manager.spawn(&id, cols, rows, app_handle, logger, ready_rx)?;
    }
    setup.disarm();
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
pub async fn pty_kill(
    state: State<'_, PtyState>,
    ready_state: State<'_, TerminalReadyState>,
    logger_state: State<'_, SessionLogState>,
    id: String,
) -> Result<(), String> {
    // The manager lock is a blocking one, which a stuck write to a shell can
    // hold, and killing waits for the shell to exit: do both on a blocking
    // thread, off the async workers. The kill comes before the release, so
    // the shell's reader cannot start reading on the way out.
    let manager = std::sync::Arc::clone(&state.0);
    let kill_id = id.clone();
    let killed = tauri::async_runtime::spawn_blocking(move || {
        let shell = lock_or_recover(&manager).remove(&kill_id)?;
        shell.shut_down(&kill_id);
        Ok::<_, String>(())
    })
    .await;
    match killed {
        // Not a session of this manager: nothing of it to release.
        Ok(Err(not_found)) => Err(not_found),
        Ok(Ok(())) => {
            release_session(&id, &ready_state, &logger_state);
            Ok(())
        }
        // Removed, then the kill panicked: release it all the same.
        Err(e) => {
            release_session(&id, &ready_state, &logger_state);
            Err(format!("Failed to stop the shell: {e}"))
        }
    }
}
