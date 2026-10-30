// Clippy pedantic is enabled in `just lint`. The lints allowed below are
// deliberate stylistic / architectural choices in this codebase; the
// remaining pedantic lints are kept active and addressed at the call site.
//
// - too_many_lines: orchestration entry points (session_connect, run,
//   db_save_config, db_import, …) are documented as essential complexity
//   in their own headers — splitting them was tried and abandoned.
// - option_option: `Option<Option<T>>` is the three-state encoding for
//   nullable-field updates (skip column / set NULL / set value) used by
//   UpdateBuilder and BulkSessionEdit. Replacing it with a custom enum
//   would just rename the pattern without removing the three states.
// - needless_pass_by_value: Tauri command handlers receive owned
//   parameters by convention (`String`, `Uuid`, …) — taking them by
//   reference would force every call site to clone.
// - cast_possible_truncation / cast_possible_wrap / cast_precision_loss /
//   cast_sign_loss: terminal column/row counts, port numbers, file sizes,
//   and timestamps stay well within the smaller integer ranges. Per-call
//   `try_from` would just produce `unreachable!()` arms; the casts are
//   bounded by validation higher up the stack.
#![allow(
    clippy::too_many_lines,
    clippy::option_option,
    clippy::needless_pass_by_value,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]

mod commands;
mod config;
mod credentials;
mod db;
mod highlight;
mod import;
mod limits;
mod login_sequence;
mod migrate_legacy;
mod private_fs;
mod pty;
mod session_logger;
mod ssh;
mod telnet;

use std::sync::Arc;

use commands::{DbStatusState, TerminalReadyState};
use config::{ConfigState, DbBackend};
use db::postgres::PostgresProvider;
use db::sqlite::SqliteProvider;
use db::{CredentialDbState, DbState, LoginSequenceDbState, PgPoolState, PgUserState};
use pty::PtyState;
use sha2::{Digest, Sha384};
use sqlx::postgres::PgPoolOptions;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;
use ssh::SshState;
use tauri::menu::{Menu, MenuItemBuilder, PredefinedMenuItem, Submenu};
use tauri::{Emitter, Manager};
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
    unsafe { GetStartupInfoW(&raw mut info) };
    (info.dw_flags & STARTF_USESHOWWINDOW) != 0 && info.w_show_window == SW_SHOWMAXIMIZED
}
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Initialize the local `SQLite` pool.
///
/// When `enforce_fk` is true, foreign keys are enforced (used in `SQLite`-only
/// mode where sessions and credentials share the same database).
///
/// When `enforce_fk` is false, foreign keys are `NOT` enforced (used when
/// `PostgreSQL` hosts sessions but local `SQLite` stores credentials — the `FK`
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
        // migration.sql is `sqlx::SqlStr` since sqlx 0.9; call `.as_str()`.
        let lf_sql = migration.sql.as_str().replace("\r\n", "\n");
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

/// Check whether all compiled migrations are already applied in the database.
///
/// `sqlx::migrate!().run()` unconditionally runs `CREATE TABLE IF `NOT` EXISTS
/// _sqlx_migrations`, which requires `CREATE` privilege on the schema. DML-only
/// `PostgreSQL` users (Option A in `DESIGN`.md §4.5.2) lack that privilege. This
/// function lets us skip the migrate call entirely when the admin has already
/// applied all migrations.
async fn pg_migrations_up_to_date(pool: &sqlx::PgPool) -> Result<bool, Box<dyn std::error::Error>> {
    let table_exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (\
             SELECT FROM information_schema.tables \
             WHERE table_name = '_sqlx_migrations'\
         )",
    )
    .fetch_one(pool)
    .await?;
    if !table_exists {
        return Ok(false);
    }

    // Every migration this build carries must be applied. Comparing counts
    // would also pass with a different set, such as another build's.
    let applied: std::collections::HashSet<i64> =
        sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations WHERE success = true")
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();
    Ok(sqlx::migrate!()
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
        .all(|m| applied.contains(&m.version)))
}

/// Strip connection `URLs` and credentials from `PostgreSQL` error messages
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

