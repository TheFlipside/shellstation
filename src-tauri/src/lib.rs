mod commands;
mod config;
// Keychain integration — unused until we switch from DB-stored secrets
// to OS keychain with a reliable backend (e.g. secret-service on Linux).
#[allow(dead_code)]
mod credentials;
mod db;
mod pty;
mod ssh;

use std::sync::Arc;

use commands::DbStatusState;
use config::{ConfigState, DbBackend};
use db::postgres::PostgresProvider;
use db::sqlite::SqliteProvider;
use db::{CredentialDbState, DbState};
use pty::PtyState;
use sqlx::postgres::PgPoolOptions;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;
use ssh::SshState;
use tauri::Manager;
use tracing_subscriber::EnvFilter;

/// Initialize the local SQLite pool.
///
/// When `enforce_fk` is true, foreign keys are enforced (used in SQLite-only
/// mode where sessions and credentials share the same database).
///
/// When `enforce_fk` is false, foreign keys are NOT enforced (used when
/// PostgreSQL hosts sessions but local SQLite stores credentials — the FK
/// on `credentials.session_id → sessions.id` would fail because sessions
/// live in a different database).
fn init_local_sqlite(
    config_dir: &std::path::Path,
    enforce_fk: bool,
) -> Result<SqlitePool, Box<dyn std::error::Error>> {
    let db_path = config_dir.join("sessions.db");
    tauri::async_runtime::block_on(async {
        let mut opts = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true);
        if enforce_fk {
            opts = opts.pragma("foreign_keys", "ON");
        } else {
            opts = opts.pragma("foreign_keys", "OFF");
        }
        let pool = SqlitePool::connect_with(opts).await?;
        sqlx::migrate!().run(&pool).await?;
        Ok(pool)
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    tauri::Builder::default()
        .manage(PtyState::default())
        .manage(SshState::default())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Set window icon at runtime (needed on Linux outside of bundled installs)
            if let Some(window) = app.get_webview_window("main") {
                if let Ok(icon) =
                    tauri::image::Image::from_bytes(include_bytes!("../icons/128x128.png"))
                {
                    let _ = window.set_icon(icon);
                }
            }

            let config_dir = dirs::config_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("shellstation");
            std::fs::create_dir_all(&config_dir)?;

            // Load application config
            let app_config = config::load_config(&config_dir);
            let config_path = config_dir.join("config.json");

            // Save default config if it doesn't exist yet
            if !config_path.exists() {
                let _ = config::save_config(&config_path, &app_config);
            }

            app.manage(ConfigState {
                config: std::sync::Mutex::new(app_config.clone()),
                config_path,
            });

            // Initialize the session database provider based on config
            match app_config.db_backend {
                DbBackend::Sqlite => {
                    // In SQLite mode, the same pool serves both sessions and credentials.
                    // Foreign keys are enforced since everything is in one DB.
                    let local_pool = init_local_sqlite(&config_dir, true)?;
                    let provider = Arc::new(SqliteProvider::new(local_pool));
                    app.manage(DbState(provider.clone() as Arc<dyn db::DatabaseProvider>));
                    app.manage(CredentialDbState(provider as Arc<dyn db::DatabaseProvider>));
                    app.manage(DbStatusState(commands::DbStatus {
                        backend: "sqlite".to_string(),
                        healthy: true,
                        error: None,
                    }));
                }
                DbBackend::Postgres => {
                    // Credentials always stay in local SQLite. FK enforcement is
                    // OFF because sessions live in PostgreSQL — the credentials
                    // table's FK to sessions would fail cross-database.
                    let local_pool = init_local_sqlite(&config_dir, false)?;
                    let cred_provider =
                        Arc::new(SqliteProvider::new(local_pool)) as Arc<dyn db::DatabaseProvider>;
                    app.manage(CredentialDbState(cred_provider));

                    let url = app_config.postgres.connection_url();
                    match tauri::async_runtime::block_on(async {
                        let pool = PgPoolOptions::new()
                            .max_connections(10)
                            .acquire_timeout(std::time::Duration::from_secs(5))
                            .connect(&url)
                            .await?;
                        sqlx::migrate!().run(&pool).await?;
                        Ok::<sqlx::PgPool, Box<dyn std::error::Error>>(pool)
                    }) {
                        Ok(pool) => {
                            let provider = PostgresProvider::new(pool);
                            app.manage(DbState(Arc::new(provider)));
                            app.manage(DbStatusState(commands::DbStatus {
                                backend: "postgres".to_string(),
                                healthy: true,
                                error: None,
                            }));
                        }
                        Err(e) => {
                            tracing::error!("PostgreSQL connection failed: {e}");

                            // Provide a stub DbState so the app can still start
                            // and the user can fix settings. Use a fresh in-memory
                            // SQLite so we don't mix local data with PG expectations.
                            let stub_pool = tauri::async_runtime::block_on(async {
                                let opts = SqliteConnectOptions::new()
                                    .filename(":memory:")
                                    .create_if_missing(true)
                                    .pragma("foreign_keys", "ON");
                                let pool = SqlitePool::connect_with(opts).await?;
                                sqlx::migrate!().run(&pool).await?;
                                Ok::<SqlitePool, Box<dyn std::error::Error>>(pool)
                            })?;
                            let provider = SqliteProvider::new(stub_pool);
                            app.manage(DbState(Arc::new(provider)));
                            app.manage(DbStatusState(commands::DbStatus {
                                backend: "postgres".to_string(),
                                healthy: false,
                                error: Some(e.to_string()),
                            }));
                        }
                    }
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // PTY
            commands::pty_spawn,
            commands::pty_write,
            commands::pty_resize,
            commands::pty_kill,
            // SSH
            commands::ssh_connect,
            commands::ssh_write,
            commands::ssh_resize,
            commands::ssh_disconnect,
            commands::ssh_host_verify_response,
            // Folders
            commands::folder_create,
            commands::folder_list,
            commands::folder_rename,
            commands::folder_move,
            commands::folder_delete,
            // Sessions
            commands::session_create,
            commands::session_get,
            commands::session_list_all,
            commands::session_update,
            commands::session_move,
            commands::session_delete,
            commands::session_search,
            commands::session_connect,
            // Database config & migration
            commands::db_get_config,
            commands::db_get_status,
            commands::db_test_connection,
            commands::db_save_config,
            commands::db_export,
            commands::db_import,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
