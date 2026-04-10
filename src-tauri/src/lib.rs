mod commands;
mod config;
mod credentials;
mod db;
mod highlight;
mod import;
mod pty;
mod session_logger;
mod ssh;
mod telnet;

use std::sync::Arc;

use commands::DbStatusState;
use config::{ConfigState, DbBackend};
use db::postgres::PostgresProvider;
use db::sqlite::SqliteProvider;
use db::{CredentialDbState, DbState};
use pty::PtyState;
use sha2::{Digest, Sha384};
use sqlx::postgres::PgPoolOptions;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;
use ssh::SshState;
use tauri::menu::{Menu, MenuItemBuilder, PredefinedMenuItem, Submenu};
use tauri::Manager;
use tauri_plugin_opener::OpenerExt;
use telnet::TelnetState;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
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
///
/// When `custom_path` is `Some`, that path is used instead of the default
/// `<config_dir>/sessions.db`.
fn init_local_sqlite(
    config_dir: &std::path::Path,
    enforce_fk: bool,
    custom_path: Option<&str>,
) -> Result<SqlitePool, Box<dyn std::error::Error>> {
    let db_path = match custom_path {
        Some(p) if !p.is_empty() => std::path::PathBuf::from(p),
        _ => config_dir.join("sessions.db"),
    };
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

/// Fix migration checksum mismatches caused by CRLF/LF line-ending
/// differences across platforms.
///
/// sqlx embeds migration checksums at compile time. When compiled on
/// Windows (CRLF files) but the DB was initialized from Linux/macOS (LF),
/// every checksum differs even though the SQL content is identical.
///
/// This function detects that specific case and updates the DB checksums
/// to match the compiled binary, so `sqlx::migrate!().run()` succeeds.
fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

async fn fix_crlf_migration_checksums(
    pool: &sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    // If the migrations table doesn't exist yet, nothing to fix.
    let table_exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (\
             SELECT FROM information_schema.tables \
             WHERE table_name = '_sqlx_migrations'\
         )",
    )
    .fetch_one(pool)
    .await?;
    if !table_exists {
        return Ok(());
    }

    let migrator = sqlx::migrate!();
    for migration in migrator.iter() {
        let compiled_checksum: &[u8] = migration.checksum.as_ref();

        // Fetch the checksum that was stored when this migration was applied.
        let row: Option<(Vec<u8>,)> =
            sqlx::query_as("SELECT checksum FROM _sqlx_migrations WHERE version = $1")
                .bind(migration.version)
                .fetch_optional(pool)
                .await?;

        let Some((db_checksum,)) = row else {
            continue;
        };

        if db_checksum == compiled_checksum {
            continue;
        }

        // Checksums differ — verify it's purely a line-ending difference.
        // Compute the SHA-384 of the SQL with LF-only line endings.
        let lf_sql = migration.sql.replace("\r\n", "\n");
        let lf_hash: Vec<u8> = Sha384::digest(lf_sql.as_bytes()).to_vec();

        // Compute the SHA-384 of the SQL with CRLF line endings.
        let crlf_sql = lf_sql.replace('\n', "\r\n");
        let crlf_hash: Vec<u8> = Sha384::digest(crlf_sql.as_bytes()).to_vec();

        // The DB checksum must match one of the two normalized forms.
        if db_checksum != lf_hash && db_checksum != crlf_hash {
            // Genuine content change — don't touch it. The migration was
            // edited in source after this database was initialized, so the
            // schema in the DB no longer matches what the binary expects.
            tracing::error!(
                version = migration.version,
                description = %migration.description,
                db_checksum = %hex_lower(&db_checksum),
                expected_checksum = %hex_lower(compiled_checksum),
                "Migration content changed since this database was initialized. \
                 The schema in PostgreSQL is from an older revision of this migration. \
                 Reset the database (DROP DATABASE / CREATE DATABASE) or manually \
                 update the checksum row in _sqlx_migrations if the schema is still compatible."
            );
            continue;
        }

        tracing::info!(
            version = migration.version,
            "Fixing CRLF migration checksum for \"{}\"",
            migration.description
        );
        sqlx::query("UPDATE _sqlx_migrations SET checksum = $1 WHERE version = $2")
            .bind(compiled_checksum)
            .bind(migration.version)
            .execute(pool)
            .await?;
    }

    Ok(())
}

