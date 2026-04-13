use std::collections::HashSet;

use tauri::{AppHandle, State};
use uuid::Uuid;

use zeroize::Zeroizing;

use crate::db::models::{
    Credential, CredentialResponse, DataFingerprint, Folder, NewFolder, NewSession, Session,
    UpdateSession,
};
use crate::db::{CredentialDbState, DbState};
use crate::session_logger::SessionLogState;
use crate::ssh::{establish_ssh_connection, SshConnectParams, SshState};
use crate::telnet::{establish_telnet_connection, TelnetConnectParams, TelnetState};

use super::{validate_dimensions, validate_port, validate_session_fields, MAX_JUMP_HOPS};

// ── Folder commands ──────────────────────────────────────────────────

#[tauri::command]
pub async fn folder_create(
    state: State<'_, DbState>,
    name: String,
    parent_id: Option<String>,
) -> Result<Folder, String> {
    if name.is_empty() || name.len() > super::MAX_NAME_LEN {
        return Err(format!(
            "Folder name must be 1–{} characters",
            super::MAX_NAME_LEN
        ));
    }
    let parent = parent_id.map(|s| parse_uuid(&s)).transpose()?;
    state
        .0
        .create_folder(NewFolder {
            name,
            parent_id: parent,
        })
        .await
}

#[tauri::command]
pub async fn folder_list(state: State<'_, DbState>) -> Result<Vec<Folder>, String> {
    state.0.list_folders().await
}

#[tauri::command]
pub async fn folder_rename(
    state: State<'_, DbState>,
    id: String,
    name: String,
) -> Result<(), String> {
    if name.is_empty() || name.len() > super::MAX_NAME_LEN {
        return Err(format!(
            "Folder name must be 1–{} characters",
            super::MAX_NAME_LEN
        ));
    }
    state.0.rename_folder(parse_uuid(&id)?, &name).await
}

#[tauri::command]
pub async fn folder_move(
    state: State<'_, DbState>,
    id: String,
    new_parent_id: Option<String>,
) -> Result<(), String> {
    let parent = new_parent_id.map(|s| parse_uuid(&s)).transpose()?;
    state.0.move_folder(parse_uuid(&id)?, parent).await
}

#[tauri::command]
pub async fn folder_delete(state: State<'_, DbState>, id: String) -> Result<(), String> {
    state.0.delete_folder(parse_uuid(&id)?).await
}

// ── Session commands ─────────────────────────────────────────────────

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn session_create(
    state: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
    folder_id: String,
    name: String,
    hostname: String,
    port: i32,
    username: String,
    protocol: Option<String>,
    auth_method: String,
    tags: String,
    icon: String,
    jump_host_id: Option<String>,
    highlight_profile_id: Option<String>,
    password: Option<String>,
    key_path: Option<String>,
) -> Result<Session, String> {
    validate_port(port)?;
    validate_session_fields(Some(&name), Some(&hostname), Some(&tags))?;
    if username.len() > super::MAX_USERNAME_LEN {
        return Err(format!(
            "Username too long (max {} characters)",
            super::MAX_USERNAME_LEN
        ));
    }
    if icon.len() > super::MAX_ICON_LEN {
        return Err(format!(
            "Icon too long (max {} characters)",
            super::MAX_ICON_LEN
        ));
    }
    let folder = parse_uuid(&folder_id)?;
    let jump = jump_host_id.map(|s| parse_uuid(&s)).transpose()?;

    let effective_protocol = protocol.unwrap_or_else(|| "ssh".to_string());
    if effective_protocol != "ssh" && effective_protocol != "telnet" {
        return Err(format!("Unsupported protocol: {effective_protocol}"));
    }

    let highlight = highlight_profile_id.map(|s| parse_uuid(&s)).transpose()?;

    let session = state
        .0
        .create_session(NewSession {
            folder_id: folder,
            name,
            hostname,
            port,
            protocol: effective_protocol,
            auth_method: auth_method.clone(),
            jump_host_id: jump,
            tags,
            icon,
            highlight_profile_id: highlight,
        })
        .await?;

    // Store credential metadata locally, secret in OS keychain.
    let secret = match auth_method.as_str() {
        "password" => password.unwrap_or_default(),
        "publickey" => key_path.unwrap_or_default(),
        _ => String::new(),
    };
    let keychain_ref = format!("session-{}", session.id);

    if let Err(e) = cred_db
        .0
        .upsert_credential(Credential {
            id: Uuid::new_v4(),
            session_id: session.id,
            username,
            auth_type: auth_method,
            keychain_ref: keychain_ref.clone(),
        })
        .await
    {
        tracing::error!(session_id = %session.id, "credential upsert failed: {e}");
    }

    if !secret.is_empty() {
        if let Err(e) = crate::credentials::store(&keychain_ref, &secret) {
            tracing::error!(session_id = %session.id, "keychain store failed: {e}");
        }
    }

    Ok(session)
}

