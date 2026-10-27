use std::collections::{HashMap, HashSet};
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use tauri::{AppHandle, State};
use uuid::Uuid;

use crate::config::{self, AppConfig, ConfigState, DbBackend, PostgresConfig};
use crate::db::models::{
    Credential, ExportData, Folder, NewFolder, NewSession, Session, SESSION_AUTH_METHODS,
    SESSION_PROTOCOLS,
};
use crate::db::{CredentialDbState, DbState};

/// Status of the database backend at startup.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DbStatus {
    pub backend: String,
    pub healthy: bool,
    pub error: Option<String>,
    /// `PostgreSQL` `current_user` — populated only in `PostgreSQL` mode.
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
    // Never return the PostgreSQL password to the webview. The settings UI
    // shows an empty field; the keychain entry is preserved on save unless
    // the user types a replacement value.
    config.postgres.password.clear();
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
    if trimmed.chars().any(char::is_control) {
        return Err("User identity must not contain control characters".to_string());
    }
    let mut config = state
        .config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {e}"))?;
    config.user_ident = Some(trimmed);
    config::save_config(&state.config_path, &config)
}

/// Return the current `OS` username for pre-filling the user identity prompt.
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

    if let Ok(pool) = result {
        sqlx::query("SELECT 1")
            .execute(&pool)
            .await
            .map_err(|_| "Connection succeeded but test query failed".to_string())?;
        pool.close().await;
        Ok("ok".to_string())
    } else {
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

    // Defense-in-depth quote_ident equivalent: double-quote the identifier
    // and escape any embedded double-quotes. The character-set allowlist
    // above already rejects `"`, `\`, NUL, and every other character that
    // could break out of a double-quoted identifier, so this `replace()` is
    // a no-op on every input that reaches it today. It stays because if
    // the allowlist is ever widened (e.g. to permit dots for `schema.db`),
    // the next reviewer should not have to also remember to re-add the
    // escaping — it is already here, unconditionally, and the cost is
    // negligible.
    let quoted = format!("\"{}\"", database.replace('"', "\"\""));
    let sql = format!("CREATE DATABASE {quoted}");

    sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
        .execute(&pool)
        .await
        .map_err(|e| format!("Failed to create database: {e}"))?;

    pool.close().await;
    Ok("ok".to_string())
}

