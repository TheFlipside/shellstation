use std::collections::HashMap;
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::config::{self, AppConfig, ConfigState, DbBackend, PostgresConfig};
use crate::db::models::{ExportData, Folder, NewFolder, NewSession};
use crate::db::{CredentialDbState, DbState};

/// Status of the database backend at startup.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbStatus {
    pub backend: String,
    pub healthy: bool,
    pub error: Option<String>,
    /// PostgreSQL `current_user` — populated only in PostgreSQL mode.
    pub pg_user: Option<String>,
}

/// Tauri managed state for DB health status.
pub struct DbStatusState(pub DbStatus);

/// Relaunch the current application binary. Used by settings after a
/// change that requires a fresh process (e.g. swapping DB backends).
#[tauri::command]
pub fn app_restart(app: AppHandle) {
    app.restart();
}

// ── Config commands ──────────────────────────────────────────────────

#[tauri::command]
pub async fn db_get_config(state: State<'_, ConfigState>) -> Result<AppConfig, String> {
    let mut config = state
        .config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {e}"))?
        .clone();
    // Populate PostgreSQL password from OS keychain for the settings UI.
    config.postgres.password =
        crate::credentials::retrieve(config::PG_PASSWORD_KEYCHAIN_REF).unwrap_or_default();
    Ok(config)
}

#[tauri::command]
pub async fn db_get_status(state: State<'_, DbStatusState>) -> Result<DbStatus, String> {
    Ok(state.0.clone())
}

// ── User identity commands (multi-user PostgreSQL mode) ─────────────

/// Maximum length for the user identity string.
const MAX_USER_IDENT_LEN: usize = 128;

/// Return the configured user identity for multi-user credential mapping.
#[tauri::command]
pub async fn get_user_ident(state: State<'_, ConfigState>) -> Result<Option<String>, String> {
    let config = state
        .config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {e}"))?;
    Ok(config.user_ident.clone())
}

/// Set the user identity and persist it to config.json.
#[tauri::command]
pub async fn set_user_ident(
    state: State<'_, ConfigState>,
    user_ident: String,
) -> Result<(), String> {
    let trimmed = user_ident.trim().to_string();
    if trimmed.is_empty() {
        return Err("User identity must not be empty".to_string());
    }
    if trimmed.len() > MAX_USER_IDENT_LEN {
        return Err(format!(
            "User identity too long (max {MAX_USER_IDENT_LEN} characters)"
        ));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err("User identity must not contain control characters".to_string());
    }
    let mut config = state
        .config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {e}"))?;
    config.user_ident = Some(trimmed);
    config::save_config(&state.config_path, &config)
}

/// Return the current OS username for pre-filling the user identity prompt.
#[tauri::command]
pub async fn get_os_username() -> Result<String, String> {
    #[cfg(unix)]
    {
        Ok(std::env::var("USER").unwrap_or_else(|_| "user".to_string()))
    }
    #[cfg(windows)]
    {
        Ok(std::env::var("USERNAME").unwrap_or_else(|_| "user".to_string()))
    }
}

#[tauri::command]
pub async fn db_test_connection(
    host: String,
    port: u16,
    database: String,
    username: String,
    password: String,
    ssl_mode: Option<String>,
) -> Result<String, String> {
    let ssl = ssl_mode.unwrap_or_else(|| "prefer".to_string());
    PostgresConfig::validate_ssl_mode(&ssl)?;

    let pg_config = PostgresConfig {
        host,
        port,
        database,
        username,
        password,
        ssl_mode: ssl,
    };

    // First, try connecting to the requested database directly.
    let pg_opts = pg_config.connect_options();
    let result = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(pg_opts)
        .await;

    match result {
        Ok(pool) => {
            sqlx::query("SELECT 1")
                .execute(&pool)
                .await
                .map_err(|_| "Connection succeeded but test query failed".to_string())?;
            pool.close().await;
            Ok("ok".to_string())
        }
        Err(_) => {
            // The target database may not exist yet.  Try well-known
            // maintenance databases with the same credentials to distinguish
            // "server unreachable / bad credentials" from "database missing".
            // Not all servers have "postgres" — some only have "template1" or
            // default to a database named after the user.
            let fallback_dbs = ["postgres", "template1", pg_config.username.as_str()];

            for db_name in fallback_dbs {
                let fallback_opts = PostgresConfig {
                    database: db_name.to_string(),
                    ..pg_config.clone()
                }
                .connect_options();

                let fallback = PgPoolOptions::new()
                    .max_connections(1)
                    .acquire_timeout(Duration::from_secs(5))
                    .connect_with(fallback_opts)
                    .await;

                if let Ok(pool) = fallback {
                    let query_ok = sqlx::query("SELECT 1").execute(&pool).await.is_ok();
                    pool.close().await;
                    if query_ok {
                        return Ok("db_not_found".to_string());
                    }
                }
            }

            Err("Connection failed: unable to connect to PostgreSQL server".to_string())
        }
    }
}

