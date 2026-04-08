use std::collections::HashMap;
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use tauri::State;
use uuid::Uuid;

use crate::config::{self, AppConfig, ConfigState, DbBackend, PostgresConfig};
use crate::db::models::{Credential, ExportCredential, ExportData, Folder, NewFolder, NewSession};
use crate::db::{CredentialDbState, DbState};

/// Status of the database backend at startup.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DbStatus {
    pub backend: String,
    pub healthy: bool,
    pub error: Option<String>,
}

/// Tauri managed state for DB health status.
pub struct DbStatusState(pub DbStatus);

// ── Config commands ──────────────────────────────────────────────────

#[tauri::command]
pub async fn db_get_config(state: State<'_, ConfigState>) -> Result<AppConfig, String> {
    let config = state
        .config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {e}"))?;
    Ok(config.clone())
}

#[tauri::command]
pub async fn db_get_status(state: State<'_, DbStatusState>) -> Result<DbStatus, String> {
    Ok(state.0.clone())
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

#[tauri::command]
pub async fn db_export(
    state: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
) -> Result<ExportData, String> {
    let folders = state.0.list_folders().await?;
    let sessions = state.0.list_all_sessions().await?;
    let credentials = cred_db.0.list_all_credentials().await?;

    // Strip secrets from exported credentials — export only metadata.
    // Secrets are local-only and must not leave the device.
    // Username is exported (not secret — needed for other users to know
    // what format to use when setting their own).
    let safe_creds: Vec<ExportCredential> = credentials
        .into_iter()
        .map(|c| ExportCredential {
            id: c.id,
            session_id: c.session_id,
            username: c.username,
            auth_type: c.auth_type,
            keychain_ref: c.keychain_ref,
            secret: String::new(),
        })
        .collect();

    Ok(ExportData {
        folders,
        sessions,
        credentials: safe_creds,
    })
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
            return Err(format!(
                "Folder name too long: \"{}...\" (max 255 chars)",
                &folder.name[..50]
            ));
        }
    }
    for session in &data.sessions {
        if session.name.len() > 255 {
            return Err(format!(
                "Session name too long: \"{}...\" (max 255 chars)",
                &session.name[..50]
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
        let remapped_parent = folder
            .parent_id
            .map(|pid| folder_id_map.get(&pid).copied().unwrap_or(pid));

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
        let remapped_folder = folder_id_map
            .get(&session.folder_id)
            .copied()
            .unwrap_or(session.folder_id);

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
                auth_method: session.auth_method.clone(),
                jump_host_id: None,
                tags: session.tags.clone(),
                icon: session.icon.clone(),
            })
            .await
            .map_err(|e| format!("Failed to import session '{}': {e}", session.name))?;
        session_id_map.insert(session.id, created.id);
        session_count += 1;
    }

    // Second pass: wire up jump_host_id references with remapped IDs.
    for session in &data.sessions {
        if let Some(jump_id) = session.jump_host_id {
            let new_session_id = session_id_map
                .get(&session.id)
                .copied()
                .unwrap_or(session.id);
            let new_jump_id = session_id_map.get(&jump_id).copied().unwrap_or(jump_id);
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

    // Import credentials into local store (skip entries with empty secrets
    // since exported data has secrets redacted for safety).
    // Remap session_id to the newly created session IDs.
    for cred in &data.credentials {
        if cred.secret.is_empty() {
            continue;
        }
        let mut remapped_cred = cred.clone();
        if let Some(&new_sid) = session_id_map.get(&cred.session_id) {
            remapped_cred.session_id = new_sid;
        }
        cred_db
            .0
            .upsert_credential(Credential::from(remapped_cred))
            .await
            .map_err(|e| format!("Failed to import credential: {e}"))?;
        credential_count += 1;
    }

    let mut summary = format!(
        "Imported {folder_count} folders, {session_count} sessions, {credential_count} credentials"
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
        .filter(|f| f.parent_id.is_none() || !by_id.contains_key(&f.parent_id.unwrap()))
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
