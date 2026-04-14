use std::collections::HashSet;

use tauri::{AppHandle, State};
use uuid::Uuid;

use zeroize::Zeroizing;

use crate::db::models::{
    CredentialProfile, DataFingerprint, Folder, NewFolder, NewSession, Session, UpdateSession,
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
    let folder_uuid = parse_uuid(&id)?;
    let parent = new_parent_id.map(|s| parse_uuid(&s)).transpose()?;

    // Reject cycles: a folder cannot be moved into itself or any of its
    // descendants. Without this, a recursive sort would stack-overflow.
    if let Some(target_parent) = parent {
        if target_parent == folder_uuid {
            return Err("A folder cannot be moved into itself.".to_string());
        }
        let all_folders = state.0.list_folders().await?;
        let mut current = Some(target_parent);
        while let Some(cur) = current {
            if cur == folder_uuid {
                return Err("A folder cannot be moved into one of its descendants.".to_string());
            }
            current = all_folders
                .iter()
                .find(|f| f.id == cur)
                .and_then(|f| f.parent_id);
        }
    }

    state.0.move_folder(folder_uuid, parent).await
}

#[tauri::command]
pub async fn folder_delete(state: State<'_, DbState>, id: String) -> Result<(), String> {
    state.0.delete_folder(parse_uuid(&id)?).await
}

/// Bulk-edit sessions in a folder (and its descendants). Each `set_*` flag
/// controls whether its sibling field is applied; when `true`, a `null`
/// value clears the column. This avoids the `Option<Option<T>>` JSON
/// ambiguity.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn folder_bulk_edit_sessions(
    state: State<'_, DbState>,
    folder_id: String,
    set_jump_host: bool,
    jump_host_id: Option<String>,
    set_highlight_profile: bool,
    highlight_profile_id: Option<String>,
    icon: Option<String>,
) -> Result<u32, String> {
    let folder_uuid = parse_uuid(&folder_id)?;
    let jump_host = if set_jump_host {
        Some(match jump_host_id {
            Some(s) => Some(parse_uuid(&s)?),
            None => None,
        })
    } else {
        None
    };
    let highlight = if set_highlight_profile {
        Some(match highlight_profile_id {
            Some(s) => Some(parse_uuid(&s)?),
            None => None,
        })
    } else {
        None
    };
    if let Some(ref i) = icon {
        if i.len() > super::MAX_ICON_LEN {
            return Err(format!(
                "Icon too long (max {} characters)",
                super::MAX_ICON_LEN
            ));
        }
    }
    state
        .0
        .bulk_edit_sessions(
            folder_uuid,
            crate::db::BulkSessionEdit {
                jump_host_id: jump_host,
                highlight_profile_id: highlight,
                icon,
            },
        )
        .await
}

// ── Session commands ─────────────────────────────────────────────────

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn session_create(
    state: State<'_, DbState>,
    folder_id: String,
    name: String,
    hostname: String,
    port: i32,
    protocol: Option<String>,
    tags: String,
    icon: String,
    jump_host_id: Option<String>,
    highlight_profile_id: Option<String>,
    credential_profile_id: Option<String>,
    legacy_algorithms: Option<bool>,
) -> Result<Session, String> {
    validate_port(port)?;
    validate_session_fields(Some(&name), Some(&hostname), Some(&tags))?;
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
    let credential = credential_profile_id
        .as_deref()
        .map(parse_uuid)
        .transpose()?;

    state
        .0
        .create_session(NewSession {
            folder_id: folder,
            name,
            hostname,
            port,
            protocol: effective_protocol,
            // auth_method is determined by the credential profile at connect
            // time; keep the column populated for backwards compatibility
            // with existing rows but don't expose it in the dialog.
            auth_method: "profile".to_string(),
            jump_host_id: jump,
            tags,
            icon,
            highlight_profile_id: highlight,
            credential_profile_id: credential,
            legacy_algorithms: legacy_algorithms.unwrap_or(false),
        })
        .await
}

#[tauri::command]
pub async fn session_get(state: State<'_, DbState>, id: String) -> Result<Option<Session>, String> {
    state.0.get_session(parse_uuid(&id)?).await
}