#[tauri::command]
pub async fn db_create_database(
    host: String,
    port: u16,
    database: String,
    username: String,
    password: String,
    ssl_mode: Option<String>,
) -> Result<String, String> {
    // Validate the requested database name: only allow ASCII alphanumerics,
    // underscores, and hyphens to prevent SQL injection.  The CREATE DATABASE
    // statement uses quote_ident–style quoting as an extra safety layer, but
    // restricting the character set upfront is the primary defence.
    if database.is_empty() || database.len() > 63 {
        return Err("Database name must be 1–63 characters".to_string());
    }
    if !database
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(
            "Database name may only contain ASCII letters, digits, underscores, and hyphens"
                .to_string(),
        );
    }

    let ssl = ssl_mode.unwrap_or_else(|| "prefer".to_string());
    PostgresConfig::validate_ssl_mode(&ssl)?;

    // Connect to a maintenance database to issue the CREATE DATABASE command.
    // Not all servers have "postgres" — try well-known fallbacks.
    let pg_config = PostgresConfig {
        host,
        port,
        database: String::new(),
        username,
        password,
        ssl_mode: ssl,
    };

    let fallback_dbs = ["postgres", "template1", pg_config.username.as_str()];

    let mut pool = None;
    for db_name in fallback_dbs {
        let opts = PostgresConfig {
            database: db_name.to_string(),
            ..pg_config.clone()
        }
        .connect_options();

        if let Ok(p) = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(opts)
            .await
        {
            pool = Some(p);
            break;
        }
    }

    let pool = pool
        .ok_or_else(|| "Connection failed: unable to connect to PostgreSQL server".to_string())?;

    // Use PG's quote_ident equivalent: double-quote the identifier and
    // escape any embedded double-quotes.
    let quoted = format!("\"{}\"", database.replace('"', "\"\""));
    let sql = format!("CREATE DATABASE {quoted}");

    sqlx::query(&sql)
        .execute(&pool)
        .await
        .map_err(|e| format!("Failed to create database: {e}"))?;

    pool.close().await;
    Ok("ok".to_string())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn db_save_config(
    state: State<'_, ConfigState>,
    backend: String,
    sqlite_path: Option<String>,
    host: String,
    port: u16,
    database: String,
    username: String,
    password: String,
    ssl_mode: Option<String>,
) -> Result<(), String> {
    let db_backend = match backend.as_str() {
        "sqlite" => DbBackend::Sqlite,
        "postgres" => DbBackend::Postgres,
        other => return Err(format!("Unknown backend: {other}")),
    };

    // Normalize empty string to None so the config file stays clean.
    let sqlite_path = sqlite_path.filter(|p| !p.trim().is_empty());

    // Validate the custom SQLite path before persisting.
    if let Some(ref path_str) = sqlite_path {
        let path = std::path::Path::new(path_str);

        // Must be an absolute path to prevent ambiguity.
        if !path.is_absolute() {
            return Err("SQLite path must be absolute".to_string());
        }

        // Must not point to an existing directory (needs to be a file path).
        if path.is_dir() {
            return Err(
                "Path points to a directory. Please specify a file, e.g. /path/to/sessions.db"
                    .to_string(),
            );
        }

        // The parent directory must exist so SQLite can create the file.
        // Canonicalize the parent to resolve symlinks and `..` sequences,
        // preventing directory traversal attacks.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                let canonical_parent = std::fs::canonicalize(parent).map_err(|_| {
                    format!("Parent directory does not exist: {}", parent.display())
                })?;
                if !canonical_parent.is_dir() {
                    return Err(format!(
                        "Parent directory does not exist: {}",
                        parent.display()
                    ));
                }
            }
        }
    }

    let ssl = ssl_mode.unwrap_or_else(|| "prefer".to_string());
    PostgresConfig::validate_ssl_mode(&ssl)?;

    // Preserve existing logging configs when saving DB settings.
    let (existing_logging, existing_app_logging) = {
        let cfg = state
            .config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {e}"))?;
        (cfg.logging.clone(), cfg.app_logging.clone())
    };

    // Store the PostgreSQL password in the OS keychain, not in config.json.
    // Verify the round-trip: after storing, immediately retrieve and compare.
    // This catches cases where the keychain backend silently corrupts or
    // truncates the value, so the user finds out at save time instead of on
    // the next app launch.
    if db_backend == DbBackend::Postgres {
        if let Err(e) = crate::credentials::store(config::PG_PASSWORD_KEYCHAIN_REF, &password) {
            tracing::error!("Failed to store PostgreSQL password in keychain: {e}");
            return Err(format!("Failed to store database password securely: {e}"));
        }
        match crate::credentials::retrieve(config::PG_PASSWORD_KEYCHAIN_REF) {
            Ok(round_tripped)
                if round_tripped.len() == password.len()
                    && subtle::ConstantTimeEq::ct_eq(
                        round_tripped.as_bytes(),
                        password.as_bytes(),
                    )
                    .into() =>
            {
                tracing::info!("PostgreSQL password stored and verified in keychain");
            }
            Ok(_) => {
                tracing::error!("PostgreSQL keychain round-trip verification failed");
                return Err(
                    "Keychain round-trip verification failed: the OS keychain returned a \
                     different value than what was stored. Database password not saved."
                        .to_string(),
                );
            }
            Err(e) => {
                tracing::error!("Keychain round-trip read-back failed: {e}");
                return Err(format!(
                    "Keychain round-trip verification failed: {e}. Database password not saved."
                ));
            }
        }
    }

    // Preserve user_ident across config saves.
    let existing_user_ident = state
        .config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {e}"))?
        .user_ident
        .clone();

    let new_config = AppConfig {
        db_backend,
        sqlite_path,
        postgres: PostgresConfig {
            host,
            port,
            database,
            username,
            password,
            ssl_mode: ssl,
        },
        logging: existing_logging,
        app_logging: existing_app_logging,
        user_ident: existing_user_ident,
    };

    config::save_config(&state.config_path, &new_config)?;

    let mut config = state
        .config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {e}"))?;
    *config = new_config;

    Ok(())
}