/// Strip connection URLs and credentials from PostgreSQL error messages
/// so they are safe to display in the frontend.
fn sanitize_pg_error(raw: &str) -> String {
    if raw.contains("password authentication failed") {
        return "Authentication failed — check username and password.".to_string();
    }
    if raw.contains("Connection refused") || raw.contains("connection refused") {
        return "Connection refused — check host and port.".to_string();
    }
    if raw.contains("timeout") || raw.contains("Timed out") {
        return "Connection timed out — check host, port, and firewall rules.".to_string();
    }
    if raw.contains("previously applied but has been modified")
        || (raw.contains("migration") && raw.contains("checksum"))
        || raw.contains("VersionMismatch")
    {
        return "Database schema is from an older revision of ShellStation. \
                Reset the PostgreSQL database (DROP DATABASE / CREATE DATABASE) \
                or update the affected row in _sqlx_migrations. See application \
                logs for the migration version."
            .to_string();
    }
    if raw.contains("VersionMissing") {
        return "Database migration state is inconsistent with this build — \
                a previously applied migration is no longer present in the binary. \
                See application logs for details."
            .to_string();
    }
    if raw.contains("relation") && raw.contains("does not exist") {
        return "A required table is missing — migrations did not run successfully. \
                See application logs for details."
            .to_string();
    }
    if raw.contains("database") && raw.contains("does not exist") {
        return "Database does not exist — check the database name.".to_string();
    }
    // Fallback: generic message without leaking internals. Direct the user
    // to the logs since the real detail is there.
    "PostgreSQL initialization failed. See application logs for details.".to_string()
}

/// Build the application menu, mirroring the Tauri default but using a custom
/// Help submenu passed by the caller.
fn build_app_menu(
    app: &tauri::AppHandle,
    help_menu: &Submenu<tauri::Wry>,
) -> Result<Menu<tauri::Wry>, Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    let pkg = app.package_info();
    #[cfg(target_os = "macos")]
    let about = tauri::menu::AboutMetadata {
        name: Some(pkg.name.clone()),
        version: Some(pkg.version.to_string()),
        copyright: app.config().bundle.copyright.clone(),
        authors: app.config().bundle.publisher.clone().map(|p| vec![p]),
        ..Default::default()
    };

    #[cfg(not(any(
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )))]
    let window_menu = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::maximize(app, None)?,
            #[cfg(target_os = "macos")]
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::close_window(app, None)?,
        ],
    )?;

    let menu = Menu::with_items(
        app,
        &[
            #[cfg(target_os = "macos")]
            &Submenu::with_items(
                app,
                pkg.name.clone(),
                true,
                &[
                    &PredefinedMenuItem::about(app, None, Some(about.clone()))?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::services(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::hide(app, None)?,
                    &PredefinedMenuItem::hide_others(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::quit(app, None)?,
                ],
            )?,
            #[cfg(not(any(
                target_os = "linux",
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd"
            )))]
            &Submenu::with_items(
                app,
                "File",
                true,
                &[
                    &PredefinedMenuItem::close_window(app, None)?,
                    #[cfg(not(target_os = "macos"))]
                    &PredefinedMenuItem::quit(app, None)?,
                ],
            )?,
            &Submenu::with_items(
                app,
                "Edit",
                true,
                &[
                    &PredefinedMenuItem::undo(app, None)?,
                    &PredefinedMenuItem::redo(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::cut(app, None)?,
                    &PredefinedMenuItem::copy(app, None)?,
                    &PredefinedMenuItem::paste(app, None)?,
                    &PredefinedMenuItem::select_all(app, None)?,
                ],
            )?,
            #[cfg(target_os = "macos")]
            &Submenu::with_items(
                app,
                "View",
                true,
                &[&PredefinedMenuItem::fullscreen(app, None)?],
            )?,
            #[cfg(not(any(
                target_os = "linux",
                target_os = "dragonfly",
                target_os = "freebsd",
                target_os = "netbsd",
                target_os = "openbsd"
            )))]
            &window_menu,
            help_menu,
        ],
    )?;

    Ok(menu)
}