#[tauri::command]
pub async fn session_list_all(state: State<'_, DbState>) -> Result<Vec<Session>, String> {
    state.0.list_all_sessions().await
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn session_update(
    state: State<'_, DbState>,
    id: String,
    name: Option<String>,
    hostname: Option<String>,
    port: Option<i32>,
    protocol: Option<String>,
    tags: Option<String>,
    icon: Option<String>,
    jump_host_id: Option<String>,
    highlight_profile_id: Option<String>,
    credential_profile_id: Option<String>,
    legacy_algorithms: Option<bool>,
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
    if let Some(ref i) = icon {
        if i.len() > super::MAX_ICON_LEN {
            return Err(format!(
                "Icon too long (max {} characters)",
                super::MAX_ICON_LEN
            ));
        }
    }
    let session_id = parse_uuid(&id)?;
    // The frontend always sends these fields explicitly on update, so None
    // from the wire is the user's "clear to none" intent — translate it to
    // Some(None) to distinguish from "don't touch" in the DB layer.
    let jump = Some(match jump_host_id {
        Some(s) => Some(parse_uuid(&s)?),
        None => None,
    });
    let highlight = Some(match highlight_profile_id {
        Some(s) => Some(parse_uuid(&s)?),
        None => None,
    });
    let credential = Some(match credential_profile_id {
        Some(s) => Some(parse_uuid(&s)?),
        None => None,
    });

    state
        .0
        .update_session(
            session_id,
            UpdateSession {
                name,
                hostname,
                port,
                protocol,
                auth_method: None,
                jump_host_id: jump,
                tags,
                icon,
                highlight_profile_id: highlight,
                credential_profile_id: credential,
                legacy_algorithms,
            },
        )
        .await
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
pub async fn session_delete(state: State<'_, DbState>, id: String) -> Result<(), String> {
    state.0.delete_session(parse_uuid(&id)?).await
}

/// Cap the search term length to bound LIKE-query cost and prevent a
/// pathologically long needle from forcing expensive full-table scans.
const MAX_SEARCH_QUERY_LEN: usize = 200;

#[tauri::command]
pub async fn session_search(
    state: State<'_, DbState>,
    query: String,
) -> Result<Vec<Session>, String> {
    let trimmed = if query.len() > MAX_SEARCH_QUERY_LEN {
        &query[..query
            .char_indices()
            .take_while(|(i, _)| *i < MAX_SEARCH_QUERY_LEN)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0)]
    } else {
        query.as_str()
    };
    state.0.search_sessions(trimmed).await
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

/// Load a saved session from the database, resolve its credential profile,
/// walk any jump host chain, and open an SSH (or Telnet) connection.
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

        {
            let manager = telnet.0.lock().await;
            manager.check_capacity()?;
        }

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
                if let Ok(mut mgr) = logger_state.0.lock() {
                    mgr.close_log(&conn_id);
                }
                return Err(e);
            }
        };

        {
            let mut manager = telnet.0.lock().await;
            manager.register_session(conn_id.clone(), telnet_session)?;
        }

        return Ok(conn_id);
    }

    // SSH path (default). Resolve the credential profile assigned to the
    // session; if none, reject with a clear error. (Keyboard-interactive
    // fallback requires a UI prompt flow and is tracked separately.)
    let profile = resolve_profile_for_session(&cred_db, &session).await?;
    let (username, auth_method, auth_credential) = profile_to_auth(&profile)?;

    // Resolve jump host chain.
    let jump_hops = resolve_jump_chain(&db, &cred_db, &session).await?;
    if jump_hops.len() > MAX_JUMP_HOPS {
        return Err(format!("Too many jump hops (max {MAX_JUMP_HOPS})"));
    }

    let conn_id = Uuid::new_v4().to_string();

    {
        if let Ok(mut mgr) = logger_state.0.lock() {
            if let Err(e) = mgr.open_log(&conn_id, &session.name) {
                tracing::warn!("Failed to open session log: {e}");
            }
        }
    }
    let logger = Some(std::sync::Arc::clone(&logger_state.0));

    {
        let manager = ssh.manager.lock().await;
        manager.check_capacity()?;
    }

    let ssh_session = match establish_ssh_connection(
        SshConnectParams {
            id: conn_id.clone(),
            host: session.hostname,
            port: session.port as u16,
            username,
            auth_method,
            auth_credential,
            cols,
            rows,
            app_handle,
            jump_hops,
            legacy_algorithms: session.legacy_algorithms,
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
            if let Ok(mut mgr) = logger_state.0.lock() {
                mgr.close_log(&conn_id);
            }
            return Err(e);
        }
    };

    {
        let mut manager = ssh.manager.lock().await;
        manager.register_session(conn_id.clone(), ssh_session)?;
    }

    Ok(conn_id)
}

// ── Helpers ──────────────────────────────────────────────────────────

fn parse_uuid(s: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

/// Look up the credential profile assigned to a session, returning a clear
/// error if none is set or the referenced profile has been deleted.
async fn resolve_profile_for_session(
    cred_db: &State<'_, CredentialDbState>,
    session: &Session,
) -> Result<CredentialProfile, String> {
    let profile_id = session.credential_profile_id.ok_or_else(|| {
        format!(
            "No credential profile assigned to session \"{}\". \
             Open the Credentials Manager to create one, then edit the \
             session to link it.",
            session.name
        )
    })?;
    cred_db
        .0
        .get_credential_profile(profile_id)
        .await?
        .ok_or_else(|| {
            format!(
                "Credential profile for session \"{}\" no longer exists. \
                 Edit the session and pick a different profile.",
                session.name
            )
        })
}

/// Translate a profile into the (username, ssh auth_method, secret) triple
/// expected by the SSH connection layer.
fn profile_to_auth(
    profile: &CredentialProfile,
) -> Result<(String, String, Zeroizing<String>), String> {
    match profile.auth_type.as_str() {
        "password" => {
            let secret = crate::credentials::retrieve(&profile.keychain_ref)
                .map_err(|e| format!("Failed to retrieve profile secret: {e}"))?;
            Ok((
                profile.username.clone(),
                "password".to_string(),
                Zeroizing::new(secret),
            ))
        }
        "key" => {
            // Private-key path is stored on the profile row; any passphrase
            // lives in the keychain but is not currently passed to russh.
            Ok((
                profile.username.clone(),
                "publickey".to_string(),
                Zeroizing::new(profile.key_path.clone()),
            ))
        }
        other => Err(format!(
            "Auth type \"{other}\" is not yet supported for connect \
             (profile \"{}\").",
            profile.name
        )),
    }
}

/// Walk the jump_host_id chain in the database to build a list of JumpHop
/// entries for the SSH connect call. Each hop pulls its own profile.
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

        let jump_profile = resolve_profile_for_session(cred_db, &jump_session).await?;
        let (username, auth_method, auth_credential) = profile_to_auth(&jump_profile)?;

        hops.push(crate::ssh::JumpHop {
            host: jump_session.hostname.clone(),
            port: jump_session.port as u16,
            username,
            auth_method,
            auth_credential,
        });

        current_jump_id = jump_session.jump_host_id;
    }

    // The chain is built from target→bastion order, but SSH needs bastion→target.
    hops.reverse();
    Ok(hops)
}