#[tauri::command]
pub async fn session_get(state: State<'_, DbState>, id: String) -> Result<Option<Session>, String> {
    state.0.get_session(parse_uuid(&id)?).await
}

#[tauri::command]
pub async fn credential_get(
    cred_db: State<'_, CredentialDbState>,
    session_id: String,
) -> Result<Option<CredentialResponse>, String> {
    let cred = cred_db.0.get_credential(parse_uuid(&session_id)?).await?;
    Ok(cred.map(|c| {
        let secret = crate::credentials::retrieve(&c.keychain_ref).unwrap_or_default();
        CredentialResponse {
            username: c.username,
            auth_type: c.auth_type,
            secret,
        }
    }))
}

#[tauri::command]
pub async fn session_list_all(state: State<'_, DbState>) -> Result<Vec<Session>, String> {
    state.0.list_all_sessions().await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn session_update(
    state: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
    id: String,
    name: Option<String>,
    hostname: Option<String>,
    port: Option<i32>,
    protocol: Option<String>,
    username: Option<String>,
    auth_method: Option<String>,
    tags: Option<String>,
    icon: Option<String>,
    jump_host_id: Option<Option<String>>,
    highlight_profile_id: Option<Option<String>>,
    password: Option<String>,
    key_path: Option<String>,
) -> Result<(), String> {
    if let Some(p) = port {
        validate_port(p)?;
    }
    if let Some(ref proto) = protocol {
        if proto != "ssh" && proto != "telnet" {
            return Err(format!("Unsupported protocol: {proto}"));
        }
    }
    validate_session_fields(name.as_deref(), hostname.as_deref(), tags.as_deref())?;
    if let Some(ref u) = username {
        if u.len() > super::MAX_USERNAME_LEN {
            return Err(format!(
                "Username too long (max {} characters)",
                super::MAX_USERNAME_LEN
            ));
        }
    }
    if let Some(ref i) = icon {
        if i.len() > super::MAX_ICON_LEN {
            return Err(format!(
                "Icon too long (max {} characters)",
                super::MAX_ICON_LEN
            ));
        }
    }
    let session_id = parse_uuid(&id)?;
    let jump = jump_host_id
        .map(|opt| opt.map(|s| parse_uuid(&s)).transpose())
        .transpose()?;
    let highlight = highlight_profile_id
        .map(|opt| opt.map(|s| parse_uuid(&s)).transpose())
        .transpose()?;

    state
        .0
        .update_session(
            session_id,
            UpdateSession {
                name,
                hostname,
                port,
                protocol,
                auth_method: auth_method.clone(),
                jump_host_id: jump,
                tags,
                icon,
                highlight_profile_id: highlight,
            },
        )
        .await?;

    // Update credential locally if username or secret was provided.
    let effective_method = auth_method;
    let secret = match effective_method.as_deref() {
        Some("password") => password,
        Some("publickey") => key_path,
        _ => password.or(key_path),
    };
    // Upsert credential if either username or secret changed.
    if username.is_some() || secret.is_some() {
        // Fetch existing credential to merge fields.
        let existing = cred_db.0.get_credential(session_id).await?;
        let keychain_ref = format!("session-{session_id}");

        let effective_username = username.unwrap_or_else(|| {
            existing
                .as_ref()
                .map_or(String::new(), |c| c.username.clone())
        });
        // Determine the secret to persist. If the caller supplied one, use it.
        // Otherwise merge with the existing keychain entry — but propagate any
        // retrieval error instead of silently overwriting with an empty string.
        let effective_secret = match secret {
            Some(s) => s,
            None => match existing.as_ref() {
                Some(c) => crate::credentials::retrieve(&c.keychain_ref).map_err(|e| {
                    format!("Failed to retrieve existing credential from keychain: {e}")
                })?,
                None => String::new(),
            },
        };
        let effective_auth = effective_method.unwrap_or_else(|| {
            existing
                .as_ref()
                .map_or(String::new(), |c| c.auth_type.clone())
        });

        if let Err(e) = cred_db
            .0
            .upsert_credential(Credential {
                id: Uuid::new_v4(),
                session_id,
                username: effective_username,
                auth_type: effective_auth,
                keychain_ref: keychain_ref.clone(),
            })
            .await
        {
            tracing::error!(session_id = %session_id, "credential upsert failed: {e}");
        }

        if let Err(e) = crate::credentials::store(&keychain_ref, &effective_secret) {
            tracing::error!(session_id = %session_id, "keychain store failed: {e}");
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn session_move(
    state: State<'_, DbState>,
    id: String,
    new_folder_id: String,
) -> Result<(), String> {
    state
        .0
        .move_session(parse_uuid(&id)?, parse_uuid(&new_folder_id)?)
        .await
}

#[tauri::command]
pub async fn session_delete(
    state: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
    id: String,
) -> Result<(), String> {
    let session_id = parse_uuid(&id)?;

    // Fetch keychain_ref before deleting DB rows so we can clean up the OS keychain.
    if let Ok(Some(cred)) = cred_db.0.get_credential(session_id).await {
        let _ = crate::credentials::delete(&cred.keychain_ref);
    }

    state.0.delete_session(session_id).await?;
    // Clean up local credential so it doesn't become orphaned
    // (especially important when sessions live in PostgreSQL).
    if let Err(e) = cred_db.0.delete_credential(session_id).await {
        tracing::warn!(session_id = %session_id, "credential cleanup on delete failed: {e}");
    }
    Ok(())
}

#[tauri::command]
pub async fn session_search(
    state: State<'_, DbState>,
    query: String,
) -> Result<Vec<Session>, String> {
    state.0.search_sessions(&query).await
}

#[tauri::command]
pub async fn session_data_fingerprint(
    state: State<'_, DbState>,
) -> Result<DataFingerprint, String> {
    state.0.data_fingerprint().await
}

// ── Reordering commands ──────────────────────────────────────────────

#[tauri::command]
pub async fn folder_reorder(
    state: State<'_, DbState>,
    parent_id: Option<String>,
    ordered_ids: Vec<String>,
) -> Result<(), String> {
    let parent_uuid = parent_id
        .as_deref()
        .map(|s| Uuid::parse_str(s).map_err(|e| format!("Invalid parent_id: {e}")))
        .transpose()?;
    let uuids: Vec<Uuid> = ordered_ids
        .iter()
        .map(|s| Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}")))
        .collect::<Result<_, _>>()?;
    state.0.reorder_folders(parent_uuid, uuids).await
}

#[tauri::command]
pub async fn session_reorder(
    state: State<'_, DbState>,
    folder_id: String,
    ordered_ids: Vec<String>,
) -> Result<(), String> {
    let folder_uuid = Uuid::parse_str(&folder_id).map_err(|e| format!("Invalid folder_id: {e}"))?;
    let uuids: Vec<Uuid> = ordered_ids
        .iter()
        .map(|s| Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}")))
        .collect::<Result<_, _>>()?;
    state.0.reorder_sessions(folder_uuid, uuids).await
}

#[tauri::command]
pub async fn folder_sort_alphabetically(
    state: State<'_, DbState>,
    parent_id: Option<String>,
    recursive: Option<bool>,
) -> Result<(), String> {
    let parent_uuid = parent_id
        .as_deref()
        .map(|s| Uuid::parse_str(s).map_err(|e| format!("Invalid parent_id: {e}")))
        .transpose()?;
    sort_children_alphabetically(&state.0, parent_uuid, recursive.unwrap_or(false)).await
}

#[tauri::command]
pub async fn session_sort_alphabetically(
    state: State<'_, DbState>,
    folder_id: String,
) -> Result<(), String> {
    let folder_uuid = Uuid::parse_str(&folder_id).map_err(|e| format!("Invalid folder_id: {e}"))?;
    state.0.sort_sessions_alphabetically(folder_uuid).await
}

/// Sort the immediate children (folders + sessions) of a parent folder
/// alphabetically.  When `recursive` is true, descend into every subfolder
/// and do the same.
async fn sort_children_alphabetically(
    db: &std::sync::Arc<dyn super::super::db::DatabaseProvider>,
    parent_id: Option<Uuid>,
    recursive: bool,
) -> Result<(), String> {
    db.sort_folders_alphabetically(parent_id).await?;
    if let Some(pid) = parent_id {
        db.sort_sessions_alphabetically(pid).await?;
    }

    if recursive {
        let all_folders = db.list_folders().await?;
        let children: Vec<Uuid> = all_folders
            .iter()
            .filter(|f| f.parent_id == parent_id)
            .map(|f| f.id)
            .collect();
        for child_id in children {
            Box::pin(sort_children_alphabetically(db, Some(child_id), true)).await?;
        }
    }
    Ok(())
}

// ── Connect a saved session ──────────────────────────────────────────

/// Load a saved session from the database, retrieve its credential from the
/// local credential store, resolve jump host chains, and open an SSH connection.
/// Returns the live connection ID for use with ssh_write/ssh_resize.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn session_connect(
    app_handle: AppHandle,
    db: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
    ssh: State<'_, SshState>,
    telnet: State<'_, TelnetState>,
    logger_state: State<'_, SessionLogState>,
    id: String,
    cols: u16,
    rows: u16,
    restrict_private_ips: Option<bool>,
    connect_timeout: Option<u64>,
    keepalive_interval: Option<u64>,
    keepalive_max: Option<u32>,
) -> Result<String, String> {
    validate_dimensions(cols, rows)?;

    let session_id = parse_uuid(&id)?;
    let session =
        db.0.get_session(session_id)
            .await?
            .ok_or_else(|| format!("Session {id} not found"))?;

    let restrict = restrict_private_ips.unwrap_or(false);
    let timeout_secs = connect_timeout.unwrap_or(10);
    let keepalive_secs = keepalive_interval.unwrap_or(15);
    let keepalive_missed = keepalive_max.unwrap_or(3) as usize;

    // Dispatch by protocol.
    if session.protocol == "telnet" {
        let conn_id = Uuid::new_v4().to_string();

        // Open session log before connecting.
        {
            if let Ok(mut mgr) = logger_state.0.lock() {
                if let Err(e) = mgr.open_log(&conn_id, &session.name) {
                    tracing::warn!("Failed to open session log: {e}");
                }
            }
        }
        let logger = Some(std::sync::Arc::clone(&logger_state.0));

        // Brief lock: check capacity only.
        {
            let manager = telnet.0.lock().await;
            manager.check_capacity()?;
        }

        // Connect WITHOUT holding the lock.
        let telnet_session = match establish_telnet_connection(TelnetConnectParams {
            id: conn_id.clone(),
            host: session.hostname,
            port: session.port as u16,
            cols,
            rows,
            app_handle,
            restrict_private_ips: restrict,
            connect_timeout_secs: timeout_secs,
            logger,
        })
        .await
        {
            Ok(s) => s,
            Err(e) => {
                // Close orphaned log file on connection failure.
                if let Ok(mut mgr) = logger_state.0.lock() {
                    mgr.close_log(&conn_id);
                }
                return Err(e);
            }
        };

        // Brief lock: re-check capacity and register atomically.
        {
            let mut manager = telnet.0.lock().await;
            manager.register_session(conn_id.clone(), telnet_session)?;
        }

        return Ok(conn_id);
    }

    // SSH path (default).
    // Retrieve credential metadata from local store, secret from OS keychain.
    let cred = cred_db.0.get_credential(session_id).await?;
    let (username, auth_credential) = match cred {
        Some(c) => {
            let secret = crate::credentials::retrieve(&c.keychain_ref).unwrap_or_default();
            (c.username, Zeroizing::new(secret))
        }
        None if session.auth_method == "none" => (String::new(), Zeroizing::new(String::new())),
        None => {
            return Err(format!(
                "No credential stored for session \"{}\". \
                 Edit the session to set a username and password or key path.",
                session.name
            ))
        }
    };

    // Resolve jump host chain.
    let jump_hops = resolve_jump_chain(&db, &cred_db, &session).await?;
    if jump_hops.len() > MAX_JUMP_HOPS {
        return Err(format!("Too many jump hops (max {MAX_JUMP_HOPS})"));
    }

    let conn_id = Uuid::new_v4().to_string();

    // Open session log before connecting.
    {
        if let Ok(mut mgr) = logger_state.0.lock() {
            if let Err(e) = mgr.open_log(&conn_id, &session.name) {
                tracing::warn!("Failed to open session log: {e}");
            }
        }
    }
    let logger = Some(std::sync::Arc::clone(&logger_state.0));

    // Brief lock: check capacity only.
    {
        let manager = ssh.manager.lock().await;
        manager.check_capacity()?;
    }

    // Connect WITHOUT holding the lock.
    let ssh_session = match establish_ssh_connection(
        SshConnectParams {
            id: conn_id.clone(),
            host: session.hostname,
            port: session.port as u16,
            username,
            auth_method: session.auth_method,
            auth_credential,
            cols,
            rows,
            app_handle,
            jump_hops,
            restrict_private_ips: restrict,
            connect_timeout_secs: timeout_secs,
            keepalive_interval_secs: keepalive_secs,
            keepalive_max: keepalive_missed,
            logger,
        },
        &ssh.host_verify_senders,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            // Close orphaned log file on connection failure.
            if let Ok(mut mgr) = logger_state.0.lock() {
                mgr.close_log(&conn_id);
            }
            return Err(e);
        }
    };

    // Brief lock: re-check capacity and register atomically.
    {
        let mut manager = ssh.manager.lock().await;
        manager.register_session(conn_id.clone(), ssh_session)?;
    }

    Ok(conn_id)
}

// ── Bulk credential assignment ──────────────────────────────────────

/// Apply credentials (and optionally a jump host) to all SSH sessions
/// in a folder and its subfolders. Telnet sessions are skipped.
/// Returns the number of sessions updated.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn folder_apply_credentials(
    db: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
    folder_id: String,
    username: String,
    auth_method: String,
    credential: String,
    jump_host_id: Option<String>,
    highlight_profile_id: Option<String>,
) -> Result<u32, String> {
    if auth_method != "password" && auth_method != "publickey" {
        return Err(format!("Unsupported auth method: {auth_method}"));
    }

    let target_folder = parse_uuid(&folder_id)?;

    // Resolve the optional jump host ID once.
    let jump_host_uuid = jump_host_id.map(|s| parse_uuid(&s)).transpose()?;

    // Resolve the optional highlight profile ID once.
    let highlight_uuid = highlight_profile_id.map(|s| parse_uuid(&s)).transpose()?;

    // Collect all folder IDs under the target (inclusive, recursive).
    let all_folders = db.0.list_folders().await?;
    let mut folder_set = HashSet::new();
    folder_set.insert(target_folder);
    collect_descendant_folders(target_folder, &all_folders, &mut folder_set);

    // Find all SSH sessions in those folders.
    let all_sessions = db.0.list_all_sessions().await?;
    let ssh_sessions: Vec<&Session> = all_sessions
        .iter()
        .filter(|s| folder_set.contains(&s.folder_id) && s.protocol == "ssh")
        .collect();

    let mut count: u32 = 0;
    for session in &ssh_sessions {
        // Upsert credential metadata (stored locally per user).
        let keychain_ref = format!("session-{}", session.id);
        if let Err(e) = cred_db
            .0
            .upsert_credential(Credential {
                id: Uuid::new_v4(),
                session_id: session.id,
                username: username.clone(),
                auth_type: auth_method.clone(),
                keychain_ref: keychain_ref.clone(),
            })
            .await
        {
            tracing::error!(session_id = %session.id, "bulk credential upsert failed: {e}");
            continue;
        }

        // Store secret in OS keychain.
        if let Err(e) = crate::credentials::store(&keychain_ref, &credential) {
            tracing::error!(session_id = %session.id, "bulk keychain store failed: {e}");
            continue;
        }

        // Update auth_method, jump_host_id, and highlight_profile_id on the
        // session.  All fields are always applied (this is a bulk-set dialog).
        // Silently skip setting a session as its own jump host.
        let effective_jump = match jump_host_uuid {
            Some(jid) if jid == session.id => None,
            other => other,
        };
        let update = UpdateSession {
            auth_method: Some(auth_method.clone()),
            jump_host_id: Some(effective_jump),
            highlight_profile_id: Some(highlight_uuid),
            ..Default::default()
        };
        if let Err(e) = db.0.update_session(session.id, update).await {
            tracing::error!(session_id = %session.id, "bulk session update failed: {e}");
            continue;
        }

        count += 1;
    }

    Ok(count)
}

/// Recursively collect all descendant folder IDs into `out`.
fn collect_descendant_folders(parent: Uuid, all: &[Folder], out: &mut HashSet<Uuid>) {
    for folder in all {
        if folder.parent_id == Some(parent) && out.insert(folder.id) {
            collect_descendant_folders(folder.id, all, out);
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────

fn parse_uuid(s: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

/// Walk the jump_host_id chain in the database to build a list of JumpHop
/// entries for the SSH connect call. Credentials come from local store.
async fn resolve_jump_chain(
    db: &State<'_, DbState>,
    cred_db: &State<'_, CredentialDbState>,
    session: &Session,
) -> Result<Vec<crate::ssh::JumpHop>, String> {
    let mut hops = Vec::new();
    let mut visited = HashSet::new();
    let mut current_jump_id = session.jump_host_id;

    while let Some(jump_id) = current_jump_id {
        if !visited.insert(jump_id) {
            return Err(format!(
                "Circular jump host chain detected at session {jump_id}"
            ));
        }
        if hops.len() >= MAX_JUMP_HOPS {
            return Err("Jump host chain too deep".to_string());
        }

        let jump_session =
            db.0.get_session(jump_id)
                .await?
                .ok_or_else(|| format!("Jump host session {jump_id} not found"))?;

        let jump_cred = match cred_db.0.get_credential(jump_id).await? {
            Some(cred) => cred,
            None => {
                return Err(format!(
                    "No credential stored for jump host \"{}\". \
                     Edit the session to set a username and password or key path.",
                    jump_session.name
                ));
            }
        };

        let jump_secret = crate::credentials::retrieve(&jump_cred.keychain_ref).map_err(|e| {
            format!(
                "Failed to retrieve credential for jump host \"{}\": {e}",
                jump_session.name
            )
        })?;

        hops.push(crate::ssh::JumpHop {
            host: jump_session.hostname.clone(),
            port: jump_session.port as u16,
            username: jump_cred.username,
            auth_method: jump_session.auth_method.clone(),
            auth_credential: Zeroizing::new(jump_secret),
        });

        current_jump_id = jump_session.jump_host_id;
    }

    // The chain is built from target→bastion order, but SSH needs bastion→target.
    hops.reverse();
    Ok(hops)
}