/// Idempotent `RLS` setup for `PostgreSQL` multi-user isolation.
///
/// Called after `sqlx::migrate!()` succeeds. Statements are written so that
/// running them multiple times is harmless (`IF `NOT` EXISTS`, etc.).
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
        ),
        (
            "folders_shared_read",
            "folders",
            "SELECT",
            "visibility = 'shared'",
        ),
        (
            "folders_shared_update",
            "folders",
            "UPDATE",
            "visibility = 'shared'",
        ),
        (
            "folders_shared_delete",
            "folders",
            "DELETE",
            "visibility = 'shared' AND owner = current_user",
        ),
        // Sessions
        (
            "sessions_owner_all",
            "sessions",
            "ALL",
            "owner = current_user",
        ),
        (
            "sessions_shared_read",
            "sessions",
            "SELECT",
            "visibility = 'shared'",
        ),
        (
            "sessions_shared_update",
            "sessions",
            "UPDATE",
            "visibility = 'shared'",
        ),
        (
            "sessions_shared_delete",
            "sessions",
            "DELETE",
            "visibility = 'shared' AND owner = current_user",
        ),
    ];

    for (name, table, cmd, using_expr) in &policies {
        let sql = format!(
            "DO $$ BEGIN \
                 CREATE POLICY {name} ON {table} FOR {cmd} USING ({using_expr}); \
             EXCEPTION WHEN duplicate_object THEN NULL; \
             END $$"
        );
        sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
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
        sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
            .execute(pool)
            .await
            .map_err(|e| format!("RLS setup (constraint {table}_vis_ck): {e}"))?;
    }

    // Triggers keep a shared row with its owner and where its owner put it.
    // Anyone may update a shared row, and PostgreSQL accepts the updated row
    // if it passes any permissive policy. That leaves two ways around the
    // owner-only rule of `*_shared_delete`:
    //
    // - `SET owner = current_user` passes `*_owner_all` and makes the row the
    //   updater's. A policy cannot stop this, because a `WITH CHECK` sees
    //   only the new row. Owner changes are left to superusers (who reassign
    //   a removed user's rows, ADMIN_GUIDE.md §4) and the table owner, plus
    //   claiming a placeholder `'local'` row for oneself, as the fix-up at
    //   the top of this function does.
    // - Moving the row under a folder of one's own (`folder_id`, `parent_id`)
    //   hides it from everyone else, and deleting that folder deletes it
    //   too: the foreign keys cascade, and a cascade is not subject to RLS.
    //   Moves are left to the row's owner, which the app's own move commands
    //   already require, and to the table owner and superusers.
    //
    // The functions run as the updating user, so every name in them is
    // pinned: a temporary table is found before `pg_catalog` unless the path
    // says otherwise, and a temp table named `pg_class` would otherwise let
    // any user claim to own the table. They are replaced on every run so
    // that a fix to them reaches existing databases; only the table owner
    // gets this far, the `ALTER TABLE`s above stop everyone else. This step
    // comes last, so a failure to replace them (a function owned by another
    // role) leaves the rest of the setup applied.
    let functions = [
        (
            "shellstation_keep_owner",
            "NEW.owner IS DISTINCT FROM OLD.owner \
             AND NOT (OLD.owner = 'local' AND NEW.owner = current_user)",
            "only the table owner can change the owner of a % row",
        ),
        (
            "shellstation_keep_place",
            "OLD.owner IS DISTINCT FROM current_user",
            "only its owner can move a % row",
        ),
    ];
    for (name, refuse_when, message) in functions {
        let sql = format!(
            "CREATE OR REPLACE FUNCTION {name}() RETURNS trigger \
             LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $fn$ \
             BEGIN \
                 IF {refuse_when} \
                    AND NOT pg_catalog.pg_has_role(current_user, \
                        (SELECT relowner FROM pg_catalog.pg_class WHERE oid = TG_RELID), 'MEMBER') \
                 THEN \
                     RAISE EXCEPTION '{message}', TG_TABLE_NAME \
                         USING ERRCODE = 'insufficient_privilege'; \
                 END IF; \
                 RETURN NEW; \
             END $fn$"
        );
        sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
            .execute(pool)
            .await
            .map_err(|e| format!("RLS setup (function {name}): {e}"))?;
    }
    let triggers = [
        (
            "folders_keep_owner",
            "folders",
            "owner",
            "shellstation_keep_owner",
        ),
        (
            "sessions_keep_owner",
            "sessions",
            "owner",
            "shellstation_keep_owner",
        ),
        (
            "folders_keep_place",
            "folders",
            "parent_id",
            "shellstation_keep_place",
        ),
        (
            "sessions_keep_place",
            "sessions",
            "folder_id",
            "shellstation_keep_place",
        ),
    ];
    for (name, table, column, function) in triggers {
        let sql = format!(
            "DO $$ BEGIN \
                 CREATE TRIGGER {name} BEFORE UPDATE OF {column} ON {table} FOR EACH ROW \
                     WHEN (OLD.{column} IS DISTINCT FROM NEW.{column}) \
                     EXECUTE FUNCTION {function}(); \
             EXCEPTION WHEN duplicate_object THEN NULL; \
             END $$"
        );
        sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
            .execute(pool)
            .await
            .map_err(|e| format!("RLS setup (trigger {name}): {e}"))?;
    }

    tracing::info!("PostgreSQL RLS setup complete");
    Ok(())
}