// ── Export / Import ──────────────────────────────────────────────────

async fn build_export_data(
    state: &DbState,
    cred_db: &CredentialDbState,
) -> Result<ExportData, String> {
    let folders = state.0.list_folders().await?;
    let sessions = state.0.list_all_sessions().await?;
    let credentials = cred_db.0.list_all_credentials().await?;
    let highlight_profiles = state.0.list_highlight_profiles().await?;
    let credential_profiles = cred_db.0.list_credential_profiles().await?;

    // Credentials contain only metadata (username, auth_type, keychain_ref).
    // Secrets are stored in the OS keychain and never exported.
    Ok(ExportData {
        folders,
        sessions,
        credentials,
        highlight_profiles,
        credential_profiles,
    })
}

#[tauri::command]
pub async fn db_export(
    state: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
) -> Result<ExportData, String> {
    build_export_data(&state, &cred_db).await
}

#[tauri::command]
pub async fn db_export_file(
    state: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
    path: String,
) -> Result<String, String> {
    let dest = std::path::Path::new(&path);

    if !dest.is_absolute() {
        return Err("Export path must be absolute".to_string());
    }

    let file_name = dest
        .file_name()
        .ok_or_else(|| "Export path must include a file name".to_string())?;

    // Canonicalize the parent directory to resolve symlinks and prevent
    // directory traversal, then reconstruct the final path from the
    // canonical parent + filename so the write target is fully resolved.
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| "Export path has no parent directory".to_string())?;

    let canonical_parent = std::fs::canonicalize(parent)
        .map_err(|_| format!("Parent directory does not exist: {}", parent.display()))?;
    if !canonical_parent.is_dir() {
        return Err(format!(
            "Parent path is not a directory: {}",
            canonical_parent.display()
        ));
    }

    let final_path = canonical_parent.join(file_name);

    let data = build_export_data(&state, &cred_db).await?;
    let json = serde_json::to_string_pretty(&data)
        .map_err(|e| format!("Failed to serialize export data: {e}"))?;
    std::fs::write(&final_path, json).map_err(|e| format!("Failed to write export file: {e}"))?;
    Ok(format!(
        "Exported {} folders, {} sessions",
        data.folders.len(),
        data.sessions.len()
    ))
}

/// Maximum number of items allowed in a single import to prevent resource exhaustion.
const MAX_IMPORT_FOLDERS: usize = 10_000;
const MAX_IMPORT_SESSIONS: usize = 50_000;
const MAX_IMPORT_CREDENTIALS: usize = 50_000;

