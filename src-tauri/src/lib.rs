mod commands;
mod config;
mod credentials;
mod db;
mod highlight;
mod import;
mod migrate_legacy;
mod pty;
mod session_logger;
mod ssh;
mod telnet;

use std::sync::Arc;

use commands::{DbStatusState, TerminalReadyState};
use config::{ConfigState, DbBackend};
use db::postgres::PostgresProvider;
use db::sqlite::SqliteProvider;
use db::{CredentialDbState, DbState, PgPoolState, PgUserState};
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

/// Reject paths from `config.json` that aren't absolute or that contain
/// parent-directory (`..`) components. This is defense-in-depth: the config
/// file is 0600, but if it's ever editable by another process or restored
/// from an untrusted backup we don't want to follow `..` traversal into
/// arbitrary filesystem locations.
fn validate_config_path(raw: &str, label: &str) -> Result<std::path::PathBuf, String> {
    let path = std::path::PathBuf::from(raw);
    if !path.is_absolute() {
        return Err(format!("{label} must be an absolute path: {raw}"));
    }
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!(
            "{label} must not contain parent-directory (..) components: {raw}"
        ));
    }
    Ok(path)
}

#[cfg(target_os = "windows")]
fn startup_show_maximized() -> bool {
    // Minimal FFI into kernel32!GetStartupInfoW. We only read the two fields
    // we care about, so avoid pulling in the full `windows-sys` crate.
    #[repr(C)]
    struct StartupInfoW {
        cb: u32,
        lp_reserved: *mut u16,
        lp_desktop: *mut u16,
        lp_title: *mut u16,
        dw_x: u32,
        dw_y: u32,
        dw_x_size: u32,
        dw_y_size: u32,
        dw_x_count_chars: u32,
        dw_y_count_chars: u32,
        dw_fill_attribute: u32,
        dw_flags: u32,
        w_show_window: u16,
        cb_reserved2: u16,
        lp_reserved2: *mut u8,
        h_std_input: *mut std::ffi::c_void,
        h_std_output: *mut std::ffi::c_void,
        h_std_error: *mut std::ffi::c_void,
    }
    extern "system" {
        fn GetStartupInfoW(lp_startup_info: *mut StartupInfoW);
    }
    const STARTF_USESHOWWINDOW: u32 = 0x0000_0001;
    const SW_SHOWMAXIMIZED: u16 = 3;

    // SAFETY: GetStartupInfoW fills the provided structure. We zero it first
    // and set `cb` to the struct size, matching the Win32 calling contract.
    let mut info: StartupInfoW = unsafe { std::mem::zeroed() };
    info.cb = std::mem::size_of::<StartupInfoW>() as u32;
    unsafe { GetStartupInfoW(&mut info) };
    (info.dw_flags & STARTF_USESHOWWINDOW) != 0 && info.w_show_window == SW_SHOWMAXIMIZED
}
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
        Some(p) if !p.is_empty() => validate_config_path(p, "SQLite database path")
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?,
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