/// Persist the database configuration and rotate the `PostgreSQL` password
/// in the `OS` keychain.
///
/// ## Why this function is large
///
/// This is an IPC command that *must* finish atomically from the user's
/// perspective: either the new config and the new keychain entry are both
/// stored, or neither is. The branches are: validate backend / `SQLite` path
/// → conditionally store password in keychain with round-trip verification
/// → preserve existing logging + `user_ident` state under a separate lock →
/// emit the new `AppConfig` to disk. Each step is itself simple; the
/// complexity is in *ordering* them so a failure in step N can't leave the
/// keychain and config file in disagreement.
///
/// The empty-password branch is the only non-obvious bit: an empty
/// password means "keep the current keychain entry", not "wipe it". This
/// lets the settings UI load with the password field blank (so the value
/// never round-trips to the webview) and still let the user save unrelated
/// changes without re-entering the password.
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
        // The same rules as at startup (absolute, no `..`), so that a path
        // saved here is one the app will also open, rather than one it
        // replaces with the default database on the next start.
        let path = crate::validate_config_path(path_str, "SQLite path")?;

        // Must not point to an existing directory (needs to be a file path).
        if path.is_dir() {
            return Err(
                "Path points to a directory. Please specify a file, e.g. /path/to/sessions.db"
                    .to_string(),
            );
        }

        // The parent directory must exist so SQLite can create the file.
        if let Some(parent) = path.parent() {
            if !parent.is_dir() {
                return Err(format!(
                    "Parent directory does not exist: {}",
                    parent.display()
                ));
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
    // An empty password from the frontend means "keep the existing keychain
    // entry" — the settings UI does not display the stored password, so a
    // blank field is the normal case when the user is changing other fields.
    // Verify the round-trip: after storing, immediately retrieve and compare.
    // This catches cases where the keychain backend silently corrupts or
    // truncates the value, so the user finds out at save time instead of on
    // the next app launch.
    let resolved_password = if db_backend == DbBackend::Postgres {
        if password.is_empty() {
            // Preserve the existing keychain entry. If none exists, fall back
            // to an empty string so the save still proceeds; the test/connect
            // flow will surface the missing password to the user.
            crate::credentials::retrieve(config::PG_PASSWORD_KEYCHAIN_REF)
                .map(|z| (*z).clone())
                .unwrap_or_default()
        } else {
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
            password
        }
    } else {
        password
    };

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
            password: resolved_password,
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

pub(crate) async fn build_export_data(
    state: &DbState,
    cred_db: &CredentialDbState,
    login_seq_db: &crate::db::LoginSequenceDbState,
) -> Result<ExportData, String> {
    let folders = state.0.list_folders().await?;
    let sessions = state.0.list_all_sessions().await?;
    let credentials = cred_db.0.list_all_credentials().await?;
    let highlight_profiles = state.0.list_highlight_profiles().await?;
    let credential_profiles = cred_db.0.list_credential_profiles().await?;
    let login_sequences = login_seq_db.0.list_login_sequences().await?;

    // Credentials contain only metadata (username, auth_type, keychain_ref).
    // Secrets are stored in the OS keychain and never exported.
    Ok(ExportData {
        folders,
        sessions,
        credentials,
        highlight_profiles,
        credential_profiles,
        login_sequences,
    })
}

#[tauri::command]
pub async fn db_export(
    state: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
    login_seq_db: State<'_, crate::db::LoginSequenceDbState>,
) -> Result<ExportData, String> {
    build_export_data(&state, &cred_db, &login_seq_db).await
}

// Writing export files lives in `data_transfer.rs`, which wraps this data
// in an envelope together with the client settings and command buttons.

/// Maximum number of items allowed in a single import to prevent resource exhaustion.
/// The folder and session limits also apply to the `SecureCRT` and `mRemoteNG`
/// imports in `import/mod.rs`.
pub(crate) const MAX_IMPORT_FOLDERS: usize = 10_000;
pub(crate) const MAX_IMPORT_SESSIONS: usize = 50_000;
const MAX_IMPORT_CREDENTIALS: usize = 50_000;
const MAX_IMPORT_HIGHLIGHT_PROFILES: usize = 1_000;
const MAX_IMPORT_LOGIN_SEQUENCES: usize = 1_000;

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

    if data.highlight_profiles.len() > MAX_IMPORT_HIGHLIGHT_PROFILES {
        return Err(format!(
            "Too many highlight profiles in import ({}, max {})",
            data.highlight_profiles.len(),
            MAX_IMPORT_HIGHLIGHT_PROFILES
        ));
    }
    if data.login_sequences.len() > MAX_IMPORT_LOGIN_SEQUENCES {
        return Err(format!(
            "Too many login sequences in import ({}, max {})",
            data.login_sequences.len(),
            MAX_IMPORT_LOGIN_SEQUENCES
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
        // Only limits the app has always applied to what it stores, so that
        // an export always imports again; the file size bounds the rest.
        if session.icon.len() > super::MAX_ICON_LEN {
            return Err(format!(
                "Session icon too long (max {} chars)",
                super::MAX_ICON_LEN
            ));
        }
        // An unknown value would be stored and then fail on every connect.
        if !SESSION_PROTOCOLS.contains(&session.protocol.as_str()) {
            return Err(format!(
                "Unsupported protocol in session \"{}\"",
                session.name
            ));
        }
        if !SESSION_AUTH_METHODS.contains(&session.auth_method.as_str()) {
            return Err(format!(
                "Unsupported auth method in session \"{}\"",
                session.name
            ));
        }
    }

    for profile in &data.highlight_profiles {
        if profile.name.len() > 255 {
            let preview: String = profile.name.chars().take(50).collect();
            return Err(format!(
                "Highlight profile name too long: \"{preview}...\" (max 255 chars)",
            ));
        }
        // Rules must be a rule list, as every stored profile's are. The size
        // and count limits of creating one in the app are not applied: a
        // SecureCRT highlight import stores profiles without them.
        serde_json::from_str::<Vec<crate::highlight::HighlightRule>>(&profile.rules)
            .map_err(|e| format!("Highlight profile \"{}\": invalid rules: {e}", profile.name))?;
    }
    for seq in &data.login_sequences {
        if seq.name.len() > 255 {
            let preview: String = seq.name.chars().take(50).collect();
            return Err(format!(
                "Login sequence name too long: \"{preview}...\" (max 255 chars)",
            ));
        }
        super::login_sequence::check_step_limits(&seq.steps)
            .map_err(|e| format!("Login sequence \"{}\": {e}", seq.name))?;
    }

    Ok(())
}

/// Import a JSON export into the live database.
///
/// ## Why this function is large
///
/// `db_import` walks six independent entity types (folders, sessions,
/// credentials, credential profiles, highlight profiles, login sequences)
/// and applies upsert semantics to each: existing rows by id are
/// preserved (skipped), new rows are inserted. Each entity type has its
/// own duplicate-detection and per-row error handling, and partial
/// success is the expected outcome — a half-broken export should import
/// what it can and report the rest as warnings rather than rolling back.
///
/// The complexity is essential: collapsing the six entity loops into a
/// generic loop requires either a trait-object per-entity dispatch
/// (which loses sqlx's compile-time query checks) or a macro (which
/// hides the per-entity divergence in defaults and `FK` resolution). The
/// flat structure here is verbose but each branch is independently
/// auditable, which matters because import is a frequent source of
/// support tickets.
///
/// Validation of the import payload (count caps, length caps) is done
/// up-front by [`validate_import_data`], which is unit-tested.
#[tauri::command]
pub async fn db_import(
    state: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
    login_seq_db: State<'_, crate::db::LoginSequenceDbState>,
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
    // `session_id_map` covers every session in the file, including those
    // matched to an existing one; `created_sessions` only the ones created
    // here, the only ones the passes below may change.
    let mut session_id_map: HashMap<Uuid, Uuid> = HashMap::new();
    let mut created_sessions: HashMap<Uuid, Uuid> = HashMap::new();
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
                login_sequence_id: None,
                legacy_algorithms: session.legacy_algorithms,
            })
            .await
            .map_err(|e| format!("Failed to import session '{}': {e}", session.name))?;
        session_id_map.insert(session.id, created.id);
        created_sessions.insert(session.id, created.id);
        session_count += 1;
    }

    // Second pass: wire up jump_host_id references with remapped IDs.
    for (session, new_session_id, new_jump_id) in
        jump_host_links(&data.sessions, &created_sessions, &session_id_map)
    {
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

    // Import credential metadata into local store.
    // Secrets are not included in exports (stored in OS keychain) — users
    // must re-enter passwords/keys after import.
    let (credentials, skipped_credentials) =
        credentials_to_import(&data.credentials, &created_sessions);
    for cred in credentials {
        cred_db
            .0
            .upsert_credential(cred)
            .await
            .map_err(|e| format!("Failed to import credential: {e}"))?;
        credential_count += 1;
    }

    // Import highlight profiles — skip duplicates by name.
    let existing_profiles = state
        .0
        .list_highlight_profiles()
        .await
        .map_err(|e| format!("Failed to list existing highlight profiles: {e}"))?;
    let existing_profile_names: HashSet<String> =
        existing_profiles.iter().map(|p| p.name.clone()).collect();
    let mut profile_count = 0u32;
    let mut skipped_profiles = 0u32;
    for profile in &data.highlight_profiles {
        if existing_profile_names.contains(&profile.name) {
            skipped_profiles += 1;
            continue;
        }
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

    // Import login sequences into local store — skip duplicates by name.
    let existing_sequences = login_seq_db
        .0
        .list_login_sequences()
        .await
        .map_err(|e| format!("Failed to list existing login sequences: {e}"))?;
    let existing_seq_names: HashSet<String> =
        existing_sequences.iter().map(|s| s.name.clone()).collect();
    let mut login_seq_count = 0u32;
    let mut skipped_sequences = 0u32;
    for seq in &data.login_sequences {
        if existing_seq_names.contains(&seq.name) {
            skipped_sequences += 1;
            continue;
        }
        login_seq_db
            .0
            .create_login_sequence(crate::db::models::NewLoginSequence {
                name: seq.name.clone(),
                send_initial_cr: seq.send_initial_cr,
                steps: seq.steps.clone(),
            })
            .await
            .map_err(|e| format!("Failed to import login sequence '{}': {e}", seq.name))?;
        login_seq_count += 1;
    }

    let mut summary = format!(
        "Imported {folder_count} folders, {session_count} sessions, {credential_count} credentials, {profile_count} highlight profiles, {login_seq_count} login sequences"
    );
    let skipped_total = skipped_folders
        + skipped_sessions
        + skipped_credentials
        + skipped_profiles
        + skipped_sequences;
    if skipped_total > 0 {
        use std::fmt::Write as _;
        let _ = write!(
            summary,
            " (skipped {skipped_folders} duplicate folders, {skipped_sessions} duplicate sessions, \
             {skipped_credentials} credentials for sessions not imported, \
             {skipped_profiles} duplicate highlight profiles, {skipped_sequences} duplicate login sequences)"
        );
    }
    Ok(summary)
}

/// The jump host links an import sets: each file session with the database
/// ids of the session and of its jump host.
///
/// Only a session this import created gets one. A session matched to one
/// already present keeps its jump host, as it keeps everything else — a file
/// could otherwise reroute an existing session through a bastion it adds.
/// The jump host can be any session the file's reference resolves to,
/// created here or already present.
fn jump_host_links<'a>(
    sessions: &'a [Session],
    created_sessions: &HashMap<Uuid, Uuid>,
    session_id_map: &HashMap<Uuid, Uuid>,
) -> Vec<(&'a Session, Uuid, Uuid)> {
    sessions
        .iter()
        .filter_map(|session| {
            let new_jump_id = *session_id_map.get(&session.jump_host_id?)?;
            let new_session_id = *created_sessions.get(&session.id)?;
            Some((session, new_session_id, new_jump_id))
        })
        .collect()
}

/// The legacy credential rows an import writes, and how many it skips.
///
/// Only a credential for a session this import created is written, under the
/// new session's id, with a fresh id and the keychain ref the legacy writers
/// used. Any other credential is skipped: written as given, it would attach
/// itself — and the keychain entry its `keychain_ref` names — to a session
/// already in the database, and the legacy credential migration at the next
/// startup would move that entry's secret into a new profile for the session.
fn credentials_to_import(
    credentials: &[Credential],
    created_sessions: &HashMap<Uuid, Uuid>,
) -> (Vec<Credential>, u32) {
    let rows: Vec<Credential> = credentials
        .iter()
        .filter_map(|cred| {
            let session_id = *created_sessions.get(&cred.session_id)?;
            Some(Credential {
                id: Uuid::new_v4(),
                session_id,
                username: cred.username.clone(),
                auth_type: cred.auth_type.clone(),
                keychain_ref: crate::migrate_legacy::legacy_keychain_ref(session_id),
            })
        })
        .collect();
    let skipped = (credentials.len() - rows.len()) as u32;
    (rows, skipped)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::{
        Credential, ExportData, Folder, HighlightProfile, LoginSequence, LoginSequenceStep, Session,
    };
    use uuid::Uuid;

    // validate_import_data is the first line of defense against a
    // malicious or corrupt export file. Each branch corresponds to a
    // distinct DoS vector (count caps) or a stored-data integrity rule
    // (field-length caps, port range). Every branch needs an explicit
    // test so silent collapsing into "Ok" cannot regress.

    fn folder(name: &str) -> Folder {
        Folder {
            id: Uuid::new_v4(),
            name: name.to_string(),
            parent_id: None,
            sort_order: 0,
            owner: "local".to_string(),
            visibility: "personal".to_string(),
        }
    }

    fn session(name: &str, hostname: &str, port: i32) -> Session {
        Session {
            id: Uuid::new_v4(),
            folder_id: Uuid::new_v4(),
            name: name.to_string(),
            hostname: hostname.to_string(),
            port,
            protocol: "ssh".to_string(),
            username: String::new(),
            auth_method: "password".to_string(),
            jump_host_id: None,
            tags: String::new(),
            icon: String::new(),
            sort_order: 0,
            highlight_profile_id: None,
            credential_profile_id: None,
            legacy_algorithms: false,
            login_sequence_id: None,
            owner: "local".to_string(),
            visibility: "personal".to_string(),
        }
    }

    #[test]
    fn empty_export_is_valid() {
        assert!(validate_import_data(&ExportData::default()).is_ok());
    }

    #[test]
    fn typical_small_export_is_valid() {
        let data = ExportData {
            folders: vec![folder("prod")],
            sessions: vec![session("web1", "web1.example.com", 22)],
            ..Default::default()
        };
        assert!(validate_import_data(&data).is_ok());
    }

    #[test]
    fn too_many_folders() {
        let data = ExportData {
            folders: (0..=MAX_IMPORT_FOLDERS)
                .map(|i| folder(&format!("f{i}")))
                .collect(),
            ..Default::default()
        };
        let err = validate_import_data(&data).unwrap_err();
        assert!(err.contains("Too many folders"));
    }

    #[test]
    fn too_many_sessions() {
        let data = ExportData {
            sessions: (0..=MAX_IMPORT_SESSIONS)
                .map(|i| session(&format!("s{i}"), "h", 22))
                .collect(),
            ..Default::default()
        };
        let err = validate_import_data(&data).unwrap_err();
        assert!(err.contains("Too many sessions"));
    }

    #[test]
    fn too_many_credentials() {
        let cred = Credential {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            username: String::new(),
            auth_type: "password".to_string(),
            keychain_ref: String::new(),
        };
        let data = ExportData {
            credentials: (0..=MAX_IMPORT_CREDENTIALS).map(|_| cred.clone()).collect(),
            ..Default::default()
        };
        let err = validate_import_data(&data).unwrap_err();
        assert!(err.contains("Too many credentials"));
    }

    #[test]
    fn too_many_highlight_profiles() {
        let p = HighlightProfile {
            id: Uuid::new_v4(),
            name: "p".to_string(),
            rules: String::new(),
            sort_order: 0,
        };
        let data = ExportData {
            highlight_profiles: (0..=MAX_IMPORT_HIGHLIGHT_PROFILES)
                .map(|_| p.clone())
                .collect(),
            ..Default::default()
        };
        let err = validate_import_data(&data).unwrap_err();
        assert!(err.contains("Too many highlight profiles"));
    }

    #[test]
    fn too_many_login_sequences() {
        let seq = LoginSequence {
            id: Uuid::new_v4(),
            name: "s".to_string(),
            send_initial_cr: true,
            steps: vec![],
            sort_order: 0,
        };
        let data = ExportData {
            login_sequences: (0..=MAX_IMPORT_LOGIN_SEQUENCES)
                .map(|_| seq.clone())
                .collect(),
            ..Default::default()
        };
        let err = validate_import_data(&data).unwrap_err();
        assert!(err.contains("Too many login sequences"));
    }

    #[test]
    fn folder_name_too_long() {
        let mut f = folder("");
        f.name = "x".repeat(256);
        let data = ExportData {
            folders: vec![f],
            ..Default::default()
        };
        let err = validate_import_data(&data).unwrap_err();
        assert!(err.contains("Folder name too long"));
    }

    #[test]
    fn session_name_too_long() {
        let mut s = session("", "h", 22);
        s.name = "x".repeat(256);
        let data = ExportData {
            sessions: vec![s],
            ..Default::default()
        };
        let err = validate_import_data(&data).unwrap_err();
        assert!(err.contains("Session name too long"));
    }

    #[test]
    fn session_hostname_too_long() {
        let mut s = session("ok", "", 22);
        s.hostname = "h".repeat(256);
        let data = ExportData {
            sessions: vec![s],
            ..Default::default()
        };
        let err = validate_import_data(&data).unwrap_err();
        assert!(err.contains("hostname too long"));
    }

    #[test]
    fn session_tags_too_long() {
        let mut s = session("ok", "h", 22);
        s.tags = "t".repeat(1025);
        let data = ExportData {
            sessions: vec![s],
            ..Default::default()
        };
        let err = validate_import_data(&data).unwrap_err();
        assert!(err.contains("tags too long"));
    }

    #[test]
    fn session_port_out_of_range() {
        for port in [0, -1, 65536, i32::MAX] {
            let s = session("ok", "h", port);
            let data = ExportData {
                sessions: vec![s],
                ..Default::default()
            };
            let err = validate_import_data(&data).unwrap_err();
            assert!(
                err.contains("Invalid port"),
                "expected port-error for {port}"
            );
        }
    }

    #[test]
    fn session_port_boundaries_valid() {
        for port in [1, 22, 65535] {
            let s = session("ok", "h", port);
            let data = ExportData {
                sessions: vec![s],
                ..Default::default()
            };
            assert!(
                validate_import_data(&data).is_ok(),
                "port {port} should be valid"
            );
        }
    }

    #[test]
    fn highlight_profile_name_too_long() {
        let p = HighlightProfile {
            id: Uuid::new_v4(),
            name: "x".repeat(256),
            rules: String::new(),
            sort_order: 0,
        };
        let data = ExportData {
            highlight_profiles: vec![p],
            ..Default::default()
        };
        let err = validate_import_data(&data).unwrap_err();
        assert!(err.contains("Highlight profile name too long"));
    }

    #[test]
    fn login_sequence_name_too_long() {
        let seq = LoginSequence {
            id: Uuid::new_v4(),
            name: "x".repeat(256),
            send_initial_cr: true,
            steps: vec![],
            sort_order: 0,
        };
        let data = ExportData {
            login_sequences: vec![seq],
            ..Default::default()
        };
        let err = validate_import_data(&data).unwrap_err();
        assert!(err.contains("Login sequence name too long"));
    }

    fn import_of_session(session: Session) -> Result<(), String> {
        validate_import_data(&ExportData {
            sessions: vec![session],
            ..Default::default()
        })
    }

    #[test]
    fn session_protocol_and_auth_method_must_be_known() {
        // A Telnet session saved before credential profiles.
        let mut s = session("a", "h", 23);
        s.protocol = "telnet".to_string();
        s.auth_method = "none".to_string();
        assert_eq!(import_of_session(s), Ok(()));
        let mut s = session("a", "h", 22);
        s.protocol = "rdp".to_string();
        assert!(import_of_session(s)
            .unwrap_err()
            .contains("Unsupported protocol"));
        let mut s = session("a", "h", 22);
        s.auth_method = "agent".to_string();
        assert!(import_of_session(s)
            .unwrap_err()
            .contains("Unsupported auth method"));
        // Every combination the app writes itself passes.
        for protocol in SESSION_PROTOCOLS {
            for auth_method in SESSION_AUTH_METHODS {
                let mut s = session("a", "h", 22);
                s.protocol = protocol.to_string();
                s.auth_method = auth_method.to_string();
                assert_eq!(import_of_session(s), Ok(()), "{protocol} {auth_method}");
            }
        }
    }

    #[test]
    fn session_icon_too_long() {
        let mut s = session("a", "h", 22);
        s.icon = "i".repeat(129);
        assert!(import_of_session(s).unwrap_err().contains("icon too long"));
    }

    #[test]
    fn session_fields_the_app_never_limited_import_as_stored() {
        // The app stores usernames of any length, and so does an import.
        let mut s = session("a", "h", 22);
        s.username = "u".repeat(1000);
        assert_eq!(import_of_session(s), Ok(()));
    }

    #[test]
    fn highlight_rules_must_be_a_rule_list() {
        let profile = |rules: String| ExportData {
            highlight_profiles: vec![HighlightProfile {
                id: Uuid::new_v4(),
                name: "p".to_string(),
                rules,
                sort_order: 0,
            }],
            ..Default::default()
        };
        assert_eq!(validate_import_data(&profile("[]".into())), Ok(()));
        assert!(validate_import_data(&profile("not json".into()))
            .unwrap_err()
            .contains("invalid rules"));
        assert!(validate_import_data(&profile("[{\"pattern\": 1}]".into()))
            .unwrap_err()
            .contains("invalid rules"));
        // More rules than the app's own editor allows, as a SecureCRT
        // highlight import can store, still import.
        let rule = r##"{"pattern":"err","color":"#ff0000","case_sensitive":false,"bold":true}"##;
        let many = format!("[{}]", vec![rule; 600].join(","));
        assert_eq!(validate_import_data(&profile(many)), Ok(()));
    }

    #[test]
    fn login_sequence_steps_are_size_checked() {
        let sequence = |steps: Vec<LoginSequenceStep>| ExportData {
            login_sequences: vec![LoginSequence {
                id: Uuid::new_v4(),
                name: "s".to_string(),
                send_initial_cr: true,
                steps,
                sort_order: 0,
            }],
            ..Default::default()
        };
        let step = |pattern: &str| LoginSequenceStep {
            pattern: pattern.to_string(),
            response: "admin".to_string(),
            append_cr: true,
        };
        assert_eq!(
            validate_import_data(&sequence(vec![step("login:")])),
            Ok(())
        );
        // Patterns are compiled when a session uses the sequence, not here.
        assert_eq!(validate_import_data(&sequence(vec![step("(")])), Ok(()));
        assert!(
            validate_import_data(&sequence(vec![step(&"x".repeat(1025))]))
                .unwrap_err()
                .contains("pattern too long")
        );
        assert!(
            validate_import_data(&sequence((0..51).map(|_| step("x")).collect()))
                .unwrap_err()
                .contains("Too many steps")
        );
    }

    // An import may only change sessions it created. A file that repeats an
    // existing session's folder, host and port has that session matched and
    // skipped; a jump host or credential it gives such a session must not
    // reach the existing one.

    fn credential_for(session_id: Uuid, keychain_ref: &str) -> Credential {
        Credential {
            id: Uuid::new_v4(),
            session_id,
            username: "admin".to_string(),
            auth_type: "password".to_string(),
            keychain_ref: keychain_ref.to_string(),
        }
    }

    #[test]
    fn credentials_to_import_writes_only_sessions_created_here() {
        let (created, matched, absent) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let new_id = Uuid::new_v4();
        let created_sessions = HashMap::from([(created, new_id)]);
        let file = [
            credential_for(created, config::PG_PASSWORD_KEYCHAIN_REF),
            credential_for(matched, "session-x"),
            credential_for(absent, "session-y"),
        ];

        let (rows, skipped) = credentials_to_import(&file, &created_sessions);

        assert_eq!(skipped, 2);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].session_id, new_id);
        assert_eq!(
            rows[0].keychain_ref,
            crate::migrate_legacy::legacy_keychain_ref(new_id)
        );
        assert_ne!(rows[0].id, file[0].id);
        assert_eq!(rows[0].username, "admin");
    }

    #[test]
    fn jump_host_links_change_only_sessions_created_here() {
        // `bastion` and `existing` match sessions already present; `added`,
        // `web`, `app` and `dangling` are created by the import.
        let bastion = session("bastion", "bastion.example", 22);
        let added = session("added", "attacker.example", 22);
        let mut existing = session("existing", "db.example", 22);
        existing.jump_host_id = Some(added.id);
        let mut web = session("web", "web.example", 22);
        web.jump_host_id = Some(bastion.id);
        let mut app = session("app", "app.example", 22);
        app.jump_host_id = Some(added.id);
        let mut dangling = session("dangling", "other.example", 22);
        dangling.jump_host_id = Some(Uuid::new_v4());

        let (old_bastion, old_existing) = (Uuid::new_v4(), Uuid::new_v4());
        let (new_added, new_web, new_app, new_dangling) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let created_sessions = HashMap::from([
            (added.id, new_added),
            (web.id, new_web),
            (app.id, new_app),
            (dangling.id, new_dangling),
        ]);
        let mut session_id_map = created_sessions.clone();
        session_id_map.insert(bastion.id, old_bastion);
        session_id_map.insert(existing.id, old_existing);

        let sessions = [bastion, added, existing, web, app, dangling];
        let links: Vec<(Uuid, Uuid)> =
            jump_host_links(&sessions, &created_sessions, &session_id_map)
                .into_iter()
                .map(|(_, s, j)| (s, j))
                .collect();

        // `web` may use the bastion already present and `app` one created
        // here; `existing` keeps its own jump host, and a reference to no
        // session sets none.
        assert_eq!(links, [(new_web, old_bastion), (new_app, new_added)]);
    }
}
