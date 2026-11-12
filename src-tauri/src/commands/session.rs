use std::collections::HashSet;

use tauri::{AppHandle, State};
use uuid::Uuid;

use zeroize::Zeroizing;

use tracing::debug;

use crate::db::models::{
    CredentialProfile, DataFingerprint, Folder, NewFolder, NewSession, Session, UpdateSession,
};
use crate::db::{CredentialDbState, DatabaseProvider, DbState};
use crate::login_pins::{needs_confirmation, LoginPinState, UsedAs};
use crate::session_logger::SessionLogState;
use crate::ssh::{establish_ssh_connection, SshConnectParams, SshState};
use crate::telnet::{establish_telnet_connection, TelnetConnectParams, TelnetState};

use super::session_credentials::CredentialMappings;
use super::{
    connection, validate_dimensions, validate_port, validate_session_fields, ConnectSetup,
    TerminalReadyState, MAX_JUMP_HOPS,
};

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
        // A ring above the target that this folder is no part of, as a
        // database may have from statements of someone's own, must not keep
        // this from coming to an end. The database then decides about the
        // move.
        let mut seen = HashSet::new();
        while let Some(cur) = current {
            if cur == folder_uuid {
                return Err("A folder cannot be moved into one of its descendants.".to_string());
            }
            if !seen.insert(cur) {
                break;
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
    set_login_sequence: bool,
    login_sequence_id: Option<String>,
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
    let login_seq = if set_login_sequence {
        Some(match login_sequence_id {
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
                login_sequence_id: login_seq,
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
    username: Option<String>,
    tags: String,
    icon: String,
    jump_host_id: Option<String>,
    highlight_profile_id: Option<String>,
    credential_profile_id: Option<String>,
    login_sequence_id: Option<String>,
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
    let login_seq = login_sequence_id.as_deref().map(parse_uuid).transpose()?;

    state
        .0
        .create_session(NewSession {
            folder_id: folder,
            name,
            hostname,
            port,
            protocol: effective_protocol,
            username: username.unwrap_or_default(),
            // auth_method is determined by the credential profile at connect
            // time; keep the column populated for backwards compatibility
            // with existing rows but don't expose it in the dialog.
            auth_method: "profile".to_string(),
            jump_host_id: jump,
            tags,
            icon,
            highlight_profile_id: highlight,
            credential_profile_id: credential,
            login_sequence_id: login_seq,
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
    username: Option<String>,
    tags: Option<String>,
    icon: Option<String>,
    jump_host_id: Option<String>,
    highlight_profile_id: Option<String>,
    credential_profile_id: Option<String>,
    login_sequence_id: Option<String>,
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
    let login_seq = Some(match login_sequence_id {
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
                username,
                auth_method: None,
                jump_host_id: jump,
                tags,
                icon,
                highlight_profile_id: highlight,
                credential_profile_id: credential,
                login_sequence_id: login_seq,
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
            .map_or(0, |(i, c)| i + c.len_utf8())]
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

// ── Visibility (multi-user) ──────────────────────────────────────────

/// Toggle an item's visibility between "personal" and "shared".
/// Only meaningful in `PostgreSQL` mode where `RLS` enforces ownership.
#[tauri::command]
pub async fn set_visibility(
    pg: State<'_, crate::db::PgPoolState>,
    id: String,
    item_type: String,
    visibility: String,
) -> Result<(), String> {
    let pool =
        pg.0.as_ref()
            .ok_or("Visibility control is only available in PostgreSQL mode")?;
    let id_uuid = parse_uuid(&id)?;
    if visibility != "personal" && visibility != "shared" {
        return Err(format!(
            "Invalid visibility \"{visibility}\": must be \"personal\" or \"shared\""
        ));
    }
    let id_str = id_uuid.to_string();
    match item_type.as_str() {
        "folder" => {
            let result = sqlx::query(
                "UPDATE folders SET visibility = $1 WHERE id = $2 AND owner = current_user",
            )
            .bind(&visibility)
            .bind(&id_str)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to set visibility: {e}"))?;
            if result.rows_affected() == 0 {
                return Err("Item not found or you are not the owner".to_string());
            }
            // Cascade to all descendant folders owned by the current user
            sqlx::query(
                "WITH RECURSIVE subtree(id) AS ( \
                     SELECT id FROM folders WHERE parent_id = $2 AND owner = current_user \
                     UNION ALL \
                     SELECT f.id FROM folders f JOIN subtree s \
                         ON f.parent_id = s.id AND f.owner = current_user \
                 ) \
                 UPDATE folders SET visibility = $1 WHERE id IN (SELECT id FROM subtree)",
            )
            .bind(&visibility)
            .bind(&id_str)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to cascade folder visibility: {e}"))?;
            // Cascade to all sessions in the target folder and descendants
            sqlx::query(
                "WITH RECURSIVE subtree(id) AS ( \
                     SELECT id FROM folders WHERE id = $2 AND owner = current_user \
                     UNION ALL \
                     SELECT f.id FROM folders f JOIN subtree s \
                         ON f.parent_id = s.id AND f.owner = current_user \
                 ) \
                 UPDATE sessions SET visibility = $1 \
                 WHERE folder_id IN (SELECT id FROM subtree) AND owner = current_user",
            )
            .bind(&visibility)
            .bind(&id_str)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to cascade session visibility: {e}"))?;
        }
        "session" => {
            let result = sqlx::query(
                "UPDATE sessions SET visibility = $1 WHERE id = $2 AND owner = current_user",
            )
            .bind(&visibility)
            .bind(&id_str)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to set visibility: {e}"))?;
            if result.rows_affected() == 0 {
                return Err("Item not found or you are not the owner".to_string());
            }
        }
        _ => {
            return Err(format!(
                "Invalid item_type \"{item_type}\": must be \"folder\" or \"session\""
            ));
        }
    }
    Ok(())
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

#[tauri::command]
pub async fn session_sort_by_hostname(
    state: State<'_, DbState>,
    folder_id: String,
) -> Result<(), String> {
    let folder_uuid = Uuid::parse_str(&folder_id).map_err(|e| format!("Invalid folder_id: {e}"))?;
    state.0.sort_sessions_by_hostname(folder_uuid).await
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
/// Returns the live connection `ID` for use with `ssh_write` / `ssh_resize`.
///
/// ## Why this function is large
///
/// `session_connect` is intentionally an orchestration boundary: it ties
/// together state from eight Tauri-managed services (db, `cred_db`,
/// `login_seq_db`, ssh, telnet, terminal-ready, session-log, pg-pool,
/// config) plus per-session settings to produce one side-effecting
/// connection. The branchiness measured by cyclomatic complexity comes
/// from coordinating those services — every `?` is a state lookup that
/// must be released cleanly before the next, and every `if` selects
/// between code paths the user explicitly chose (SSH vs Telnet, jump
/// chain vs direct, password vs key vs keyboard-interactive, logging
/// on vs off, login-sequence present vs absent).
///
/// Splitting this into smaller helpers was tried and abandoned: the
/// helpers ended up taking 6-8 of the same `State<'_, …>` handles each,
/// duplicated the audit-trail logging at every boundary, and made
/// lifetime errors harder to read without reducing total complexity.
/// Unit-testing this in isolation would require mocking all eight
/// services and a real network endpoint; the right testing point is
/// the integration layer (Tauri's `mock_builder`), tracked separately.
///
/// If you are tempted to add a new branch here, prefer adding it to a
/// freestanding helper in `ssh::` or `telnet::` and calling that helper
/// from one place in this function. The shape of `session_connect` —
/// "load → validate → branch on protocol → emit" — should stay flat.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn session_connect(
    app_handle: AppHandle,
    db: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
    login_seq_db: State<'_, crate::db::LoginSequenceDbState>,
    ssh: State<'_, SshState>,
    telnet: State<'_, TelnetState>,
    ready_state: State<'_, TerminalReadyState>,
    logger_state: State<'_, SessionLogState>,
    pg: State<'_, crate::db::PgPoolState>,
    pg_user: State<'_, crate::db::PgUserState>,
    config: State<'_, crate::config::ConfigState>,
    pins: State<'_, LoginPinState>,
    page: String,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    ready_state.check_page(&page)?;
    validate_dimensions(cols, rows)?;

    let session_id = parse_uuid(&id)?;
    let session =
        db.0.get_session(session_id)
            .await?
            .ok_or_else(|| format!("Session {id} not found"))?;
    // The commands that save a session take no other port, but a row can be
    // written past them, and a port outside the range would be cut down to
    // another one than the row shows.
    validate_port(session.port)?;

    let settings = connection::current(&config)?;

    // What the session's own row names, where that is this user's to go by;
    // see `row_defaults_apply`.
    let row_defaults = row_defaults_apply(pg.0.is_some(), pg_user.0.as_deref(), &session);
    let row_login_sequence = session.login_sequence_id.filter(|_| row_defaults);
    let row_credential_profile = session.credential_profile_id.filter(|_| row_defaults);
    if !row_defaults
        && (session.login_sequence_id.is_some() || session.credential_profile_id.is_some())
    {
        debug!(session = %id, "Not this user's session — the profile and sequence its row names are left out");
    }

    // Resolve login sequence (applies to both SSH and Telnet).
    let user_ident_for_ls = pg.0.as_ref().and_then(|_| {
        config
            .config
            .lock()
            .ok()
            .and_then(|c| c.user_ident.clone())
            .filter(|s| !s.is_empty())
    });
    let mapped_ls_id = if let (Some(pool), Some(ref ident)) = (&pg.0, &user_ident_for_ls) {
        super::session_login_sequences::query_user_login_sequence(
            pool,
            &session_id.to_string(),
            ident,
        )
        .await?
    } else {
        None
    };
    // A login sequence the user assigned to a session that is not theirs is
    // run only for what they confirmed the session names; see
    // `login_pins.rs`. This comes before the sequence is read and before
    // anything is connected to, for Telnet as for SSH: `session` is the row
    // as read above, and it is what the rest of this function goes by.
    if needs_confirmation(pg.0.is_some(), row_defaults, mapped_ls_id.is_some()) {
        pins.check(&session, UsedAs::Target, pg_user.0.is_some())?;
    }
    let effective_ls_id = mapped_ls_id.or(row_login_sequence);

    let login_sequence = if let Some(ls_id) = effective_ls_id {
        login_seq_db.0.get_login_sequence(ls_id).await?
    } else {
        None
    };

    // Dispatch by protocol.
    if session.protocol == "telnet" {
        let conn_id = Uuid::new_v4().to_string();

        {
            let manager = telnet.0.lock().await;
            manager.check_capacity()?;
        }

        let logger = Some(std::sync::Arc::clone(&logger_state.0));
        let setup = ConnectSetup::begin(&conn_id, &session.name, page, &ready_state, &logger_state);

        // Create ready signal so the reader task waits for the frontend listener.
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
        setup.add_ready_signal(move || {
            let _ = ready_tx.send(());
        });

        let telnet_login_seq = if let Some(ref seq) = login_sequence {
            Some(crate::login_sequence::resolve_login_sequence(
                seq,
                &session.username,
                "",
            )?)
        } else {
            None
        };

        let telnet_session = establish_telnet_connection(TelnetConnectParams {
            id: conn_id.clone(),
            host: session.hostname,
            port: session.port as u16,
            cols,
            rows,
            app_handle,
            restrict_private_ips: settings.restrict_private_ips,
            connect_timeout_secs: settings.connect_timeout_secs,
            logger,
            ready_rx,
            login_sequence: telnet_login_seq,
        })
        .await?;

        {
            let mut manager = telnet.0.lock().await;
            setup.check_page_unchanged()?;
            manager.register_session(conn_id.clone(), telnet_session)?;
        }

        setup.disarm();
        return Ok(conn_id);
    }

    // SSH path (default). In PG mode, resolve per-user credential mapping first;
    // fall back to session's own credential_profile_id, then keyboard-interactive.
    let user_ident = pg.0.as_ref().and_then(|_| {
        config
            .config
            .lock()
            .ok()
            .and_then(|c| c.user_ident.clone())
            .filter(|s| !s.is_empty())
    });
    let mappings = match (&pg.0, &user_ident) {
        (Some(pool), Some(ident)) => CredentialMappings::Postgres {
            pool,
            user_ident: ident,
        },
        _ => CredentialMappings::None,
    };
    let mapped_credential_id = mappings.of(session_id).await?;
    // The same for a credential profile the user assigned, before the
    // profile is read.
    if needs_confirmation(pg.0.is_some(), row_defaults, mapped_credential_id.is_some()) {
        pins.check(&session, UsedAs::Target, pg_user.0.is_some())?;
    }
    let effective_credential_id = mapped_credential_id.or(row_credential_profile);

    let (username, auth_method, auth_credential) = if let Some(profile_id) = effective_credential_id
    {
        debug!(session = %id, profile_id = %profile_id, "Resolving credential profile for session");
        let profile = cred_db
            .0
            .get_credential_profile(profile_id)
            .await?
            .ok_or_else(|| {
                format!(
                    "Credential profile for session \"{}\" no longer exists. {}",
                    session.name,
                    other_profile_advice(row_defaults)
                )
            })?;
        debug!(session = %id, auth_type = %profile.auth_type, user = %profile.username, "Credential profile resolved");
        profile_to_auth(&profile)?
    } else {
        debug!(session = %id, user = %session.username, "No credential profile — falling back to keyboard-interactive");
        if session.username.is_empty() {
            return Err(format!(
                "No credential profile assigned and no username configured. {}",
                no_profile_advice(
                    row_defaults,
                    "Edit the session to set a username or assign a credential profile."
                )
            ));
        }
        (
            session.username.clone(),
            "keyboard-interactive".to_string(),
            Zeroizing::new(String::new()),
        )
    };

    // Resolve jump host chain.
    let jump_hops = resolve_jump_chain(
        db.0.as_ref(),
        cred_db.0.as_ref(),
        &session,
        pg.0.as_ref(),
        &mappings,
        pg_user.0.as_deref(),
        &pins,
    )
    .await?;
    if jump_hops.len() > MAX_JUMP_HOPS {
        return Err(format!("Too many jump hops (max {MAX_JUMP_HOPS})"));
    }

    let conn_id = Uuid::new_v4().to_string();

    {
        let manager = ssh.manager.lock().await;
        manager.check_capacity()?;
    }

    let logger = Some(std::sync::Arc::clone(&logger_state.0));
    let setup = ConnectSetup::begin(&conn_id, &session.name, page, &ready_state, &logger_state);

    // Create ready signal so the reader task waits for the frontend listener.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    setup.add_ready_signal(move || {
        let _ = ready_tx.send(());
    });

    let ssh_login_seq = if let Some(ref seq) = login_sequence {
        Some(crate::login_sequence::resolve_login_sequence(
            seq,
            &username,
            sequence_password(&auth_method, &auth_credential),
        )?)
    } else {
        None
    };

    let ssh_session = establish_ssh_connection(
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
            restrict_private_ips: settings.restrict_private_ips,
            connect_timeout_secs: settings.connect_timeout_secs,
            keepalive_interval_secs: settings.keepalive_interval_secs,
            keepalive_max: settings.keepalive_max as usize,
            logger,
            kbd_interactive_senders: std::sync::Arc::clone(&ssh.kbd_interactive_senders),
            ready_rx,
            login_sequence: ssh_login_seq,
        },
        &ssh.host_verify_senders,
    )
    .await?;

    {
        let mut manager = ssh.manager.lock().await;
        setup.check_page_unchanged()?;
        manager.register_session(conn_id.clone(), ssh_session)?;
    }

    setup.disarm();
    Ok(conn_id)
}

// ── Helpers ──────────────────────────────────────────────────────────

fn parse_uuid(s: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

/// Translate a profile into the (username, ssh `auth_method`, secret) triple
/// expected by the SSH connection layer.
fn profile_to_auth(
    profile: &CredentialProfile,
) -> Result<(String, String, Zeroizing<String>), String> {
    match profile.auth_type.as_str() {
        "password" => {
            let secret = crate::credentials::retrieve(&profile.keychain_ref)
                .map_err(|e| format!("Failed to retrieve profile secret: {e}"))?;
            Ok((profile.username.clone(), "password".to_string(), secret))
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
        "keyboard-interactive" => Ok((
            profile.username.clone(),
            "keyboard-interactive".to_string(),
            Zeroizing::new(String::new()),
        )),
        other => Err(format!(
            "Auth type \"{other}\" is not yet supported for connect \
             (profile \"{}\").",
            profile.name
        )),
    }
}

/// Whether the credential profile and the login sequence that a session's
/// own row names are this user's to connect with.
///
/// Both are IDs in the connecting user's local database, and decide which of
/// that user's secrets the connection sends to the session's host. In
/// `PostgreSQL` mode a session can be another user's, shared: then that user
/// wrote the row, the host and these IDs alike, and an ID in it that happens
/// to be, or was made to be, one of this user's own profiles would send that
/// profile's secret to a host of the other user's choosing. So there the
/// row's IDs count only for the session's owner, and anyone else connects by
/// their own mapping of the session (`session_credentials`,
/// `session_login_sequences`) or without a profile.
///
/// The app itself leaves both columns empty in `PostgreSQL` mode and keeps
/// each user's choice in the mapping tables, so what a row names there was
/// imported or written by a statement of someone's own. In `SQLite` mode
/// every session is the user's own.
///
/// `pg_user` is the role the app connected as. Where that is not known, no
/// row is taken for this user's, nor is one that still has the placeholder
/// owner.
fn row_defaults_apply(pg_mode: bool, pg_user: Option<&str>, session: &Session) -> bool {
    // `'local'` is the placeholder owner of rows from before rows had one,
    // and no role's own, also not that of a role with this name.
    !pg_mode || (pg_user == Some(session.owner.as_str()) && session.owner != "local")
}

/// What a connect that finds no credential profile to go by tells the user to
/// do about it. `own` is the advice for a session that is the user's to edit.
/// For any other it is that what its row names does not count, and where the
/// user assigns their own: the session's context menu, which has **My Login**
/// for another user's session in place of **Edit**.
///
/// "Any other" is as [`row_defaults_apply`] has it: another user's shared
/// session, but also a row that still has the placeholder owner, and every
/// session while the role the app connected as is not known. The text
/// therefore does not say whose the session is. It does say who chooses the
/// host, since a profile assigned to a session is used for whatever host its
/// owner has it name, and that the User Identity has to be set, without
/// which no mapping is saved or looked up. While the role is not known the
/// menu has no **My Login**; that state is the database's status not having
/// been read, and passes with it.
fn no_profile_advice(row_defaults: bool, own: &'static str) -> &'static str {
    if row_defaults {
        own
    } else {
        "The credential profile and login sequence this session names are \
         used only for its owner, and you are not on record as that. Assign \
         a credential profile of your own to it from its context menu (My \
         Login), with your User Identity set in Settings. Its owner chooses \
         the host your profile is then used for."
    }
}

/// What a connect tells the user to do when the credential profile assigned
/// to a session or jump host is gone: where another one is picked, which is
/// the session dialog for a session that is the user's to edit and **My
/// Login** for any other; see [`no_profile_advice`].
fn other_profile_advice(row_defaults: bool) -> &'static str {
    if row_defaults {
        "Edit that session and pick a different profile."
    } else {
        "Pick a different profile for it from its context menu (My Login)."
    }
}

/// What `\w` in a login sequence stands for on an SSH connection: the
/// password of a password profile, and nothing with any other way of
/// authenticating. With a key profile the credential is the path of the key
/// file, which is no password, and nothing to type at a remote host's prompt:
/// it would tell that host where the key is kept and under which user name.
fn sequence_password<'a>(auth_method: &str, auth_credential: &'a str) -> &'a str {
    if auth_method == "password" {
        auth_credential
    } else {
        ""
    }
}

/// Whether a session may be the jump host of the session whose row names it,
/// given the owners of the two.
///
/// In `PostgreSQL` mode a jump host has to be a session of the same owner,
/// link by link, so a chain has one owner. The owner of a shared session
/// chooses its jump host, and the database took any session's ID for it,
/// another user's included. A user who opened such a session went through a
/// jump host they had a profile for, which is first of all a session of
/// their own: their own bastion then opened a connection to the host and
/// port the other user's session names. And a jump host that its owner
/// deleted was dropped from the session of someone else that named it, which
/// then connected directly.
///
/// The database refuses to name such a jump host now
/// (`shellstation_own_jump_host` in `setup_postgres_rls`). This is for what
/// was named before, for a database whose owning role has not started that
/// version yet, and for one where a trigger does not hold a user (Option B).
/// It goes by the owners of the two rows, not by who connects, and so holds
/// as far as a row's owner does: a user who can write another owner into a
/// row of their own, which the policies and `shellstation_keep_owner` keep a
/// regular user from, gets past it. Rows that still have the placeholder
/// `'local'` for an owner count as one owner's among themselves, and as
/// another owner's to everyone else's.
///
/// In `SQLite` mode every session is the user's own, whatever its owner says.
fn jump_host_is_its_owners(pg_mode: bool, named_by_owner: &str, jump_host_owner: &str) -> bool {
    !pg_mode || named_by_owner == jump_host_owner
}

/// Walk the `jump_host_id` chain in the database to build a list of `JumpHop`
/// entries for the SSH connect call. Each hop pulls its own profile. In
/// `PostgreSQL` mode the walk fails at a hop that is not a session of the
/// same owner as the session that names it, before that hop's profile is
/// looked up, and no hop is returned; see [`jump_host_is_its_owners`]. It
/// fails likewise at a hop that is not the user's own session and no longer
/// names what the user confirmed of it (`pins`, see `login_pins.rs`). Where
/// a profile the user assigned is used for such a hop, the session connected
/// to is held to what the user confirmed of it as well, also where nothing
/// is assigned to that session itself: its switch for legacy algorithms is
/// what the connection to every hop is made with, so a session of the same
/// owner that the user never confirmed, or one whose switch has been turned
/// since, would carry the hop's profile over weaker algorithms than the
/// user agreed to. The caller has resolved the target's own profile by
/// then, and has connected to nothing.
async fn resolve_jump_chain(
    db: &dyn DatabaseProvider,
    cred_db: &dyn DatabaseProvider,
    session: &Session,
    pg_pool: Option<&sqlx::PgPool>,
    mappings: &CredentialMappings<'_>,
    pg_user: Option<&str>,
    pins: &LoginPinState,
) -> Result<Vec<crate::ssh::JumpHop>, String> {
    let mut hops = Vec::new();
    let mut visited = HashSet::new();
    let mut current_jump_id = session.jump_host_id;
    // The session whose row names the hop being looked at: the target first,
    // then each hop for the one before it.
    let mut named_by = (session.name.clone(), session.owner.clone());
    // Whether the session connected to has been held to what the user
    // confirmed of it, which the first hop with a profile of the user's
    // asks for.
    let mut target_confirmed = false;

    while let Some(jump_id) = current_jump_id {
        if !visited.insert(jump_id) {
            return Err(format!(
                "Circular jump host chain detected at session {jump_id}"
            ));
        }
        if hops.len() >= MAX_JUMP_HOPS {
            return Err("Jump host chain too deep".to_string());
        }

        let jump_session = db.get_session(jump_id).await?.ok_or_else(|| {
            "Jump host session not found. It may be a personal session belonging \
             to another user — ask the owner to share it."
                .to_string()
        })?;

        // As for the session connected to: no port that would be cut down
        // to another one.
        validate_port(jump_session.port).map_err(|e| {
            format!(
                "Jump host \"{}\": {e}",
                crate::login_pins::name_for_message(&jump_session.name)
            )
        })?;

        if !jump_host_is_its_owners(pg_pool.is_some(), &named_by.1, &jump_session.owner) {
            debug!(hop = hops.len(), jump_id = %jump_id, "Jump hop is not a session of the owner of the session that names it");
            return Err(format!(
                "\"{}\" names \"{}\" as its jump host, which is not a session \
                 of the same owner. A jump host has to be one, so this \
                 connection is not opened. The owner of \"{}\" can pick one of \
                 their own sessions as its jump host.",
                named_by.0, jump_session.name, named_by.0
            ));
        }

        debug!(
            hop = hops.len(),
            jump_id = %jump_id,
            host = %jump_session.hostname,
            port = jump_session.port,
            "Resolving jump hop"
        );

        // In PG mode, resolve per-user credential mapping for the hop. The
        // profile the hop's own row names counts only where the hop is this
        // user's own session; see `row_defaults_apply`.
        let row_defaults = row_defaults_apply(pg_pool.is_some(), pg_user, &jump_session);
        let row_credential_profile = jump_session.credential_profile_id.filter(|_| row_defaults);
        let mapped_profile_id = mappings.of(jump_id).await?;
        // A profile the user assigned to a jump host that is not their own
        // session is used only for what they confirmed that session names,
        // before the profile is read; see `login_pins.rs`.
        if needs_confirmation(pg_pool.is_some(), row_defaults, mapped_profile_id.is_some()) {
            if !target_confirmed {
                pins.check(session, UsedAs::Target, pg_user.is_some())?;
                target_confirmed = true;
            }
            pins.check(&jump_session, UsedAs::JumpHost, pg_user.is_some())?;
        }
        let effective_profile_id = mapped_profile_id.or(row_credential_profile);

        let profile_id = effective_profile_id.ok_or_else(|| {
            format!(
                "No credential profile assigned to jump host \"{}\". {}",
                jump_session.name,
                no_profile_advice(
                    row_defaults,
                    "Open the Credentials Manager to create one, then edit the \
                     session to link it."
                )
            )
        })?;
        let jump_profile = cred_db
            .get_credential_profile(profile_id)
            .await?
            .ok_or_else(|| {
                format!(
                    "Credential profile for jump host \"{}\" no longer exists. {}",
                    jump_session.name,
                    other_profile_advice(row_defaults)
                )
            })?;
        let (username, auth_method, auth_credential) = profile_to_auth(&jump_profile)?;

        hops.push(crate::ssh::JumpHop {
            host: jump_session.hostname.clone(),
            port: jump_session.port as u16,
            username,
            auth_method,
            auth_credential,
        });

        current_jump_id = jump_session.jump_host_id;
        named_by = (jump_session.name, jump_session.owner);
    }

    // The chain is built from target→bastion order, but SSH needs bastion→target.
    hops.reverse();
    debug!(total_hops = hops.len(), "Jump chain resolved");
    Ok(hops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::NewCredentialProfile;
    use crate::db::sqlite::SqliteProvider;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    fn session_of(owner: &str) -> Session {
        Session {
            id: Uuid::new_v4(),
            folder_id: Uuid::new_v4(),
            name: "session".to_string(),
            hostname: "host.example".to_string(),
            port: 22,
            protocol: "ssh".to_string(),
            username: String::new(),
            auth_method: "password".to_string(),
            jump_host_id: None,
            tags: "[]".to_string(),
            icon: "desktop".to_string(),
            sort_order: 0,
            highlight_profile_id: None,
            credential_profile_id: Some(Uuid::new_v4()),
            legacy_algorithms: false,
            login_sequence_id: Some(Uuid::new_v4()),
            owner: owner.to_string(),
            visibility: "shared".to_string(),
        }
    }

    #[test]
    fn a_row_names_a_profile_for_its_owner_only() {
        // `SQLite`: every session is the user's own, whatever its owner says.
        assert!(row_defaults_apply(false, None, &session_of("local")));
        assert!(row_defaults_apply(false, Some("alice"), &session_of("bob")));
        // `PostgreSQL`: the user's own session, but not another user's shared
        // one, and no session at all while the role is not known.
        assert!(row_defaults_apply(
            true,
            Some("alice"),
            &session_of("alice")
        ));
        assert!(!row_defaults_apply(true, Some("alice"), &session_of("bob")));
        assert!(!row_defaults_apply(
            true,
            Some("alice"),
            &session_of("Alice")
        ));
        assert!(!row_defaults_apply(true, None, &session_of("alice")));
        assert!(!row_defaults_apply(true, None, &session_of("")));
        // The placeholder owner is no role's, also not that of a role named
        // like it.
        assert!(!row_defaults_apply(
            true,
            Some("local"),
            &session_of("local")
        ));
        // The advice to edit a session is for its owner; anyone else is
        // told where their own profile is assigned.
        assert_eq!(
            no_profile_advice(true, "Edit the session."),
            "Edit the session."
        );
        let advice = no_profile_advice(false, "Edit the session.");
        assert!(advice.contains("only for its owner"));
        assert!(advice.contains("My Login"));
        assert!(advice.contains("owner chooses the host"));
        assert!(!advice.contains("Edit the session"));
        // It is also what a row with the placeholder owner gets, so it does
        // not call the session another user's.
        assert!(!advice.contains("another user"));
        // The same goes for a profile that is gone.
        assert!(other_profile_advice(true).contains("Edit that session"));
        assert!(other_profile_advice(false).contains("My Login"));
        assert!(!other_profile_advice(false).contains("Edit"));
    }

    #[test]
    fn a_login_sequence_types_a_password_and_no_key_path() {
        assert_eq!(sequence_password("password", "hunter2"), "hunter2");
        assert_eq!(
            sequence_password("publickey", "/home/a/.ssh/id_ed25519"),
            ""
        );
        assert_eq!(sequence_password("keyboard-interactive", ""), "");
        assert_eq!(sequence_password("", "x"), "");
    }

    #[test]
    fn a_jump_host_is_a_session_of_the_same_owner() {
        // `SQLite`: every session is the user's own, whatever its owner says.
        assert!(jump_host_is_its_owners(false, "local", "local"));
        assert!(jump_host_is_its_owners(false, "alice", "bob"));
        // `PostgreSQL`: the jump host of a session is its owner's, whoever
        // connects. Another user's is none, and role names are not folded.
        assert!(jump_host_is_its_owners(true, "alice", "alice"));
        assert!(!jump_host_is_its_owners(true, "alice", "bob"));
        assert!(!jump_host_is_its_owners(true, "bob", "alice"));
        assert!(!jump_host_is_its_owners(true, "alice", "Alice"));
        // A database from before rows had an owner, where every row still
        // has the placeholder: there is one owner, and no one to keep apart.
        assert!(jump_host_is_its_owners(true, "local", "local"));
        assert!(!jump_host_is_its_owners(true, "local", "alice"));
    }

    /// Nothing confirmed, and a file that is never written: these chains
    /// are of the user's own sessions, which need no confirming.
    fn no_pins() -> LoginPinState {
        LoginPinState::new(std::env::temp_dir().join(format!("ss-no-pins-{}", Uuid::new_v4())))
    }

    /// The error of a chain that does not resolve.
    async fn chain_error(
        db: &SqliteProvider,
        target: Uuid,
        pg_pool: Option<&sqlx::PgPool>,
    ) -> String {
        let target = db.get_session(target).await.unwrap().unwrap();
        resolve_jump_chain(
            db,
            db,
            &target,
            pg_pool,
            &CredentialMappings::None,
            Some("alice"),
            &no_pins(),
        )
        .await
        .err()
        .expect("the chain resolved")
    }

    #[tokio::test]
    async fn a_chain_fails_at_the_first_jump_host_of_another_owner() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(":memory:"))
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        let db = SqliteProvider::new(pool.clone());
        // What says that this is `PostgreSQL` mode. No server is contacted:
        // the pool connects at its first statement, and without a user
        // identity no mapping is looked up.
        let pg = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/none")
            .unwrap();

        let folder = db
            .create_folder(NewFolder {
                name: "folder".to_string(),
                parent_id: None,
            })
            .await
            .unwrap()
            .id;
        let profile = db
            .create_credential_profile(NewCredentialProfile {
                name: "key".to_string(),
                auth_type: "key".to_string(),
                username: "admin".to_string(),
                key_path: "id_ed25519".to_string(),
            })
            .await
            .unwrap()
            .id;
        // A session of `owner`, which a `PostgreSQL` row has and the `SQLite`
        // provider writes as the placeholder.
        let session = |name: &'static str,
                       owner: &'static str,
                       jump_host_id: Option<Uuid>,
                       credential_profile_id: Option<Uuid>| {
            let (db, pool) = (&db, &pool);
            async move {
                let id = db
                    .create_session(NewSession {
                        folder_id: folder,
                        name: name.to_string(),
                        hostname: format!("{name}.example"),
                        port: 22,
                        protocol: "ssh".to_string(),
                        username: String::new(),
                        auth_method: "password".to_string(),
                        jump_host_id,
                        tags: "[]".to_string(),
                        icon: "desktop".to_string(),
                        highlight_profile_id: None,
                        credential_profile_id,
                        login_sequence_id: None,
                        legacy_algorithms: false,
                    })
                    .await
                    .unwrap()
                    .id;
                sqlx::query("UPDATE sessions SET owner = ? WHERE id = ?")
                    .bind(owner)
                    .bind(id.to_string())
                    .execute(pool)
                    .await
                    .unwrap();
                id
            }
        };

        // target (alice) -> near (alice) -> far (bob), and `far` has no
        // profile.
        let far = session("far", "bob", None, None).await;
        let near = session("near", "alice", Some(far), Some(profile)).await;
        let target = session("target", "alice", Some(near), None).await;

        // The link that is refused is the one from `near` to `far`, whether
        // it is the first of the chain or a later one, and the error names
        // its two sessions. It comes before `far`'s profile is looked up, or
        // the missing profile would be the error.
        for from in [near, target] {
            let error = chain_error(&db, from, Some(&pg)).await;
            assert!(
                error.starts_with("\"near\" names \"far\" as its jump host"),
                "{error}"
            );
            assert!(error.contains("not a session of the same owner"), "{error}");
        }

        // `SQLite` mode has no such rule: the walk goes on to `far`.
        let error = chain_error(&db, target, None).await;
        assert!(
            error.starts_with("No credential profile assigned to jump host \"far\""),
            "{error}"
        );

        // A chain of one owner resolves, bastion first.
        let far = session("far2", "alice", None, Some(profile)).await;
        let near = session("near2", "alice", Some(far), Some(profile)).await;
        let target = session("target2", "alice", Some(near), None).await;
        let target = db.get_session(target).await.unwrap().unwrap();
        let hops = resolve_jump_chain(
            &db,
            &db,
            &target,
            Some(&pg),
            &CredentialMappings::None,
            Some("alice"),
            &no_pins(),
        )
        .await
        .expect("a chain of one owner did not resolve");
        let hosts: Vec<&str> = hops.iter().map(|hop| hop.host.as_str()).collect();
        assert_eq!(hosts, ["far2.example", "near2.example"]);

        // Another user's shared session and its jump host, which `alice`
        // connects to with a profile of her own assigned to the jump host.
        // The profile is used only for what she confirmed the jump host
        // names; see `login_pins.rs`.
        let bastion = session("bastion", "bob", None, None).await;
        let shared = session("shared", "bob", Some(bastion), None).await;
        let shared = db.get_session(shared).await.unwrap().unwrap();
        let assigned = CredentialMappings::Fixed([(bastion, profile)].into());
        let dir = crate::login_pins::tests::TestDir::new();
        let pins = LoginPinState::new(dir.file());
        let through_bastion = |pins| {
            let (db, shared, pg, assigned) = (&db, &shared, &pg, &assigned);
            async move {
                resolve_jump_chain(db, db, shared, Some(pg), assigned, Some("alice"), pins).await
            }
        };

        // Never confirmed: refused, before the profile is read. Nothing is
        // assigned to the session itself, but the profile for its jump host
        // would be used with the session's switch for legacy algorithms, so
        // the session has to be confirmed too.
        let error = through_bastion(&pins).await.err().expect("not refused");
        assert!(
            error.starts_with("Session \"shared\" is not your own, and you have not confirmed"),
            "{error}"
        );
        // The session confirmed and its jump host not: refused at that.
        pins.confirm(
            (shared.id, crate::login_pins::SessionPin::of(&shared)),
            Vec::new(),
        )
        .unwrap();
        let error = through_bastion(&pins).await.err().expect("not refused");
        assert!(
            error.starts_with("Jump host \"bastion\" is not your own, and you have not confirmed"),
            "{error}"
        );
        // Both confirmed, each as what it is: the chain resolves.
        let row = db.get_session(bastion).await.unwrap().unwrap();
        pins.confirm(
            (shared.id, crate::login_pins::SessionPin::of(&shared)),
            vec![(bastion, crate::login_pins::JumpHostPin::of(&row))],
        )
        .unwrap();
        let hops = through_bastion(&pins)
            .await
            .expect("refused though confirmed");
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].host, "bastion.example");
        // Another session of the same owner through the same jump host, which
        // was never confirmed: the profile for the jump host is not used
        // through it, whatever its switch says.
        let other = session("other", "bob", Some(bastion), None).await;
        let other = db.get_session(other).await.unwrap().unwrap();
        let error =
            resolve_jump_chain(&db, &db, &other, Some(&pg), &assigned, Some("alice"), &pins)
                .await
                .err()
                .expect("not refused");
        assert!(
            error.starts_with("Session \"other\" is not your own, and you have not confirmed"),
            "{error}"
        );
        // And the confirmed session with its switch turned on since.
        let mut switched = shared.clone();
        switched.legacy_algorithms = true;
        let error = resolve_jump_chain(
            &db,
            &db,
            &switched,
            Some(&pg),
            &assigned,
            Some("alice"),
            &pins,
        )
        .await
        .err()
        .expect("not refused");
        assert!(error.contains("legacy algorithms off → on"), "{error}");
        // Its owner has it name another host: refused again, and told why.
        sqlx::query("UPDATE sessions SET hostname = 'elsewhere.example' WHERE id = ?")
            .bind(bastion.to_string())
            .execute(&pool)
            .await
            .unwrap();
        let error = through_bastion(&pins).await.err().expect("not refused");
        assert!(
            error.contains("host bastion.example → elsewhere.example"),
            "{error}"
        );
        // Without a known role nothing of hers is used.
        let error = resolve_jump_chain(&db, &db, &shared, Some(&pg), &assigned, None, &pins)
            .await
            .err()
            .expect("not refused");
        assert!(error.contains("Restart ShellStation"), "{error}");
        // And with nothing assigned there is nothing to confirm: the jump
        // host has no profile for her, which is the error.
        let error = resolve_jump_chain(
            &db,
            &db,
            &shared,
            Some(&pg),
            &CredentialMappings::None,
            Some("alice"),
            &pins,
        )
        .await
        .err()
        .expect("not refused");
        assert!(
            error.starts_with("No credential profile assigned to jump host \"bastion\""),
            "{error}"
        );
        // In `SQLite` mode every session is the user's own, whatever its
        // owner says, so there the row's own profile counts and nothing is
        // confirmed; this one's row names none.
        let error = resolve_jump_chain(
            &db,
            &db,
            &shared,
            None,
            &CredentialMappings::None,
            Some("alice"),
            &pins,
        )
        .await
        .err()
        .expect("not refused");
        assert!(
            error.starts_with("No credential profile assigned to jump host \"bastion\""),
            "{error}"
        );
    }
}