/// Build the application menu, mirroring the Tauri default but using a custom
/// Help submenu passed by the caller.
fn build_app_menu(
    app: &tauri::AppHandle,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
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

/// The app log level to use for the configured `level`. The value comes from
/// the config file, where it may have been edited by hand, and is validated
/// here because `app_log_directives` splices it into the filter string: a
/// value such as `trace,sqlx=trace` would otherwise switch on logging for
/// other crates. An invalid value falls back to the default.
fn app_log_level(level: &str) -> String {
    // EnvFilter reads levels case-insensitively, so a hand-edited "DEBUG"
    // has always worked and must keep working.
    let normalized = level.to_ascii_lowercase();
    if config::AppLoggingConfig::validate_level(&normalized).is_ok() {
        normalized
    } else {
        let fallback = config::AppLoggingConfig::default().level;
        // The subscriber is not installed yet, so this goes straight to stderr.
        eprintln!("Invalid app log level {level:?} in config; using {fallback:?}.");
        fallback
    }
}

/// Filter directives for a level from `app_log_level`. Only the app's own
/// targets get that level; every other crate (sqlx, russh, hyper, ...) has no
/// directive and so stays off, whatever the level.
fn app_log_directives(level: &str) -> String {
    format!("shellstation={level},shellstation_lib={level}")
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

    let level = app_log_level(&cfg.level);
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(app_log_directives(&level)));

    let stdout_layer = fmt::layer().with_writer(std::io::stdout);

    let log_file = if cfg.enabled {
        open_app_log_file(config_dir, cfg)
    } else {
        None
    };
    let Some((file_appender, log_dir)) = log_file else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(stdout_layer)
            .init();
        return None;
    };

    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let file_layer = fmt::layer().with_ansi(false).with_writer(non_blocking);
    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();
    tracing::info!(
        log_dir = %log_dir.display(),
        level = %level,
        "Application file logging enabled (daily rotation)"
    );
    Some(guard)
}

/// Open the daily-rotating application log file for `init_tracing`, and
/// return it with the directory it is in.
///
/// Returns `None` when the configured directory is invalid or the directory
/// or the file cannot be created, so that the caller falls back to
/// stdout-only logging. The reason goes straight to stderr, because the
/// tracing subscriber is not installed yet.
fn open_app_log_file(
    config_dir: &std::path::Path,
    cfg: &config::AppLoggingConfig,
) -> Option<(
    tracing_appender::rolling::RollingFileAppender,
    std::path::PathBuf,
)> {
    // The default directory belongs to the app and is made owner-only on
    // Unix; the session logger keeps its logs there too. A directory the
    // user chose is created if it is missing but otherwise left as it is.
    let (log_dir, created) = match &cfg.log_directory {
        Some(p) if !p.is_empty() => match validate_config_path(p, "Application log directory") {
            Ok(path) => {
                let created = std::fs::create_dir_all(&path);
                (path, created)
            }
            Err(e) => {
                eprintln!("{e} — application logging disabled for this session.");
                return None;
            }
        },
        _ => {
            let path = config_dir.join("logs");
            let created = crate::private_fs::create_private_dir(&path);
            if created.is_ok() {
                // A file system without Unix modes can refuse this; the
                // directory is still usable, so logging goes ahead.
                if let Err(e) = crate::private_fs::restrict_dir_to_owner(&path) {
                    eprintln!(
                        "Failed to restrict the application log directory {} to its owner: {e}",
                        path.display()
                    );
                }
            }
            (path, created)
        }
    };
    if let Err(e) = created {
        eprintln!(
            "Failed to create application log directory {}: {e} — \
             application logging disabled for this session.",
            log_dir.display()
        );
        return None;
    }
    // Creating the directory can succeed and opening the file still fail
    // (directory not writable, disk full); that falls back the same way.
    match tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("shellstation")
        .filename_suffix("log")
        .build(&log_dir)
    {
        Ok(appender) => Some((appender, log_dir)),
        Err(e) => {
            eprintln!(
                "Failed to open application log file in {}: {e} — \
                 application logging disabled for this session.",
                log_dir.display()
            );
            None
        }
    }
}

