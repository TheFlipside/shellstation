use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::telnet::{TelnetConnectParams, TelnetState};

use super::{validate_dimensions, MAX_WRITE_SIZE};

/// Connect to a remote host via Telnet. Returns the session ID.
#[tauri::command]
pub async fn telnet_connect(
    app_handle: AppHandle,
    state: State<'_, TelnetState>,
    host: String,
    port: u16,
    cols: u16,
    rows: u16,
    restrict_private_ips: Option<bool>,
) -> Result<String, String> {
    validate_dimensions(cols, rows)?;
    let id = Uuid::new_v4().to_string();
    let mut manager = state.0.lock().await;
    manager
        .connect(TelnetConnectParams {
            id: id.clone(),
            host,
            port,
            cols,
            rows,
            app_handle,
            restrict_private_ips: restrict_private_ips.unwrap_or(false),
        })
        .await?;
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
pub async fn telnet_disconnect(state: State<'_, TelnetState>, id: String) -> Result<(), String> {
    let mut manager = state.0.lock().await;
    manager.disconnect(&id).await
}
