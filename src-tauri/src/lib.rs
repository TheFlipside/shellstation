mod commands;
// Keychain integration — unused until we switch from DB-stored secrets
// to OS keychain with a reliable backend (e.g. secret-service on Linux).
#[allow(dead_code)]
mod credentials;
mod db;
mod pty;
mod ssh;

use std::sync::Arc;

use db::sqlite::SqliteProvider;
use db::DbState;
use pty::PtyState;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;
use ssh::SshState;
use tauri::Manager;
use tracing_subscriber::EnvFilter;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    tauri::Builder::default()
        .manage(PtyState::default())
        .manage(SshState::default())
        .plugin(tauri_plugin_opener::init())
        .setup(|_app| {
            let db_dir = dirs::config_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("shellstation");
            std::fs::create_dir_all(&db_dir)?;

            let db_path = db_dir.join("sessions.db");
            let pool = tauri::async_runtime::block_on(async {
                let opts = SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true)
                    .pragma("foreign_keys", "ON");
                let pool = SqlitePool::connect_with(opts).await?;
                sqlx::migrate!().run(&pool).await?;
                Ok::<SqlitePool, Box<dyn std::error::Error>>(pool)
            })?;

            let provider = SqliteProvider::new(pool);
            _app.manage(DbState(Arc::new(provider)));
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
