use std::collections::HashSet;

use tauri::{AppHandle, State};
use uuid::Uuid;

use zeroize::Zeroizing;

use crate::db::models::{Credential, Folder, NewFolder, NewSession, Session, UpdateSession};
use crate::db::{CredentialDbState, DbState};
use crate::ssh::{SshConnectParams, SshState};

use super::{validate_dimensions, validate_port, validate_session_fields, MAX_JUMP_HOPS};

// ── Folder commands ──────────────────────────────────────────────────

#[tauri::command]
pub async fn folder_create(
    state: State<'_, DbState>,
    name: String,
    parent_id: Option<String>,
) -> Result<Folder, String> {
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
    auth_method: String,
    tags: String,
    icon: String,
    jump_host_id: Option<String>,
    password: Option<String>,
    key_path: Option<String>,
) -> Result<Session, String> {
    validate_port(port)?;
    validate_session_fields(Some(&name), Some(&hostname), Some(&username), Some(&tags))?;
    let folder = parse_uuid(&folder_id)?;
    let jump = jump_host_id.map(|s| parse_uuid(&s)).transpose()?;

    let session = state
        .0
        .create_session(NewSession {
            folder_id: folder,
            name,
            hostname,
            port,
            protocol: "ssh".to_string(),
            username,
            auth_method: auth_method.clone(),
            jump_host_id: jump,
            tags,
            icon,
        })
        .await?;

    // Store credential locally (never in the shared central DB).
    // Credential failure must not block session creation — the session is
    // valid without a credential; the user just cannot connect until they
    // set one via session_update.
    let secret = match auth_method.as_str() {
        "password" => password,
        "publickey" => key_path,
        _ => None,
    };
    if let Some(secret_value) = secret {
        let keychain_ref = format!("session-{}", session.id);
        if let Err(e) = cred_db
            .0
            .upsert_credential(Credential {
                id: Uuid::new_v4(),
                session_id: session.id,
                auth_type: auth_method,
                keychain_ref,
                secret: secret_value,
            })
            .await
        {
            tracing::error!(session_id = %session.id, "credential upsert failed: {e}");
        }
    }

    Ok(session)
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
    cred_db: State<'_, CredentialDbState>,
    id: String,
    name: Option<String>,
    hostname: Option<String>,
    port: Option<i32>,
    username: Option<String>,
    auth_method: Option<String>,
    tags: Option<String>,
    icon: Option<String>,
    jump_host_id: Option<Option<String>>,
    password: Option<String>,
    key_path: Option<String>,
) -> Result<(), String> {
    if let Some(p) = port {
        validate_port(p)?;
    }
    validate_session_fields(
        name.as_deref(),
        hostname.as_deref(),
        username.as_deref(),
        tags.as_deref(),
    )?;
    let session_id = parse_uuid(&id)?;
    let jump = jump_host_id
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
                username,
                auth_method: auth_method.clone(),
                jump_host_id: jump,
                tags,
                icon,
            },
        )
        .await?;

    // Update credential locally if a new secret was provided.
    let effective_method = auth_method;
    let secret = match effective_method.as_deref() {
        Some("password") => password,
        Some("publickey") => key_path,
        _ => password.or(key_path),
    };
    if let Some(secret_value) = secret {
        let keychain_ref = format!("session-{session_id}");
        if let Err(e) = cred_db
            .0
            .upsert_credential(Credential {
                id: Uuid::new_v4(),
                session_id,
                auth_type: effective_method.unwrap_or_default(),
                keychain_ref,
                secret: secret_value,
            })
            .await
        {
            tracing::error!(session_id = %session_id, "credential upsert failed: {e}");
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
    id: String,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    validate_dimensions(cols, rows)?;

    let session_id = parse_uuid(&id)?;
    let session =
        db.0.get_session(session_id)
            .await?
            .ok_or_else(|| format!("Session {id} not found"))?;

    // Retrieve credential from local store — wrapped in Zeroizing so it is
    // scrubbed from memory as soon as it is no longer needed.
    let auth_credential: Zeroizing<String> = match cred_db.0.get_credential(session_id).await? {
        Some(cred) => Zeroizing::new(cred.secret),
        None if session.auth_method == "none" => Zeroizing::new(String::new()),
        None => {
            return Err(format!(
                "No credential stored for session \"{}\". \
                 Edit the session to set a password or key path.",
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
    let mut manager = ssh.0.lock().await;
    manager
        .connect(SshConnectParams {
            id: conn_id.clone(),
            host: session.hostname,
            port: session.port as u16,
            username: session.username,
            auth_method: session.auth_method,
            auth_credential,
            cols,
            rows,
            app_handle,
            jump_hops,
        })
        .await?;

    Ok(conn_id)
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

        let jump_credential = match cred_db.0.get_credential(jump_id).await? {
            Some(cred) => Zeroizing::new(cred.secret),
            None => {
                return Err(format!(
                    "No credential stored for jump host \"{}\". \
                     Edit the session to set a password or key path.",
                    jump_session.name
                ));
            }
        };

        hops.push(crate::ssh::JumpHop {
            host: jump_session.hostname.clone(),
            port: jump_session.port as u16,
            username: jump_session.username.clone(),
            auth_method: jump_session.auth_method.clone(),
            auth_credential: jump_credential,
        });

        current_jump_id = jump_session.jump_host_id;
    }

    // The chain is built from target→bastion order, but SSH needs bastion→target.
    hops.reverse();
    Ok(hops)
}
