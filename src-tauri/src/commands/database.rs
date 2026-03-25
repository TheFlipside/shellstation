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
) -> Result<String, String> {
    let pg_opts = PostgresConfig {
        host,
        port,
        database,
        username,
        password,
    }
    .connect_options();

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect_with(pg_opts)
        .await
        .map_err(|_| "Connection failed: unable to connect to PostgreSQL server".to_string())?;

    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .map_err(|_| "Connection succeeded but test query failed".to_string())?;

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

        // Must not point to an existing directory (needs to be a file path).
        if path.is_dir() {
            return Err(
                "Path points to a directory. Please specify a file, e.g. /path/to/sessions.db"
                    .to_string(),
            );
        }

        // The parent directory must exist so SQLite can create the file.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() && !parent.is_dir() {
                return Err(format!(
                    "Parent directory does not exist: {}",
                    parent.display()
                ));
            }
        }
    }

    let new_config = AppConfig {
        db_backend,
        sqlite_path,
        postgres: PostgresConfig {
            host,
            port,
            database,
            username,
            password,
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
    let safe_creds: Vec<ExportCredential> = credentials
        .into_iter()
        .map(|c| ExportCredential {
            id: c.id,
            session_id: c.session_id,
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
        if session.username.len() > 128 {
            return Err("Session username too long (max 128 chars)".to_string());
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
    let mut session_count = 0u32;
    let mut credential_count = 0u32;

    // Import folders — must respect parent ordering (parents before children).
    // Sort by depth: folders with no parent first, then children.
    let ordered_folders = topological_sort_folders(&data.folders);
    for folder in &ordered_folders {
        state
            .0
            .create_folder(NewFolder {
                name: folder.name.clone(),
                parent_id: folder.parent_id,
            })
            .await
            .map_err(|e| format!("Failed to import folder '{}': {e}", folder.name))?;
        folder_count += 1;
    }

    // Import sessions
    for session in &data.sessions {
        state
            .0
            .create_session(NewSession {
                folder_id: session.folder_id,
                name: session.name.clone(),
                hostname: session.hostname.clone(),
                port: session.port,
                protocol: session.protocol.clone(),
                username: session.username.clone(),
                auth_method: session.auth_method.clone(),
                jump_host_id: session.jump_host_id,
                tags: session.tags.clone(),
                icon: session.icon.clone(),
            })
            .await
            .map_err(|e| format!("Failed to import session '{}': {e}", session.name))?;
        session_count += 1;
    }

    // Import credentials into local store (skip entries with empty secrets
    // since exported data has secrets redacted for safety).
    for cred in &data.credentials {
        if cred.secret.is_empty() {
            continue;
        }
        cred_db
            .0
            .upsert_credential(Credential::from(cred.clone()))
            .await
            .map_err(|e| format!("Failed to import credential: {e}"))?;
        credential_count += 1;
    }

    Ok(format!(
        "Imported {folder_count} folders, {session_count} sessions, {credential_count} credentials"
    ))
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