/// Application entry point. Initializes the database, logging, Tauri
/// builder, and event loop.
///
/// # Panics
///
/// Panics if Tauri's `Builder::run` cannot start the application — this is
/// terminal and unrecoverable (typically a webview / windowing failure),
/// so a panic is the appropriate signal for the `OS` to surface.
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let config_dir = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("shellstation");
    // Owner-only on Unix: it holds the config, the local database and the
    // logs. The subscriber is not installed yet, so a problem goes to stderr
    // now and to the log once there is one.
    let config_dir_problem = private_fs::create_private_dir(&config_dir)
        .map_err(|e| format!("Failed to create the config directory: {e}"))
        .and_then(|()| {
            private_fs::restrict_dir_to_owner(&config_dir)
                .map_err(|e| format!("Failed to restrict the config directory to its owner: {e}"))
        })
        .err();
    if let Some(problem) = &config_dir_problem {
        eprintln!("{problem} ({})", config_dir.display());
    }

    // Load config first so the tracing subscriber can be configured from it.
    // Tracing calls inside load_config itself are no-ops here (no subscriber
    // yet), but we re-emit a confirmation line right after init_tracing.
    let app_config = config::load_config(&config_dir);
    let _log_guard = init_tracing(&config_dir, &app_config.app_logging);
    if let Some(problem) = config_dir_problem {
        tracing::warn!(path = %config_dir.display(), "{problem}");
    }
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
        .on_page_load(|webview, payload| {
            // Only the main window holds terminal tabs. A load there, a
            // reload included, starts the frontend over without them. Any
            // navigation in that window counts, so the frontend must never
            // navigate it (no in-app routing by URL, no links opened in it).
            if webview.label() == "main" && payload.event() == tauri::webview::PageLoadEvent::Started
            {
                commands::close_all_sessions(webview.app_handle());
            }
        })
        // Only the Help menu opens URLs, from Rust, so the webview is given
        // no opener permission (see `no_capability_grants_the_opener`). The
        // link-click handler the plugin injects by default calls the opener
        // from the webview, so it is left out.
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
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
            let help_updates =
                MenuItemBuilder::with_id("help_check_updates", "Check for Updates\u{2026}")
                    .build(app_handle)?;
            let help_menu = Submenu::with_items(
                app_handle,
                "Help",
                true,
                &[
                    &help_docs,
                    &PredefinedMenuItem::separator(app_handle)?,
                    &help_issues,
                    &PredefinedMenuItem::separator(app_handle)?,
                    &help_updates,
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
                    "help_check_updates" => {
                        // Tell the frontend to run a manual update check.
                        // The webview owns the dialog UI, signature verification
                        // happens in the plugin itself.
                        let _ = handle.emit("updater:check-requested", ());
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
                if let Err(e) = config::save_config(&config_path, &app_config) {
                    tracing::warn!("Failed to write the default config: {e}");
                }
            }

            app.manage(ConfigState {
                config: std::sync::Mutex::new(app_config.clone()),
                config_path,
            });

            // Initialize session logger
            let log_dir = match &app_config.logging.log_directory {
                Some(p) if !p.is_empty() => validate_config_path(p, "Session log directory")
                    .unwrap_or_else(|e| {
                        eprintln!("{e} — using default log directory.");
                        config_dir.join("logs")
                    }),
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
                    app.manage(CredentialDbState(
                        provider.clone() as Arc<dyn db::DatabaseProvider>
                    ));
                    app.manage(LoginSequenceDbState(
                        provider as Arc<dyn db::DatabaseProvider>,
                    ));
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
                    app.manage(LoginSequenceDbState(cred_provider.clone()));

                    tracing::info!(
                        host = %app_config.postgres.host,
                        port = app_config.postgres.port,
                        database = %app_config.postgres.database,
                        ssl_mode = %app_config.postgres.ssl_mode,
                        password_configured = !app_config.postgres.password.is_empty(),
                        "Connecting to PostgreSQL…"
                    );
                    let password_configured = !app_config.postgres.password.is_empty();
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
                        if pg_migrations_up_to_date(&pool).await? {
                            tracing::info!(
                                "All migrations already applied — skipping sqlx::migrate!()"
                            );
                        } else {
                            sqlx::migrate!().run(&pool).await?;
                        }
                        Ok::<sqlx::PgPool, Box<dyn std::error::Error>>(pool)
                    }) {
                        Ok(pool) => {
                            // Run post-migration RLS setup (idempotent).
                            if let Err(e) =
                                tauri::async_runtime::block_on(setup_postgres_rls(&pool))
                            {
                                tracing::warn!("PostgreSQL RLS setup skipped; only the role that owns the tables can apply it (ADMIN_GUIDE.md §5): {e}");
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
                            let safe_msg = if password_configured {
                                sanitize_pg_error(&e.to_string())
                            } else {
                                "PostgreSQL connection failed. Check your database \
                                 configuration in Settings."
                                    .to_string()
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
            commands::session_sort_by_hostname,
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
            commands::db_pick_sqlite_path,
            commands::db_save_config,
            commands::db_export,
            commands::data_export_file,
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
            // Login Sequences
            commands::login_sequence_create,
            commands::login_sequence_list,
            commands::login_sequence_get,
            commands::login_sequence_update,
            commands::login_sequence_delete,
            commands::folder_apply_login_sequence,
            // Session login sequences (multi-user)
            commands::set_session_login_sequence,
            commands::get_session_login_sequence,
            commands::bulk_set_session_login_sequences,
            // Session logging
            commands::logging_get_config,
            commands::logging_pick_directory,
            commands::logging_save_config,
            // Application logging
            commands::app_logging_get_config,
            commands::app_logging_pick_directory,
            commands::app_logging_save_config,
            // Connection settings
            commands::connection_get_config,
            commands::connection_save_config,
            commands::connection_set_restrict_private_ips,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_log_directives_leave_other_crates_off() {
        use tracing::Level;
        let subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new(app_log_directives(&app_log_level("trace"))));
        tracing::subscriber::with_default(subscriber, || {
            assert!(tracing::enabled!(target: "shellstation_lib::import", Level::TRACE));
            assert!(!tracing::enabled!(target: "sqlx::query", Level::ERROR));
            assert!(!tracing::enabled!(target: "russh::client", Level::ERROR));
        });
    }

    #[test]
    fn app_log_level_normalizes_case_and_rejects_extra_directives() {
        assert_eq!(app_log_level("debug"), "debug");
        assert_eq!(app_log_level("DEBUG"), "debug");
        assert_eq!(app_log_level("trace,sqlx=trace"), "info");
        assert_eq!(app_log_level(""), "info");
    }

    /// The opener's commands would let a compromised webview open any web
    /// link in the browser or reveal any file in the file manager, and the
    /// webview needs neither: the Help menu opens its links from Rust.
    #[test]
    fn no_capability_grants_the_opener() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let capability: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            for permission in capability["permissions"].as_array().unwrap() {
                // Either the identifier alone, or an object with a scope.
                let id = permission
                    .as_str()
                    .or_else(|| permission["identifier"].as_str())
                    .unwrap();
                assert!(!id.starts_with("opener:"), "{} grants {id}", path.display());
            }
        }
    }

    /// The rules for configured paths, which startup and saving the SQLite
    /// path in Settings share.
    #[test]
    fn validate_config_path_wants_absolute_paths_without_parent_components() {
        let absolute = std::env::temp_dir().join("sessions.db");
        let raw = absolute.to_string_lossy();
        assert_eq!(validate_config_path(&raw, "Path"), Ok(absolute.clone()));
        assert!(validate_config_path("relative/sessions.db", "Path")
            .unwrap_err()
            .contains("absolute"));
        let up = std::env::temp_dir().join("..").join("sessions.db");
        assert!(validate_config_path(&up.to_string_lossy(), "Path")
            .unwrap_err()
            .contains("(..)"));
    }

    fn log_config(dir: &std::path::Path) -> config::AppLoggingConfig {
        config::AppLoggingConfig {
            log_directory: Some(dir.to_string_lossy().into_owned()),
            ..Default::default()
        }
    }

    #[test]
    fn open_app_log_file_opens_the_configured_directory() {
        let dir = std::env::temp_dir().join(format!("ss-applog-{}", uuid::Uuid::new_v4()));
        let opened = open_app_log_file(&std::env::temp_dir(), &log_config(&dir));
        assert_eq!(opened.as_ref().map(|(_, d)| d), Some(&dir));
        drop(opened);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_app_log_file_falls_back_when_the_file_cannot_be_opened() {
        // The directory exists, but the day's file name is taken by a
        // directory. Today and tomorrow (UTC, as the appender names files)
        // both, so a run across midnight still hits one.
        let dir = std::env::temp_dir().join(format!("ss-applog-{}", uuid::Uuid::new_v4()));
        let today = chrono::Utc::now().date_naive();
        for day in [today, today + chrono::Days::new(1)] {
            std::fs::create_dir_all(dir.join(format!("shellstation.{day}.log"))).unwrap();
        }
        assert!(open_app_log_file(&std::env::temp_dir(), &log_config(&dir)).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_app_log_file_falls_back_when_the_directory_cannot_be_created() {
        let blocker = std::env::temp_dir().join(format!("ss-applog-{}", uuid::Uuid::new_v4()));
        std::fs::write(&blocker, b"").unwrap();
        let cfg = log_config(&blocker.join("logs"));
        assert!(open_app_log_file(&std::env::temp_dir(), &cfg).is_none());
        let _ = std::fs::remove_file(&blocker);
    }

    #[test]
    fn open_app_log_file_rejects_a_relative_directory() {
        let cfg = log_config(std::path::Path::new("relative/logs"));
        assert!(open_app_log_file(&std::env::temp_dir(), &cfg).is_none());
    }

    // sanitize_pg_error converts sqlx / libpq error text into a UX-safe
    // message. The branches matter: each one corresponds to a distinct
    // user remediation, so a regression that collapses one branch into
    // the generic fallback degrades support quality without being
    // visible in any other test.

    #[test]
    fn pg_auth_failure() {
        let s =
            sanitize_pg_error("FATAL: password authentication failed for user \"shellstation\"");
        assert!(s.contains("Authentication failed"));
        // Must NOT leak the username from the raw error.
        assert!(!s.contains("shellstation"));
    }

    #[test]
    fn pg_connection_refused() {
        assert!(
            sanitize_pg_error("could not connect to server: Connection refused")
                .contains("Connection refused")
        );
        assert!(sanitize_pg_error("io error: connection refused").contains("Connection refused"));
    }

    #[test]
    fn pg_timeout() {
        assert!(sanitize_pg_error("connection timeout expired").contains("timed out"));
        assert!(sanitize_pg_error("Timed out connecting").contains("timed out"));
    }

    #[test]
    fn pg_migration_checksum_mismatch() {
        // Three different sqlx phrasings — all must produce the
        // schema-from-older-revision message.
        let s1 = sanitize_pg_error(
            "migration 20240101000000 was previously applied but has been modified",
        );
        let s2 = sanitize_pg_error("migration checksum mismatch");
        let s3 = sanitize_pg_error("VersionMismatch(20240101000000)");
        assert!(s1.contains("older revision"));
        assert!(s2.contains("older revision"));
        assert!(s3.contains("older revision"));
    }

    #[test]
    fn pg_version_missing() {
        let s = sanitize_pg_error("VersionMissing(20240101000000)");
        assert!(s.contains("no longer present"));
    }

    #[test]
    fn pg_relation_missing() {
        let s = sanitize_pg_error("error: relation \"sessions\" does not exist");
        assert!(s.contains("required table is missing"));
    }

    #[test]
    fn pg_database_missing() {
        let s = sanitize_pg_error("FATAL: database \"shellstation\" does not exist");
        assert!(s.contains("Database does not exist"));
    }

    #[test]
    fn pg_fallback_strips_internals() {
        // Anything we don't recognize must collapse to a generic message
        // that does not leak the original text (paths, sockets, etc.).
        let raw = "io error: /var/run/postgresql/.s.PGSQL.5432: No such file or directory";
        let s = sanitize_pg_error(raw);
        assert!(s.contains("See application logs"));
        assert!(!s.contains("/var/run/postgresql"));
    }

    async fn pg_exec(
        pool: &sqlx::PgPool,
        sql: &str,
    ) -> Result<sqlx::postgres::PgQueryResult, sqlx::Error> {
        sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await
    }

    /// The refusals of the two triggers; RLS refuses with the same code.
    const BY_OWNER_TRIGGER: &str = "only the table owner can change the owner";
    const BY_MOVE_TRIGGER: &str = "only its owner can move";

    fn refused(result: Result<sqlx::postgres::PgQueryResult, sqlx::Error>, by: &str) -> bool {
        matches!(result, Err(sqlx::Error::Database(e))
            if e.code().as_deref() == Some("42501") && e.message().contains(by))
    }

    async fn insert_folder(
        pool: &sqlx::PgPool,
        parent: Option<uuid::Uuid>,
        visibility: &str,
    ) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO folders (id, name, parent_id, sort_order, owner, visibility) \
             VALUES ($1, 'folder', $2, 0, current_user, $3)",
        )
        .bind(id)
        .bind(parent)
        .bind(visibility)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    /// `setup_postgres_rls` against a real server, with the roles the admin
    /// guide sets up. Needs a superuser URL for a throwaway server in
    /// `SHELLSTATION_TEST_PG_URL`, for example
    /// `postgres://postgres:pw@127.0.0.1:55432/postgres` after
    /// `docker run --rm -e POSTGRES_PASSWORD=pw -p 127.0.0.1:55432:5432 postgres:17`.
    /// Run with `cargo test -- --ignored shared_rows`. It creates its own
    /// database and roles and drops them again afterwards.
    #[tokio::test]
    #[ignore = "needs a throwaway PostgreSQL server in SHELLSTATION_TEST_PG_URL"]
    async fn shared_rows_keep_their_owner() {
        let url = std::env::var("SHELLSTATION_TEST_PG_URL")
            .expect("SHELLSTATION_TEST_PG_URL must name a throwaway PostgreSQL server");
        let server = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let suffix = &uuid::Uuid::new_v4().simple().to_string()[..8];
        let (db, admin, alice, bob) = (
            format!("ss_rls_{suffix}"),
            format!("ss_admin_{suffix}"),
            format!("ss_alice_{suffix}"),
            format!("ss_bob_{suffix}"),
        );
        for sql in [
            format!("CREATE DATABASE {db}"),
            format!("CREATE ROLE {admin} NOLOGIN"),
            format!("CREATE ROLE {alice} NOLOGIN"),
            format!("CREATE ROLE {bob} NOLOGIN"),
        ] {
            pg_exec(&server, &sql).await.unwrap();
        }

        // In a task of their own, so that a failing check still gets the
        // database and roles dropped before its panic is passed on.
        let opts: sqlx::postgres::PgConnectOptions = url.parse().unwrap();
        let checks = tokio::spawn(check_shared_rows_keep_their_owner(
            opts.database(&db),
            admin.clone(),
            alice.clone(),
            bob.clone(),
        ))
        .await;
        for sql in [
            format!("DROP DATABASE {db} WITH (FORCE)"),
            format!("DROP ROLE {admin}, {alice}, {bob}"),
        ] {
            pg_exec(&server, &sql).await.unwrap();
        }
        if let Err(e) = checks {
            std::panic::resume_unwind(e.into_panic());
        }
    }

    /// `pg_migrations_up_to_date` against a real server: it must want every
    /// migration this build carries, not merely as many. Same server as for
    /// `shared_rows_keep_their_owner`; run with `--ignored migrations_check`.
    #[tokio::test]
    #[ignore = "needs a throwaway PostgreSQL server in SHELLSTATION_TEST_PG_URL"]
    async fn migrations_check_compares_versions() {
        let url = std::env::var("SHELLSTATION_TEST_PG_URL")
            .expect("SHELLSTATION_TEST_PG_URL must name a throwaway PostgreSQL server");
        let server = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .unwrap();
        let db = format!("ss_mig_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        pg_exec(&server, &format!("CREATE DATABASE {db}"))
            .await
            .unwrap();

        let opts = url
            .parse::<sqlx::postgres::PgConnectOptions>()
            .unwrap()
            .database(&db);
        let checks = tokio::spawn(async move {
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .connect_with(opts)
                .await
                .unwrap();
            assert!(!pg_migrations_up_to_date(&pool).await.unwrap());
            sqlx::migrate!().run(&pool).await.unwrap();
            assert!(pg_migrations_up_to_date(&pool).await.unwrap());
            // A migration recorded as failed is not applied.
            pg_exec(
                &pool,
                "UPDATE _sqlx_migrations SET success = false \
                 WHERE version = (SELECT min(version) FROM _sqlx_migrations)",
            )
            .await
            .unwrap();
            assert!(!pg_migrations_up_to_date(&pool).await.unwrap());
            pg_exec(&pool, "UPDATE _sqlx_migrations SET success = true")
                .await
                .unwrap();
            assert!(pg_migrations_up_to_date(&pool).await.unwrap());
            // As many applied rows as before, but one is not this build's.
            pg_exec(
                &pool,
                "UPDATE _sqlx_migrations SET version = version + 1000000000 \
                 WHERE version = (SELECT max(version) FROM _sqlx_migrations)",
            )
            .await
            .unwrap();
            assert!(!pg_migrations_up_to_date(&pool).await.unwrap());
            pool.close().await;
        })
        .await;
        pg_exec(&server, &format!("DROP DATABASE {db} WITH (FORCE)"))
            .await
            .unwrap();
        if let Err(e) = checks {
            std::panic::resume_unwind(e.into_panic());
        }
    }

    async fn check_shared_rows_keep_their_owner(
        opts: sqlx::postgres::PgConnectOptions,
        admin: String,
        alice: String,
        bob: String,
    ) {
        // One connection, so each SET ROLE holds for the statements after it.
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        pg_exec(&pool, &format!("GRANT ALL ON SCHEMA public TO {admin}"))
            .await
            .unwrap();
        pg_exec(
            &pool,
            &format!("GRANT USAGE ON SCHEMA public TO {alice}, {bob}"),
        )
        .await
        .unwrap();

        // As the admin: migrations and RLS setup, twice to show it is
        // idempotent, then DML for the users (admin guide, Option A).
        pg_exec(&pool, &format!("SET ROLE {admin}")).await.unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        setup_postgres_rls(&pool).await.unwrap();
        setup_postgres_rls(&pool).await.unwrap();
        pg_exec(
            &pool,
            &format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public \
                 TO {alice}, {bob}"
            ),
        )
        .await
        .unwrap();

        pg_exec(&pool, &format!("SET ROLE {alice}")).await.unwrap();
        let folder = insert_folder(&pool, None, "shared").await;
        let subfolder = insert_folder(&pool, Some(folder), "shared").await;
        let session = uuid::Uuid::new_v4();
        sqlx::query(
            "INSERT INTO sessions (id, folder_id, name, hostname, username, owner, visibility) \
             VALUES ($1, $2, 'shared', 'host.example', '', current_user, 'shared')",
        )
        .bind(session)
        .bind(folder)
        .execute(&pool)
        .await
        .unwrap();
        let id = folder.to_string();
        let take_folder = format!("UPDATE folders SET owner = current_user WHERE id = '{id}'");

        pg_exec(&pool, &format!("SET ROLE {bob}")).await.unwrap();
        assert!(refused(
            pg_exec(&pool, &take_folder).await,
            BY_OWNER_TRIGGER
        ));
        assert!(refused(
            pg_exec(
                &pool,
                &format!("UPDATE sessions SET owner = current_user WHERE id = '{session}'"),
            )
            .await,
            BY_OWNER_TRIGGER
        ));
        // A temporary table is found before pg_catalog for an unqualified
        // name; one named pg_class must not let bob pass as the table owner.
        pg_exec(&pool, "CREATE TEMP TABLE pg_class (oid oid, relowner oid)")
            .await
            .unwrap();
        pg_exec(
            &pool,
            "INSERT INTO pg_class SELECT 'folders'::regclass, oid FROM pg_roles \
             WHERE rolname = current_user",
        )
        .await
        .unwrap();
        assert!(refused(
            pg_exec(&pool, &take_folder).await,
            BY_OWNER_TRIGGER
        ));
        let rename = format!("UPDATE folders SET name = 'renamed' WHERE id = '{id}'");
        assert_eq!(pg_exec(&pool, &rename).await.unwrap().rows_affected(), 1);
        let delete = format!("DELETE FROM folders WHERE id = '{id}'");
        assert_eq!(pg_exec(&pool, &delete).await.unwrap().rows_affected(), 0);

        // Moved under a folder of bob's, alice's rows would be hidden from
        // everyone else and deleted with it: the foreign keys cascade, and a
        // cascade is not subject to RLS.
        let own = insert_folder(&pool, None, "personal").await;
        assert!(refused(
            pg_exec(
                &pool,
                &format!("UPDATE sessions SET folder_id = '{own}' WHERE id = '{session}'"),
            )
            .await,
            BY_MOVE_TRIGGER
        ));
        assert!(refused(
            pg_exec(
                &pool,
                &format!("UPDATE folders SET parent_id = '{own}' WHERE id = '{subfolder}'"),
            )
            .await,
            BY_MOVE_TRIGGER
        ));
        let delete_own = format!("DELETE FROM folders WHERE id = '{own}'");
        assert_eq!(
            pg_exec(&pool, &delete_own).await.unwrap().rows_affected(),
            1
        );
        let left: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM sessions WHERE id = '{session}'"
        )))
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(left, 1);

        // The owner cannot hand the row to someone else either, but can move
        // it between folders of their own.
        pg_exec(&pool, &format!("SET ROLE {alice}")).await.unwrap();
        assert!(refused(
            pg_exec(
                &pool,
                &format!("UPDATE folders SET owner = '{bob}' WHERE id = '{id}'"),
            )
            .await,
            BY_OWNER_TRIGGER
        ));
        let other = insert_folder(&pool, None, "personal").await;
        let move_own = format!("UPDATE sessions SET folder_id = '{other}' WHERE id = '{session}'");
        assert_eq!(pg_exec(&pool, &move_own).await.unwrap().rows_affected(), 1);

        // The admin, as table owner, reassigns rows (admin guide §4).
        pg_exec(&pool, &format!("SET ROLE {admin}")).await.unwrap();
        let reassign = format!("UPDATE folders SET owner = '{admin}' WHERE id = '{id}'");
        assert_eq!(pg_exec(&pool, &reassign).await.unwrap().rows_affected(), 1);

        pool.close().await;
    }
}