/// Validate imported data for size limits and field constraints.
fn validate_import_data(data: &ExportData) -> Result<(), String> {
    if data.folders.len() > MAX_IMPORT_FOLDERS {
        return Err(format!(
            "Too many folders in import ({}, max {})",
            data.folders.len(),
            MAX_IMPORT_FOLDERS
        ));
    }
    if data.sessions.len() > MAX_IMPORT_SESSIONS {
        return Err(format!(
            "Too many sessions in import ({}, max {})",
            data.sessions.len(),
            MAX_IMPORT_SESSIONS
        ));
    }
    if data.credentials.len() > MAX_IMPORT_CREDENTIALS {
        return Err(format!(
            "Too many credentials in import ({}, max {})",
            data.credentials.len(),
            MAX_IMPORT_CREDENTIALS
        ));
    }

    // Validate individual field lengths.
    for folder in &data.folders {
        if folder.name.len() > 255 {
            let preview: String = folder.name.chars().take(50).collect();
            return Err(format!(
                "Folder name too long: \"{preview}...\" (max 255 chars)",
            ));
        }
    }
    for session in &data.sessions {
        if session.name.len() > 255 {
            let preview: String = session.name.chars().take(50).collect();
            return Err(format!(
                "Session name too long: \"{preview}...\" (max 255 chars)",
            ));
        }
        if session.hostname.len() > 255 {
            return Err("Session hostname too long (max 255 chars)".to_string());
        }
        if session.tags.len() > 1024 {
            return Err("Session tags too long (max 1024 chars)".to_string());
        }
        if !(1..=65535).contains(&session.port) {
            return Err(format!(
                "Invalid port {} in session \"{}\"",
                session.port, session.name
            ));
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn db_import(
    state: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
    data: ExportData,
) -> Result<String, String> {
    validate_import_data(&data)?;

    let mut folder_count = 0u32;
    let mut skipped_folders = 0u32;
    let mut session_count = 0u32;
    let mut skipped_sessions = 0u32;
    let mut credential_count = 0u32;

    // Snapshot existing data so we can detect duplicates and merge.
    let existing_folders = state
        .0
        .list_folders()
        .await
        .map_err(|e| format!("Failed to list existing folders: {e}"))?;
    let existing_sessions = state
        .0
        .list_all_sessions()
        .await
        .map_err(|e| format!("Failed to list existing sessions: {e}"))?;

    // Index existing folders by (name, parent_id) for dedup lookups.
    let existing_folder_key: HashMap<(String, Option<Uuid>), Uuid> = existing_folders
        .iter()
        .map(|f| ((f.name.clone(), f.parent_id), f.id))
        .collect();

    // Import folders — must respect parent ordering (parents before children).
    // Build an ID remap table because create_folder generates new UUIDs.
    let ordered_folders = topological_sort_folders(&data.folders);
    let mut folder_id_map: HashMap<Uuid, Uuid> = HashMap::new();
    for folder in &ordered_folders {
        // If the parent failed to import (or was dropped by topo sort),
        // promote the folder to the root rather than carrying a dangling
        // FK reference that would be rejected by the database.
        let remapped_parent = folder
            .parent_id
            .and_then(|pid| folder_id_map.get(&pid).copied());

        // Reuse an existing folder with the same name under the same parent.
        let key = (folder.name.clone(), remapped_parent);
        if let Some(&existing_id) = existing_folder_key.get(&key) {
            folder_id_map.insert(folder.id, existing_id);
            skipped_folders += 1;
            continue;
        }

        let created = state
            .0
            .create_folder(NewFolder {
                name: folder.name.clone(),
                parent_id: remapped_parent,
            })
            .await
            .map_err(|e| format!("Failed to import folder '{}': {e}", folder.name))?;
        folder_id_map.insert(folder.id, created.id);
        folder_count += 1;
    }

    // Index existing sessions by (hostname, port, folder_id) for dedup.
    let existing_session_key: HashMap<(String, i32, Uuid), Uuid> = existing_sessions
        .iter()
        .map(|s| ((s.hostname.clone(), s.port, s.folder_id), s.id))
        .collect();

    // Import sessions — remap folder_id and jump_host_id references.
    // First pass: create all sessions and build an ID remap for jump hosts.
    let mut session_id_map: HashMap<Uuid, Uuid> = HashMap::new();
    for session in &data.sessions {
        let Some(remapped_folder) = folder_id_map.get(&session.folder_id).copied() else {
            return Err(format!(
                "Failed to import session '{}': parent folder was not imported",
                session.name
            ));
        };

        // Skip sessions that already exist at the same host:port in the same folder.
        let key = (session.hostname.clone(), session.port, remapped_folder);
        if let Some(&existing_id) = existing_session_key.get(&key) {
            session_id_map.insert(session.id, existing_id);
            skipped_sessions += 1;
            continue;
        }

        let created = state
            .0
            .create_session(NewSession {
                folder_id: remapped_folder,
                name: session.name.clone(),
                hostname: session.hostname.clone(),
                port: session.port,
                protocol: session.protocol.clone(),
                username: session.username.clone(),
                auth_method: session.auth_method.clone(),
                jump_host_id: None,
                tags: session.tags.clone(),
                icon: session.icon.clone(),
                highlight_profile_id: None,
                credential_profile_id: None,
                legacy_algorithms: session.legacy_algorithms,
            })
            .await
            .map_err(|e| format!("Failed to import session '{}': {e}", session.name))?;
        session_id_map.insert(session.id, created.id);
        session_count += 1;
    }

    // Second pass: wire up jump_host_id references with remapped IDs.
    for session in &data.sessions {
        if let Some(jump_id) = session.jump_host_id {
            // Skip if either the session itself or the jump host wasn't
            // successfully imported — otherwise we'd write a dangling FK.
            let Some(new_session_id) = session_id_map.get(&session.id).copied() else {
                continue;
            };
            let Some(new_jump_id) = session_id_map.get(&jump_id).copied() else {
                continue;
            };
            state
                .0
                .update_session(
                    new_session_id,
                    crate::db::models::UpdateSession {
                        jump_host_id: Some(Some(new_jump_id)),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|e| format!("Failed to set jump host for '{}': {e}", session.name))?;
        }
    }

    // Import credential metadata into local store.
    // Secrets are not included in exports (stored in OS keychain) — users
    // must re-enter passwords/keys after import.
    for cred in &data.credentials {
        let mut remapped_cred = cred.clone();
        if let Some(&new_sid) = session_id_map.get(&cred.session_id) {
            remapped_cred.session_id = new_sid;
            remapped_cred.keychain_ref = format!("session-{new_sid}");
        }
        cred_db
            .0
            .upsert_credential(remapped_cred)
            .await
            .map_err(|e| format!("Failed to import credential: {e}"))?;
        credential_count += 1;
    }

    // Import highlight profiles.
    let mut profile_count = 0u32;
    for profile in &data.highlight_profiles {
        state
            .0
            .create_highlight_profile(crate::db::models::NewHighlightProfile {
                name: profile.name.clone(),
                rules: profile.rules.clone(),
            })
            .await
            .map_err(|e| format!("Failed to import highlight profile '{}': {e}", profile.name))?;
        profile_count += 1;
    }

    // Wire up highlight_profile_id for sessions (remap IDs).
    // Highlight profiles use the same ID in export — no remap needed since
    // create_highlight_profile generates new IDs. For simplicity, skip
    // highlight_profile_id remapping during generic import; the SecureCRT
    // highlight importer is the primary path for this data.

    let mut summary = format!(
        "Imported {folder_count} folders, {session_count} sessions, {credential_count} credentials, {profile_count} highlight profiles"
    );
    if skipped_folders > 0 || skipped_sessions > 0 {
        summary.push_str(&format!(
            " (skipped {skipped_folders} duplicate folders, {skipped_sessions} duplicate sessions)"
        ));
    }
    Ok(summary)
}

/// Topological sort: parents before children.
fn topological_sort_folders(folders: &[Folder]) -> Vec<Folder> {
    use std::collections::{HashMap, HashSet, VecDeque};

    let by_id: HashMap<Uuid, &Folder> = folders.iter().map(|f| (f.id, f)).collect();
    let mut sorted = Vec::with_capacity(folders.len());
    let mut visited = HashSet::new();
    let mut queue: VecDeque<&Folder> = folders
        .iter()
        .filter(|f| match f.parent_id {
            None => true,
            Some(pid) => !by_id.contains_key(&pid),
        })
        .collect();

    while let Some(folder) = queue.pop_front() {
        if !visited.insert(folder.id) {
            continue;
        }
        sorted.push(folder.clone());
        for f in folders {
            if f.parent_id == Some(folder.id) && !visited.contains(&f.id) {
                queue.push_back(f);
            }
        }
    }

    sorted
}
