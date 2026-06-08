use tauri::{AppHandle, State};
use tokio::sync::mpsc::error::TrySendError;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::session_logger::SessionLogState;
use crate::ssh::{establish_ssh_connection, JumpHop, SshConnectParams, SshState};

use super::{validate_dimensions, TerminalReadyState, MAX_JUMP_HOPS, MAX_WRITE_SIZE};

/// Connect to a remote host via SSH. Returns the session `ID`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn ssh_connect(
    app_handle: AppHandle,
    state: State<'_, SshState>,
    ready_state: State<'_, TerminalReadyState>,
    logger_state: State<'_, SessionLogState>,
    host: String,
    port: u16,
    username: String,
    auth_method: String,
    auth_credential: String,
    cols: u16,
    rows: u16,
    jump_hops: Option<Vec<JumpHop>>,
    restrict_private_ips: Option<bool>,
    connect_timeout: Option<u64>,
    keepalive_interval: Option<u64>,
    keepalive_max: Option<u32>,
) -> Result<String, String> {
    validate_dimensions(cols, rows)?;
    let hops = jump_hops.unwrap_or_default();
    if hops.len() > MAX_JUMP_HOPS {
        return Err(format!("Too many jump hops (max {MAX_JUMP_HOPS})"));
    }
    let id = Uuid::new_v4().to_string();

    // Brief lock: check capacity only.
    {
        let manager = state.manager.lock().await;
        manager.check_capacity()?;
    }

    let logger = Some(std::sync::Arc::clone(&logger_state.0));

    // Open the log file before connecting so output from the start is captured.
    {
        let session_name = format!("{username}@{host}");
        if let Ok(mut mgr) = logger_state.0.lock() {
            if let Err(e) = mgr.open_log(&id, &session_name) {
                tracing::warn!("Failed to open session log: {e}");
            }
        }
    }

    // Create ready signal so the reader task waits for the frontend listener.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    {
        let mut senders = ready_state
            .0
            .lock()
            .map_err(|e| format!("Ready sender lock poisoned: {e}"))?;
        senders.insert(
            id.clone(),
            Box::new(move || {
                let _ = ready_tx.send(());
            }),
        );
    }

    // Establish connection WITHOUT holding the manager lock so other
    // sessions remain fully usable during the (potentially slow) handshake.
    let session = match establish_ssh_connection(
        SshConnectParams {
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
            legacy_algorithms: false,
            restrict_private_ips: restrict_private_ips.unwrap_or(false),
            connect_timeout_secs: connect_timeout.unwrap_or(10),
            keepalive_interval_secs: keepalive_interval.unwrap_or(15),
            keepalive_max: keepalive_max.unwrap_or(3) as usize,
            logger,
            kbd_interactive_senders: std::sync::Arc::clone(&state.kbd_interactive_senders),
            ready_rx,
            login_sequence: None,
        },
        &state.host_verify_senders,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            // Clean up ready sender on connection failure.
            if let Ok(mut senders) = ready_state.0.lock() {
                senders.remove(&id);
            }
            if let Ok(mut mgr) = logger_state.0.lock() {
                mgr.close_log(&id);
            }
            return Err(e);
        }
    };

    // Brief lock: re-check capacity and register atomically.
    {
        let mut manager = state.manager.lock().await;
        manager.register_session(id.clone(), session)?;
    }

    Ok(id)
}

/// Write data to an SSH session's channel.
#[tauri::command]
pub async fn ssh_write(state: State<'_, SshState>, id: String, data: String) -> Result<(), String> {
    if data.len() > MAX_WRITE_SIZE {
        return Err("Write data exceeds maximum size".to_string());
    }
    let (handle, channel_id, write_sem) = {
        let manager = state.manager.lock().await;
        let (h, cid) = manager.get_write_handle(&id)?;
        (h, cid, manager.get_write_semaphore(&id)?)
    };
    // Bound the number of in-flight write tasks. russh's `handle.data` posts
    // onto an internal bounded channel; if the SSH socket is stalled and that
    // channel back-pressures, a sustained IPC flood would otherwise spawn an
    // unbounded number of parked tasks each owning up to `MAX_WRITE_SIZE`
    // bytes — turning a worker-stall DoS into a heap-exhaustion DoS. Reject
    // when too many are already pending; the frontend should back off.
    let Ok(permit) = write_sem.try_acquire_owned() else {
        return Err("Too many pending writes for this session".to_string());
    };
    tokio::spawn(async move {
        let _permit = permit; // released when the task ends
        if let Err(e) = handle.data(channel_id, data.into_bytes()).await {
            tracing::warn!("ssh_write: failed to send data: {e:?}");
        }
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
    // try_send (never block) — keeps a misbehaving frontend (resize flood,
    // CSS-animation feedback loop, etc.) from parking Tauri command workers
    // on a full queue. The mpsc capacity is 4; if it fills, drop this
    // request. The reader task drains in microseconds under normal use, so
    // the *user's* final intended size still reaches the backend.
    if let Err(TrySendError::Closed(_)) = resize_tx.try_send(crate::ssh::ResizeRequest {
        cols: u32::from(cols),
        rows: u32::from(rows),
    }) {
        return Err("Resize channel closed".to_string());
    }
    Ok(())
}

/// Disconnect an SSH session.
#[tauri::command]
pub async fn ssh_disconnect(
    state: State<'_, SshState>,
    logger_state: State<'_, SessionLogState>,
    id: String,
) -> Result<(), String> {
    let mut manager = state.manager.lock().await;
    manager.disconnect(&id).await?;
    if let Ok(mut mgr) = logger_state.0.lock() {
        mgr.close_log(&id);
    }
    Ok(())
}

/// Respond to a pending keyboard-interactive authentication prompt.
///
/// Same deadlock-avoidance pattern as `ssh_host_verify_response`: uses
/// `std::sync::Mutex` instead of the manager's `tokio::Mutex`.
#[tauri::command]
pub async fn ssh_kbd_interactive_response(
    state: State<'_, SshState>,
    id: String,
    responses: Vec<String>,
) -> Result<(), String> {
    let mut senders = state
        .kbd_interactive_senders
        .lock()
        .map_err(|e| format!("Kbd-interactive sender lock poisoned: {e}"))?;
    let sender = senders
        .remove(&id)
        .ok_or_else(|| format!("No pending kbd-interactive prompt for session {id}"))?;
    sender
        .send(Zeroizing::new(responses))
        .map_err(|_| "Kbd-interactive channel closed".to_string())
}

/// Respond to a pending host key verification request.
///
/// This intentionally does `NOT` acquire the main `SshManager` `tokio::Mutex`.
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
