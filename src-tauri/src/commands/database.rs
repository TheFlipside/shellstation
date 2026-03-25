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
    let pg = PostgresConfig {
        host,
        port,
        database,
        username,
        password,
    };
    let url = pg.connection_url();

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&url)
        .await
        .map_err(|e| format!("Connection failed: {e}"))?;

    sqlx::query("SELECT 1")
        .execute(&pool)
        .await
        .map_err(|e| format!("Query failed: {e}"))?;

    pool.close().await;
    Ok("ok".to_string())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn db_save_config(
    state: State<'_, ConfigState>,
    backend: String,
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

    let new_config = AppConfig {
        db_backend,
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

    Ok(ExportData {
        folders,
        sessions,
        credentials: credentials
            .into_iter()
            .map(ExportCredential::from)
            .collect(),
    })
}

#[tauri::command]
pub async fn db_import(
    state: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
    data: ExportData,
) -> Result<String, String> {
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
            })
            .await
            .map_err(|e| format!("Failed to import session '{}': {e}", session.name))?;
        session_count += 1;
    }

    // Import credentials into local store
    for cred in &data.credentials {
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