/// Build the tracing subscriber from the application config.
///
/// Always installs a stdout layer. When `cfg.enabled` is true, also installs
/// a daily-rotating file appender writing to `<log_directory>/shellstation.log`
/// (default `<config_dir>/logs/`). The returned `WorkerGuard` must be held
/// for the process lifetime — dropping it flushes pending writes and shuts
/// down the background writer thread.
///
/// `RUST_LOG`, when set, takes precedence over `cfg.level`.
fn init_tracing(
    config_dir: &std::path::Path,
    cfg: &config::AppLoggingConfig,
) -> Option<WorkerGuard> {
    use tracing_subscriber::fmt;

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        EnvFilter::new(format!(
            "shellstation={lvl},shellstation_lib={lvl}",
            lvl = cfg.level
        ))
    });

    let stdout_layer = fmt::layer().with_writer(std::io::stdout);

    if cfg.enabled {
        let log_dir = match &cfg.log_directory {
            Some(p) if !p.is_empty() => std::path::PathBuf::from(p),
            _ => config_dir.join("logs"),
        };
        if let Err(e) = std::fs::create_dir_all(&log_dir) {
            // Fall back to stdout-only — we cannot use tracing yet because the
            // subscriber is not installed, so this goes straight to stderr.
            eprintln!(
                "Failed to create application log directory {}: {e} — \
                 application logging disabled for this session.",
                log_dir.display()
            );
            tracing_subscriber::registry()
                .with(env_filter)
                .with(stdout_layer)
                .init();
            return None;
        }
        let file_appender = tracing_appender::rolling::daily(&log_dir, "shellstation.log");
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
        let file_layer = fmt::layer().with_ansi(false).with_writer(non_blocking);
        tracing_subscriber::registry()
            .with(env_filter)
            .with(stdout_layer)
            .with(file_layer)
            .init();
        tracing::info!(
            log_dir = %log_dir.display(),
            level = %cfg.level,
            "Application file logging enabled (daily rotation)"
        );
        return Some(guard);
    }

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .init();
    None
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("shellstation");
    let _ = std::fs::create_dir_all(&config_dir);

    // Load config first so the tracing subscriber can be configured from it.
    // Tracing calls inside load_config itself are no-ops here (no subscriber
    // yet), but we re-emit a confirmation line right after init_tracing.
    let app_config = config::load_config(&config_dir);
    let _log_guard = init_tracing(&config_dir, &app_config.app_logging);
    tracing::info!(
        backend = ?app_config.db_backend,
        pg_host = %app_config.postgres.host,
        pg_port = app_config.postgres.port,
        "ShellStation starting"
    );

    tauri::Builder::default()
        .manage(PtyState::default())
        .manage(SshState::default())
        .manage(TelnetState::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            // Build custom menu: replicate Tauri defaults but with a
            // functional Help submenu that links to our project pages.
            let app_handle = app.handle();
            let help_docs =
                MenuItemBuilder::with_id("help_docs", "ShellStation Help").build(app_handle)?;
            let help_issues =
                MenuItemBuilder::with_id("help_issues", "Report an Issue").build(app_handle)?;
            let help_menu = Submenu::with_items(
                app_handle,
                "Help",
                true,
                &[
                    &help_docs,
                    &PredefinedMenuItem::separator(app_handle)?,
                    &help_issues,
                ],
            )?;

            let menu = build_app_menu(app_handle, &help_menu)?;
            app.set_menu(menu)?;

            app.on_menu_event(|handle, event| {
                let opener = handle.opener();
                match event.id().as_ref() {
                    "help_docs" => {
                        let _ = opener
                            .open_url("https://git.fiedler.live/tux/shellstation", None::<&str>);
                    }
                    "help_issues" => {
                        let _ = opener.open_url(
                            "https://git.fiedler.live/tux/shellstation/issues",
                            None::<&str>,
                        );
                    }
                    _ => {}
                }
            });
            // Set window icon at runtime (needed on Linux outside of bundled installs)
            if let Some(window) = app.get_webview_window("main") {
                if let Ok(icon) =
                    tauri::image::Image::from_bytes(include_bytes!("../icons/128x128.png"))
                {
                    let _ = window.set_icon(icon);
                }
            }

            // config_dir and app_config were loaded at the top of run() so
            // they could drive tracing initialization. They are captured
            // here via the move closure.
            let config_path = config_dir.join("config.json");
            if !config_path.exists() {
                let _ = config::save_config(&config_path, &app_config);
            }

            app.manage(ConfigState {
                config: std::sync::Mutex::new(app_config.clone()),
                config_path,
            });

            // Initialize session logger
            let log_dir = match &app_config.logging.log_directory {
                Some(p) if !p.is_empty() => std::path::PathBuf::from(p),
                _ => config_dir.join("logs"),
            };
            let log_manager = session_logger::SessionLogManager::new(
                app_config.logging.enabled,
                log_dir,
                app_config.logging.filename_format.clone(),
            );
            app.manage(session_logger::SessionLogState(Arc::new(
                std::sync::Mutex::new(log_manager),
            )));

            // Initialize the session database provider based on config
            match app_config.db_backend {
                DbBackend::Sqlite => {
                    // In SQLite mode, the same pool serves both sessions and credentials.
                    // Foreign keys are enforced since everything is in one DB.
                    //
                    // If a custom path is configured but fails to open, fall back
                    // to the default path so the app remains launchable and the
                    // user can fix the path in Settings.
                    let (local_pool, db_error) = match init_local_sqlite(
                        &config_dir,
                        true,
                        app_config.sqlite_path.as_deref(),
                    ) {
                        Ok(pool) => (pool, None),
                        Err(e) if app_config.sqlite_path.is_some() => {
                            tracing::error!(
                                "Custom SQLite path failed: {e} — falling back to default"
                            );
                            let fallback = init_local_sqlite(&config_dir, true, None)?;
                            (
                                fallback,
                                Some(format!(
                                    "Could not open {}: {}",
                                    app_config.sqlite_path.as_deref().unwrap_or(""),
                                    e
                                )),
                            )
                        }
                        Err(e) => return Err(Box::new(std::io::Error::other(e.to_string()))),
                    };
                    let provider = Arc::new(SqliteProvider::new(local_pool));
                    app.manage(DbState(provider.clone() as Arc<dyn db::DatabaseProvider>));
                    app.manage(CredentialDbState(provider as Arc<dyn db::DatabaseProvider>));
                    app.manage(DbStatusState(commands::DbStatus {
                        backend: "sqlite".to_string(),
                        healthy: db_error.is_none(),
                        error: db_error,
                    }));
                }
                DbBackend::Postgres => {
                    // Credentials always stay in local SQLite. FK enforcement is
                    // OFF because sessions live in PostgreSQL — the credentials
                    // table's FK to sessions would fail cross-database.
                    let local_pool = init_local_sqlite(&config_dir, false, None)?;
                    let cred_provider =
                        Arc::new(SqliteProvider::new(local_pool)) as Arc<dyn db::DatabaseProvider>;
                    app.manage(CredentialDbState(cred_provider));

                    tracing::info!(
                        host = %app_config.postgres.host,
                        port = app_config.postgres.port,
                        database = %app_config.postgres.database,
                        ssl_mode = %app_config.postgres.ssl_mode,
                        password_len = app_config.postgres.password.len(),
                        "Connecting to PostgreSQL…"
                    );
                    let password_was_empty = app_config.postgres.password.is_empty();
                    let pg_opts = app_config.postgres.connect_options();
                    match tauri::async_runtime::block_on(async {
                        let pool = PgPoolOptions::new()
                            .max_connections(10)
                            .acquire_timeout(std::time::Duration::from_secs(5))
                            .connect_with(pg_opts)
                            .await?;
                        // Fix CRLF/LF checksum mismatches before running migrations
                        // so that Windows checkouts don't fail against a DB initialized
                        // from Linux/macOS (or vice versa).
                        fix_crlf_migration_checksums(&pool).await?;
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
                            // Sanitize before logging — raw errors may contain
                            // connection strings or credentials.
                            let safe_msg = if password_was_empty {
                                "PostgreSQL password missing from keychain at startup. \
                                 Open Settings and re-save the database configuration to \
                                 restore it."
                                    .to_string()
                            } else {
                                sanitize_pg_error(&e.to_string())
                            };
                            tracing::error!("PostgreSQL connection failed: {safe_msg}");

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
                                error: Some(safe_msg),
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
            // Telnet
            commands::telnet_connect,
            commands::telnet_write,
            commands::telnet_resize,
            commands::telnet_disconnect,
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
            commands::session_data_fingerprint,
            commands::session_connect,
            // Reordering
            commands::folder_reorder,
            commands::session_reorder,
            commands::folder_sort_alphabetically,
            commands::session_sort_alphabetically,
            // Credentials
            commands::credential_get,
            // Bulk operations
            commands::folder_apply_credentials,
            // Database config & migration
            commands::db_get_config,
            commands::db_get_status,
            commands::db_test_connection,
            commands::db_create_database,
            commands::db_save_config,
            commands::db_export,
            commands::db_export_file,
            commands::db_import,
            // Import from external tools
            import::import_mremoteng,
            import::import_securecrt,
            // Highlight profiles
            commands::highlight_profile_create,
            commands::highlight_profile_list,
            commands::highlight_profile_get,
            commands::highlight_profile_update,
            commands::highlight_profile_delete,
            commands::import_securecrt_highlights,
            // Session logging
            commands::logging_get_config,
            commands::logging_save_config,
            // Application logging
            commands::app_logging_get_config,
            commands::app_logging_save_config,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
