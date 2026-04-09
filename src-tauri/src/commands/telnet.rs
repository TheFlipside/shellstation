use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::session_logger::SessionLogState;
use crate::telnet::{establish_telnet_connection, TelnetConnectParams, TelnetState};

use super::{validate_dimensions, MAX_WRITE_SIZE};

/// Connect to a remote host via Telnet. Returns the session ID.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn telnet_connect(
    app_handle: AppHandle,
    state: State<'_, TelnetState>,
    logger_state: State<'_, SessionLogState>,
    host: String,
    port: u16,
    cols: u16,
    rows: u16,
    restrict_private_ips: Option<bool>,
    connect_timeout: Option<u64>,
) -> Result<String, String> {
    validate_dimensions(cols, rows)?;
    let id = Uuid::new_v4().to_string();

    // Open session log before connecting so output from the start is captured.
    {
        let session_name = format!("{host}:{port}");
        if let Ok(mut mgr) = logger_state.0.lock() {
            if let Err(e) = mgr.open_log(&id, &session_name) {
                tracing::warn!("Failed to open session log: {e}");
            }
        }
    }

    let logger = Some(std::sync::Arc::clone(&logger_state.0));

    // Brief lock: check capacity only.
    {
        let manager = state.0.lock().await;
        manager.check_capacity()?;
    }

    // Establish connection WITHOUT holding the manager lock so other
    // sessions remain fully usable during the (potentially slow) handshake.
    let session = match establish_telnet_connection(TelnetConnectParams {
        id: id.clone(),
        host,
        port,
        cols,
        rows,
        app_handle,
        restrict_private_ips: restrict_private_ips.unwrap_or(false),
        connect_timeout_secs: connect_timeout.unwrap_or(10),
        logger,
    })
    .await
    {
        Ok(s) => s,
        Err(e) => {
            if let Ok(mut mgr) = logger_state.0.lock() {
                mgr.close_log(&id);
            }
            return Err(e);
        }
    };

    // Brief lock: re-check capacity and register atomically.
    {
        let mut manager = state.0.lock().await;
        manager.register_session(id.clone(), session)?;
    }

    Ok(id)
}

/// Write data to a Telnet session.
#[tauri::command]
pub async fn telnet_write(
    state: State<'_, TelnetState>,
    id: String,
    data: String,
) -> Result<(), String> {
    if data.len() > MAX_WRITE_SIZE {
        return Err("Write data exceeds maximum size".to_string());
    }
    let write_tx = {
        let manager = state.0.lock().await;
        manager.get_write_sender(&id)?
    };
    write_tx
        .send(data.into_bytes())
        .await
        .map_err(|_| "Failed to send data to Telnet session".to_string())
}

/// Resize the terminal window for a Telnet session (sends NAWS).
#[tauri::command]
pub async fn telnet_resize(
    state: State<'_, TelnetState>,
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

/// Disconnect a Telnet session.
#[tauri::command]
pub async fn telnet_disconnect(
    state: State<'_, TelnetState>,
    logger_state: State<'_, SessionLogState>,
    id: String,
) -> Result<(), String> {
    let mut manager = state.0.lock().await;
    manager.disconnect(&id).await?;
    if let Ok(mut mgr) = logger_state.0.lock() {
        mgr.close_log(&id);
    }
    Ok(())
}