/// Idempotent RLS setup for PostgreSQL multi-user isolation.
///
/// Called after `sqlx::migrate!()` succeeds. Statements are written so that
/// running them multiple times is harmless (`IF NOT EXISTS`, etc.).
async fn setup_postgres_rls(pool: &sqlx::PgPool) -> Result<(), String> {
    // Fix the placeholder owner from the migration DEFAULT.
    sqlx::query("UPDATE folders SET owner = current_user WHERE owner = 'local'")
        .execute(pool)
        .await
        .map_err(|e| format!("RLS setup (fix folder owners): {e}"))?;
    sqlx::query("UPDATE sessions SET owner = current_user WHERE owner = 'local'")
        .execute(pool)
        .await
        .map_err(|e| format!("RLS setup (fix session owners): {e}"))?;

    // Enable RLS. FORCE ensures policies apply even to the table owner.
    for stmt in [
        "ALTER TABLE folders ENABLE ROW LEVEL SECURITY",
        "ALTER TABLE folders FORCE ROW LEVEL SECURITY",
        "ALTER TABLE sessions ENABLE ROW LEVEL SECURITY",
        "ALTER TABLE sessions FORCE ROW LEVEL SECURITY",
    ] {
        sqlx::query(stmt)
            .execute(pool)
            .await
            .map_err(|e| format!("RLS setup ({stmt}): {e}"))?;
    }

    // Policies — use DO blocks so CREATE POLICY IF NOT EXISTS works on PG < 17.
    // PG 14+ supports IF NOT EXISTS directly; wrap in exception handler for safety.
    let policies = [
        // Folders
        (
            "folders_owner_all",
            "folders",
            "ALL",
            "owner = current_user",
            "",
        ),
        (
            "folders_shared_read",
            "folders",
            "SELECT",
            "visibility = 'shared'",
            "",
        ),
        (
            "folders_shared_update",
            "folders",
            "UPDATE",
            "visibility = 'shared'",
            "",
        ),
        (
            "folders_shared_delete",
            "folders",
            "DELETE",
            "visibility = 'shared' AND owner = current_user",
            "",
        ),
        // Sessions
        (
            "sessions_owner_all",
            "sessions",
            "ALL",
            "owner = current_user",
            "",
        ),
        (
            "sessions_shared_read",
            "sessions",
            "SELECT",
            "visibility = 'shared'",
            "",
        ),
        (
            "sessions_shared_update",
            "sessions",
            "UPDATE",
            "visibility = 'shared'",
            "",
        ),
        (
            "sessions_shared_delete",
            "sessions",
            "DELETE",
            "visibility = 'shared' AND owner = current_user",
            "",
        ),
    ];

    for (name, table, cmd, using_expr, _) in &policies {
        let sql = format!(
            "DO $$ BEGIN \
                 CREATE POLICY {name} ON {table} FOR {cmd} USING ({using_expr}); \
             EXCEPTION WHEN duplicate_object THEN NULL; \
             END $$"
        );
        sqlx::query(&sql)
            .execute(pool)
            .await
            .map_err(|e| format!("RLS setup (policy {name}): {e}"))?;
    }

    // CHECK constraints on visibility values.
    for table in ["folders", "sessions"] {
        let sql = format!(
            "DO $$ BEGIN \
                 ALTER TABLE {table} ADD CONSTRAINT {table}_vis_ck \
                     CHECK (visibility IN ('personal', 'shared')); \
             EXCEPTION WHEN duplicate_object THEN NULL; \
             END $$"
        );
        sqlx::query(&sql)
            .execute(pool)
            .await
            .map_err(|e| format!("RLS setup (constraint {table}_vis_ck): {e}"))?;
    }

    tracing::info!("PostgreSQL RLS setup complete");
    Ok(())
}

/// Build the application menu, mirroring the Tauri default but using a custom
/// Help submenu passed by the caller.
fn build_app_menu(
    app: &tauri::AppHandle,
    #[cfg_attr(
        any(
            target_os = "linux",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd"
        ),
        allow(unused_variables)
    )]
    about: &tauri::menu::AboutMetadata,
    help_menu: &Submenu<tauri::Wry>,
) -> Result<Menu<tauri::Wry>, Box<dyn std::error::Error>> {
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
                about.name.clone().unwrap_or_default(),
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
            Some(p) if !p.is_empty() => {
                match validate_config_path(p, "Application log directory") {
                    Ok(path) => path,
                    Err(e) => {
                        eprintln!("{e} — application logging disabled for this session.");
                        tracing_subscriber::registry()
                            .with(env_filter)
                            .with(stdout_layer)
                            .init();
                        return None;
                    }
                }
            }
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
        let file_appender = tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix("shellstation")
            .filename_suffix("log")
            .build(&log_dir)
            .expect("failed to initialize rolling file appender");
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
        .manage(TerminalReadyState::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            // Build custom menu: replicate Tauri defaults but with a
            // functional Help submenu that links to our project pages.
            let app_handle = app.handle();
            let pkg = app_handle.package_info();
            let about_meta = tauri::menu::AboutMetadata {
                name: Some(pkg.name.clone()),
                version: Some(pkg.version.to_string()),
                copyright: app_handle.config().bundle.copyright.clone(),
                authors: app_handle
                    .config()
                    .bundle
                    .publisher
                    .clone()
                    .map(|p| vec![p]),
                icon: tauri::image::Image::from_bytes(include_bytes!("../icons/32x32.png")).ok(),
                ..Default::default()
            };

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
                    // On macOS, About lives in the app submenu; everywhere else
                    // add it at the bottom of the Help menu.
                    #[cfg(not(target_os = "macos"))]
                    &PredefinedMenuItem::separator(app_handle)?,
                    #[cfg(not(target_os = "macos"))]
                    &PredefinedMenuItem::about(
                        app_handle,
                        Some("About ShellStation"),
                        Some(about_meta.clone()),
                    )?,
                ],
            )?;

            let menu = build_app_menu(app_handle, &about_meta, &help_menu)?;
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
                // Honor Windows shortcut "Run: Maximized". Tauri uses its own
                // window config and ignores the process-wide nCmdShow hint,
                // so we read STARTUPINFO ourselves and maximize on request.
                #[cfg(target_os = "windows")]
                if startup_show_maximized() {
                    let _ = window.maximize();
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
                    let provider_trait: Arc<dyn db::DatabaseProvider> = provider.clone();
                    if let Err(e) =
                        tauri::async_runtime::block_on(migrate_legacy::migrate_legacy_credentials(
                            &provider_trait,
                            &provider_trait,
                        ))
                    {
                        tracing::error!("Legacy credential migration failed: {e}");
                    }
                    app.manage(DbState(provider.clone() as Arc<dyn db::DatabaseProvider>));
                    app.manage(CredentialDbState(provider as Arc<dyn db::DatabaseProvider>));
                    app.manage(PgPoolState(None));
                    app.manage(PgUserState(None));
                    app.manage(DbStatusState(commands::DbStatus {
                        backend: "sqlite".to_string(),
                        healthy: db_error.is_none(),
                        error: db_error,
                        pg_user: None,
                    }));
                }
                DbBackend::Postgres => {
                    // Credentials always stay in local SQLite. FK enforcement is
                    // OFF because sessions live in PostgreSQL — the credentials
                    // table's FK to sessions would fail cross-database.
                    let local_pool = init_local_sqlite(&config_dir, false, None)?;
                    let cred_provider =
                        Arc::new(SqliteProvider::new(local_pool)) as Arc<dyn db::DatabaseProvider>;
                    app.manage(CredentialDbState(cred_provider.clone()));

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
                            // Run post-migration RLS setup (idempotent).
                            if let Err(e) =
                                tauri::async_runtime::block_on(setup_postgres_rls(&pool))
                            {
                                tracing::error!("PostgreSQL RLS setup failed: {e}");
                            }
                            // Query current_user for ownership tracking.
                            let pg_user = tauri::async_runtime::block_on(async {
                                sqlx::query_scalar::<_, String>("SELECT current_user")
                                    .fetch_one(&pool)
                                    .await
                                    .ok()
                            });
                            tracing::info!(pg_user = ?pg_user, "PostgreSQL current_user");

                            let provider: Arc<dyn db::DatabaseProvider> =
                                Arc::new(PostgresProvider::new(pool.clone()));
                            if let Err(e) = tauri::async_runtime::block_on(
                                migrate_legacy::migrate_legacy_credentials(
                                    &provider,
                                    &cred_provider,
                                ),
                            ) {
                                tracing::error!("Legacy credential migration failed: {e}");
                            }
                            app.manage(DbState(provider));
                            app.manage(PgPoolState(Some(pool)));
                            app.manage(PgUserState(pg_user.clone()));
                            app.manage(DbStatusState(commands::DbStatus {
                                backend: "postgres".to_string(),
                                healthy: true,
                                error: None,
                                pg_user,
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
                            app.manage(PgPoolState(None));
                            app.manage(PgUserState(None));
                            app.manage(DbStatusState(commands::DbStatus {
                                backend: "postgres".to_string(),
                                healthy: false,
                                error: Some(safe_msg),
                                pg_user: None,
                            }));
                        }
                    }
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Terminal ready signal (all protocols)
            commands::terminal_ready,
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
            commands::ssh_kbd_interactive_response,
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
            // Credential Profiles
            commands::credential_profile_create,
            commands::credential_profile_list,
            commands::credential_profile_get_secret,
            commands::credential_profile_update,
            commands::credential_profile_delete,
            // Bulk operations
            commands::folder_apply_credential_profile,
            commands::folder_bulk_edit_sessions,
            // Visibility (multi-user)
            commands::set_visibility,
            // Session credentials (multi-user)
            commands::set_session_credential,
            commands::get_session_credential,
            commands::bulk_set_session_credentials,
            // Database config & migration
            commands::app_restart,
            commands::db_get_config,
            commands::db_get_status,
            commands::get_user_ident,
            commands::set_user_ident,
            commands::get_os_username,
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
