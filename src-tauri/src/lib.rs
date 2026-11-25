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

mod app_log;
mod commands;
mod config;
mod credentials;
mod db;
mod highlight;
mod import;
mod limits;
mod login_pins;
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
use db::session_highlights::{SessionHighlightState, SessionHighlights};
use db::sqlite::SqliteProvider;
use db::tree_order::{TreeOrder, TreeOrderState};
use db::{
    CredentialDbState, DbState, HighlightDbState, LoginSequenceDbState, PgPoolState, PgUserState,
};
use pty::PtyState;
use sha2::{Digest, Sha384};
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

/// Apply the migrations this build carries that the database does not have
/// yet, as the role that owns the tables can; for every other role this is
/// the look that finds them all applied.
///
/// A start that finds them all applied reads [`MIGRATIONS_TABLE`] and
/// nothing more, so a regular role needs no more than `SELECT` on it, and
/// [`setup_postgres_rls`] leaves regular roles no more. Checksums that
/// differ by line endings alone are put right only where migrations are
/// about to run, which is what compares them: a checkout on Windows must
/// not fail against a database initialized from Linux or macOS, or the
/// other way round. That is an `UPDATE`, and so the owning role's as well.
async fn apply_postgres_migrations(pool: &sqlx::PgPool) -> Result<(), Box<dyn std::error::Error>> {
    if pg_migrations_up_to_date(pool).await? {
        tracing::info!("All migrations already applied — skipping sqlx::migrate!()");
        return Ok(());
    }
    fix_crlf_migration_checksums(pool).await?;
    sqlx::migrate!().run(pool).await?;
    Ok(())
}

/// Take over the highlight profiles that the shared database holds from
/// before they were each user's own, and say in the application log what
/// came of it; see [`SessionHighlights::take_over_central`].
///
/// A failure is not the start's: the app goes on with the profiles the local
/// database has, and the next start tries again. It can quote what the
/// database says, so it is logged escaped.
async fn take_over_central_highlights(highlights: &SessionHighlights, pool: &sqlx::PgPool) {
    match highlights.take_over_central(pool).await {
        Ok(None) => {}
        // Profiles that did not come over stay in the shared database and
        // are not read again, so that is a warning.
        Ok(Some(taken)) if taken.left_out > 0 || taken.not_read > 0 => tracing::warn!(
            copied = taken.copied,
            already_there = taken.already_there,
            left_out = taken.left_out,
            not_read = taken.not_read,
            sessions = taken.sessions,
            "Took over the highlight profiles of the shared database, but not all of them: some are no profile the app could have saved (left_out) or are past a limit of the take-over (not_read). From now on profiles are kept in the local database; see ADMIN_GUIDE.md §5"
        ),
        Ok(Some(taken)) => tracing::info!(
            copied = taken.copied,
            already_there = taken.already_there,
            sessions = taken.sessions,
            "Took over the highlight profiles of the shared database; from now on they are kept in the local database"
        ),
        Err(e) => tracing::warn!(
            "Could not take over the highlight profiles of the shared database; the next start tries again: {e:?}"
        ),
    }
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
    // Before the timeout of a connection below, whose text these two would
    // match as well. They are the limits of `db::postgres::pool_options`,
    // which a migration runs under like everything else.
    if raw.contains("lock timeout") {
        return "The database update could not get a lock that another session holds. \
                Start ShellStation again; if it keeps happening, see ADMIN_GUIDE.md §7."
            .to_string();
    }
    if raw.contains("statement timeout") {
        return "A statement of the database update took longer than the server allows. \
                See ADMIN_GUIDE.md §7 for giving the owning role more time."
            .to_string();
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

/// The tables that map a session to a user's own credential profile or login
/// sequence. A row belongs to the database role in its `owner` column; see
/// [`setup_postgres_rls`] and [`claim_legacy_mappings`].
const MAPPING_TABLES: [&str; 2] = ["session_credentials", "session_login_sequences"];

/// What [`setup_postgres_rls`] returns for a role that does not own the
/// tables, which is every regular user's start and nothing to act on. Any
/// other error is a setup that the owning role could not finish.
const RLS_SETUP_NOT_OWNER: &str = "RLS setup: the current role does not own the tables";

/// Policies of earlier versions that [`setup_postgres_rls`] drops, each with
/// its table.
///
/// `*_shared_update` let every role update every shared row. The app never
/// used that: its commands change only rows of the user's own. A role with
/// its own statements could, and so rewrite another user's shared session:
/// its host, port, user name and jump host, which decide where everyone who
/// opens the session sends their credentials, or its switch for legacy
/// algorithms. The same policy is what let any role take a row lock on a
/// shared row, since `SELECT … FOR UPDATE` goes by the policies for
/// `UPDATE`.
const OBSOLETE_POLICIES: [(&str, &str); 2] = [
    ("folders_shared_update", "folders"),
    ("sessions_shared_update", "sessions"),
];

/// The schema the trigger functions of [`setup_postgres_rls`] live in.
///
/// It belongs to the role that owns the tables, and no one else can create
/// anything in it: a new schema grants nothing to anyone, and making one
/// takes `CREATE` on the database, which regular users do not have. The
/// functions used to be created without a schema, which put them into
/// `public`. There a role that may create functions (every role before
/// `PostgreSQL` 15, and the users of Option B) could create one under the
/// setup's name before the owning role's first start. `CREATE OR REPLACE`
/// keeps a function's owner: an owning role that is no superuser then failed
/// at that step and got no triggers, and one that is replaced the body of a
/// function that stayed the other role's to change again, with the triggers
/// calling it.
///
/// Whoever fires a trigger needs no privilege on the schema or the function.
/// Naming a function takes `USAGE` on its schema, though, so a trigger that
/// calls one of these was made by a role with the owning role's privileges.
const RLS_SCHEMA: &str = "shellstation_rls";

/// The search path the trigger functions are pinned to, with the temporary
/// schema last: they run as the updating user, and a temporary table is
/// found before `pg_catalog` unless the path says otherwise.
const RLS_FUNCTION_PATH: &str = "pg_catalog, pg_temp";

/// When the triggers of [`setup_postgres_rls`] fire, as `pg_trigger.tgtype`
/// has it: a row trigger (1) that fires before (2) an update (16), and for
/// the ones that look at what a row is put into or names as its jump host,
/// an insert (4) as well.
const ON_CHANGE: i16 = 1 | 2 | 16;
const ON_PLACING: i16 = ON_CHANGE | 4;

/// The tables under row-level security; see [`setup_postgres_rls`].
const RLS_TABLES: [&str; 4] = ["folders", "sessions", MAPPING_TABLES[0], MAPPING_TABLES[1]];

/// The table in which sqlx keeps which migrations are applied. A start
/// decides by it whether there is one to apply
/// ([`pg_migrations_up_to_date`]), so whoever can write to it decides that
/// for every user: a row taken away or marked as failed sends every start
/// into the migrations, which regular roles are refused and the owning role
/// fails at, and a row for a migration of a later release has that one taken
/// for applied when it comes. Regular roles therefore read it and no more;
/// see [`setup_postgres_rls`].
const MIGRATIONS_TABLE: &str = "_sqlx_migrations";

/// What anyone but its owner holds on [`MIGRATIONS_TABLE`] beyond reading
/// it, as a subquery with a row for each: `grantee` is who, with 0 for
/// everyone, `privilege_type` what, and `col` the column where it is held on
/// one column alone, which the server keeps apart from what is held on the
/// table. A table or column that nobody was granted anything on has no list
/// to go through.
fn migrations_table_writers() -> String {
    let table = format!("'{MIGRATIONS_TABLE}'::pg_catalog.regclass::pg_catalog.oid");
    format!(
        "(SELECT a.grantee, a.privilege_type, NULL::pg_catalog.name AS col \
            FROM pg_catalog.pg_class c, pg_catalog.aclexplode(c.relacl) a \
           WHERE c.oid = {table} \
             AND a.grantee <> c.relowner AND a.privilege_type <> 'SELECT' \
          UNION ALL \
          SELECT a.grantee, a.privilege_type, t.attname \
            FROM pg_catalog.pg_class c \
            JOIN pg_catalog.pg_attribute t ON t.attrelid = c.oid, \
                 pg_catalog.aclexplode(t.attacl) a \
           WHERE c.oid = {table} \
             AND a.grantee <> c.relowner AND a.privilege_type <> 'SELECT')"
    )
}

/// The migrations the database has on record that this build does not
/// carry, by their versions.
///
/// A later version of the app that was started against the database leaves
/// such rows, and this build is then behind. Where none was, they were not
/// written by the app: until [`setup_postgres_rls`] left the table to its
/// owner, any regular role could add a row, and one for a migration that a
/// later release brings is taken for applied when that release comes
/// ([`pg_migrations_up_to_date`]). Before that release it is a version no
/// build carries, which is when it can be seen.
async fn migrations_not_of_this_build(pool: &sqlx::PgPool) -> Result<Vec<i64>, sqlx::Error> {
    let of_this_build: std::collections::HashSet<i64> =
        sqlx::migrate!().iter().map(|m| m.version).collect();
    let mut others: Vec<i64> = sqlx::query_scalar::<_, i64>("SELECT version FROM _sqlx_migrations")
        .fetch_all(pool)
        .await?
        .into_iter()
        .filter(|version| !of_this_build.contains(version))
        .collect();
    others.sort_unstable();
    Ok(others)
}

/// Says in the application log what [`migrations_not_of_this_build`] finds.
/// It is a warning and nothing more: a build that is behind the database is
/// an everyday thing, and the two cannot be told apart from here.
async fn warn_of_migrations_not_of_this_build(pool: &sqlx::PgPool) {
    match migrations_not_of_this_build(pool).await {
        Ok(others) if others.is_empty() => {}
        // No more of them than a line of the log should hold; the table can
        // have been anyone's to fill.
        Ok(others) => tracing::warn!(
            "The database has {} migrations on record that this version of ShellStation does not have, among them {:?}. A later version that was started against this database leaves such rows. If none was, ShellStation did not write them, and a migration with one of these numbers would be taken for applied when its release comes (ADMIN_GUIDE.md §5)",
            others.len(),
            &others[..others.len().min(20)]
        ),
        // The error can quote what the database says, so it is logged
        // escaped.
        Err(e) => tracing::warn!("Could not read the migrations the database has on record: {e:?}"),
    }
}

/// The trigger functions of [`setup_postgres_rls`], each with its name and
/// its body, in the order the setup creates them.
fn rls_functions() -> [(&'static str, String); 4] {
    // Triggers keep a row with its owner, in a folder of its owner's, and
    // behind a jump host of its owner's.
    //
    // A shared row can be changed by its owner alone: the policies give no
    // one else an UPDATE on it (see `OBSOLETE_POLICIES` for when they did).
    // What the policies cannot say is left to four functions:
    //
    // - `shellstation_keep_owner`: an owner cannot hand a row to another
    //   role. Owner changes are left to superusers (who reassign a removed
    //   user's rows, ADMIN_GUIDE.md §4) and the table owner, plus claiming a
    //   placeholder `'local'` row for oneself, as the fix-up at the top of
    //   the setup does. That fix-up finds such rows at the first setup
    //   only, while row-level security is not yet on; once it is, the
    //   policies let no role update a row that is not its own.
    // - `shellstation_keep_place`: a row is moved (`folder_id`, `parent_id`)
    //   by its owner, the table owner or a superuser.
    // - `shellstation_own_folder`: a row is put only into a folder of the
    //   role's own, when it is created and when it is moved. A new row has
    //   only its `owner` checked by the policies, and the foreign key to its
    //   folder is checked without RLS, so anyone could otherwise put a
    //   session of their own into another user's shared folder, where it
    //   shows among that user's. Top-level folders have no folder to check.
    //   Every folder above must be the role's own as well, up to the top: a
    //   folder of the role's that is in another user's, as one may be from
    //   before this was refused, would otherwise stay a place to put things
    //   into that user's tree. A folder above that the role cannot see is
    //   not its own either. The same look up the tree finds a folder that
    //   would be put into itself or below itself, which is refused as well:
    //   the queries that go down a folder's tree would not come to an end
    //   in such a ring. That is for folders only; a session has an id too,
    //   but is no folder to be found above one.
    // - `shellstation_own_jump_host`: a session names only a session of the
    //   role's own as its jump host, when it is created and when its jump
    //   host is changed. The policies check a row's `owner` and nothing it
    //   refers to, and the foreign key is checked without RLS, so the owner
    //   of a shared session could otherwise name any session by its id,
    //   another user's personal one included. Whoever then opened the shared
    //   session and had a profile for that jump host, as its owner has, went
    //   through it: a connection from their own bastion to a host and port
    //   of the other user's choosing. The other way round, the owner of the
    //   jump host could delete it, and the session that named it, which the
    //   foreign key then leaves without one, connected directly. And the
    //   foreign key's answer alone told whether a session with some id
    //   exists; this function answers first, and the same for a session
    //   that is not there as for one that is not the role's. With it, a
    //   chain of jump hosts has one owner, who alone changes any part of
    //   it. `session_connect` holds a chain to the same rule
    //   (`commands::session::jump_host_is_its_owners`), for what was named
    //   before this function refused it.
    //
    // The first two were all that kept a shared row from being taken over
    // or moved away while anyone could update it. They stay for a database
    // whose owning role has not started this version yet, and as a second
    // line behind the policies.
    //
    // The functions run as the updating user, so every name in them is
    // pinned: a temporary table is found before `pg_catalog` unless the path
    // says otherwise, and a temp table named `pg_class` would otherwise let
    // any user claim to own the table. `folders` is named with the schema of
    // the table the trigger is on, for the same reason. The functions
    // themselves are in a schema of the owning role's (`RLS_SCHEMA`), where
    // no one else can have put one. The setup replaces them on every run so
    // that a fix to them reaches existing databases, which locks no table;
    // only the table owner gets that far, its ownership check stops everyone
    // else. A failure to replace one does not stop the steps after it, but
    // its triggers are then left alone. `check_postgres_rls` compares the
    // bodies here with the ones in the database.
    let table_owner = "pg_catalog.pg_has_role(current_user, \
         (SELECT relowner FROM pg_catalog.pg_class WHERE oid = TG_RELID), 'MEMBER')";
    let refuse = |when: &str, message: &str| {
        format!(
            "BEGIN \
                 IF {when} AND NOT {table_owner} THEN \
                     RAISE EXCEPTION '{message}', TG_TABLE_NAME \
                         USING ERRCODE = 'insufficient_privilege'; \
                 END IF; \
                 RETURN NEW; \
             END"
        )
    };
    [
        (
            "shellstation_keep_owner",
            refuse(
                "NEW.owner IS DISTINCT FROM OLD.owner \
                 AND NOT (OLD.owner = 'local' AND NEW.owner = current_user)",
                "only the table owner can change the owner of a % row",
            ),
        ),
        (
            "shellstation_keep_place",
            refuse(
                "OLD.owner IS DISTINCT FROM current_user",
                "only its owner can move a % row",
            ),
        ),
        // The column that names the folder is the trigger's argument. An
        // update that leaves it as it was is not a move, whatever else it
        // changes.
        (
            "shellstation_own_folder",
            format!(
                "DECLARE folder text; was text; own boolean; ring boolean; \
                 BEGIN \
                     EXECUTE pg_catalog.format('SELECT ($1).%I', TG_ARGV[0]) USING NEW INTO folder; \
                     IF TG_OP = 'UPDATE' THEN \
                         EXECUTE pg_catalog.format('SELECT ($1).%I', TG_ARGV[0]) USING OLD INTO was; \
                         IF folder IS NOT DISTINCT FROM was THEN \
                             RETURN NEW; \
                         END IF; \
                     END IF; \
                     IF folder IS NULL OR {table_owner} THEN \
                         RETURN NEW; \
                     END IF; \
                     EXECUTE pg_catalog.format( \
                         'WITH RECURSIVE up AS ( \
                              SELECT f.id, f.parent_id, f.owner FROM %1$I.folders f \
                              WHERE f.id = $1 \
                              UNION \
                              SELECT f.id, f.parent_id, f.owner FROM %1$I.folders f \
                              JOIN up ON f.id = up.parent_id) \
                          SELECT count(*) > 0 AND bool_and(up.owner = current_user) \
                                 AND NOT EXISTS ( \
                                     SELECT 1 FROM up below \
                                     WHERE below.parent_id IS NOT NULL \
                                       AND NOT EXISTS (SELECT 1 FROM up above \
                                                       WHERE above.id = below.parent_id)), \
                                 bool_or(up.id = $2) \
                          FROM up', \
                         TG_TABLE_SCHEMA) USING folder, NEW.id INTO own, ring; \
                     IF ring AND TG_TABLE_NAME = 'folders' THEN \
                         RAISE EXCEPTION \
                             'a % row cannot be put into itself or below itself', \
                             TG_TABLE_NAME USING ERRCODE = 'insufficient_privilege'; \
                     END IF; \
                     IF own IS NOT TRUE THEN \
                         RAISE EXCEPTION \
                             'a % row can only be put into one''s own folders, and not below another user''s', \
                             TG_TABLE_NAME USING ERRCODE = 'insufficient_privilege'; \
                     END IF; \
                     RETURN NEW; \
                 END"
            ),
        ),
        // An update that leaves the jump host as it was is let through,
        // whatever else it changes: a session that named another user's
        // before this was refused can still be edited, and connects with
        // none of it (`commands::session::jump_host_is_its_owners`). The
        // look at `sessions` runs as the role and so under its policies,
        // where its own rows are all it is asked for.
        (
            "shellstation_own_jump_host",
            format!(
                "DECLARE own boolean; \
                 BEGIN \
                     IF TG_OP = 'UPDATE' THEN \
                         IF NEW.jump_host_id IS NOT DISTINCT FROM OLD.jump_host_id THEN \
                             RETURN NEW; \
                         END IF; \
                     END IF; \
                     IF NEW.jump_host_id IS NULL OR {table_owner} THEN \
                         RETURN NEW; \
                     END IF; \
                     EXECUTE pg_catalog.format( \
                         'SELECT EXISTS (SELECT 1 FROM %I.sessions s \
                                         WHERE s.id = $1 AND s.owner = current_user)', \
                         TG_TABLE_SCHEMA) USING NEW.jump_host_id INTO own; \
                     IF own IS NOT TRUE THEN \
                         RAISE EXCEPTION \
                             'a % row can only name one of one''s own sessions as its jump host', \
                             TG_TABLE_NAME USING ERRCODE = 'insufficient_privilege'; \
                     END IF; \
                     RETURN NEW; \
                 END"
            ),
        ),
    ]
}

/// A trigger of [`setup_postgres_rls`]: its name, its table, its function,
/// what it is created with, when it fires, and its column.
type RlsTrigger = (
    &'static str,
    &'static str,
    &'static str,
    String,
    i16,
    &'static str,
);

/// The triggers of [`setup_postgres_rls`].
fn rls_triggers() -> [RlsTrigger; 7] {
    // Each trigger with its table, its function, when it fires, that last
    // as `pg_trigger.tgtype` has it (`ON_CHANGE`, `ON_PLACING`), and its
    // column. The first two functions are for updates that change their
    // column. The other two are for new rows as well and look at the column
    // themselves: `shellstation_own_folder` is told which as its argument,
    // and `shellstation_own_jump_host`, which has one column to look at, is
    // called only for a row that names a jump host. A foreign key that
    // clears the column of a deleted jump host therefore does not call it.
    let on_change = |column: &str, table: &str, function: &str| {
        format!(
            "BEFORE UPDATE OF {column} ON {table} FOR EACH ROW \
             WHEN (OLD.{column} IS DISTINCT FROM NEW.{column}) \
             EXECUTE FUNCTION {RLS_SCHEMA}.{function}()"
        )
    };
    let on_placing = |column: &str, table: &str| {
        format!(
            "BEFORE INSERT OR UPDATE OF {column} ON {table} FOR EACH ROW \
             EXECUTE FUNCTION {RLS_SCHEMA}.shellstation_own_folder('{column}')"
        )
    };
    [
        (
            "folders_keep_owner",
            "folders",
            "shellstation_keep_owner",
            on_change("owner", "folders", "shellstation_keep_owner"),
            ON_CHANGE,
            "owner",
        ),
        (
            "sessions_keep_owner",
            "sessions",
            "shellstation_keep_owner",
            on_change("owner", "sessions", "shellstation_keep_owner"),
            ON_CHANGE,
            "owner",
        ),
        (
            "folders_keep_place",
            "folders",
            "shellstation_keep_place",
            on_change("parent_id", "folders", "shellstation_keep_place"),
            ON_CHANGE,
            "parent_id",
        ),
        (
            "sessions_keep_place",
            "sessions",
            "shellstation_keep_place",
            on_change("folder_id", "sessions", "shellstation_keep_place"),
            ON_CHANGE,
            "folder_id",
        ),
        (
            "folders_own_folder",
            "folders",
            "shellstation_own_folder",
            on_placing("parent_id", "folders"),
            ON_PLACING,
            "parent_id",
        ),
        (
            "sessions_own_folder",
            "sessions",
            "shellstation_own_folder",
            on_placing("folder_id", "sessions"),
            ON_PLACING,
            "folder_id",
        ),
        (
            "sessions_own_jump_host",
            "sessions",
            "shellstation_own_jump_host",
            format!(
                "BEFORE INSERT OR UPDATE OF jump_host_id ON sessions FOR EACH ROW \
                 WHEN (NEW.jump_host_id IS NOT NULL) \
                 EXECUTE FUNCTION {RLS_SCHEMA}.shellstation_own_jump_host()"
            ),
            ON_PLACING,
            "jump_host_id",
        ),
    ]
}

/// What a trigger of [`setup_postgres_rls`] has to be apart from the function
/// it calls, as a condition on a row `t` of `pg_trigger`: enabled, fired on
/// the events and for the column it should be, and with the argument and a
/// condition, or none, as it should have. What the condition says is not
/// compared.
fn rls_trigger_shape((_, _, _, definition, tgtype, column): &RlsTrigger) -> String {
    // Whether a trigger has an argument and a condition is read off the
    // definition it is created with, so the two cannot differ:
    // `shellstation_own_folder` has its column as its argument and no
    // condition, the other functions a condition and no argument.
    //
    // The argument is kept as its bytes with a zero byte after it, which
    // `encode` writes as a backslash and three zeros. The text to compare it
    // with is an `E''` string, in which a backslash is written twice
    // whatever `standard_conforming_strings` says.
    let argument = if definition.ends_with("()") {
        "t.tgnargs = 0".to_string()
    } else {
        format!(
            "t.tgnargs = 1 \
             AND pg_catalog.encode(t.tgargs, 'escape') = E'{column}\\\\000'"
        )
    };
    let condition = if definition.contains(" WHEN (") {
        "t.tgqual IS NOT NULL"
    } else {
        "t.tgqual IS NULL"
    };
    format!(
        "t.tgenabled = 'O' AND t.tgtype = {tgtype} \
         AND t.tgattr::pg_catalog.text = ( \
             SELECT a.attnum::pg_catalog.text FROM pg_catalog.pg_attribute a \
             WHERE a.attrelid = t.tgrelid AND a.attname = '{column}') \
         AND {argument} AND {condition}"
    )
}

/// A policy of [`setup_postgres_rls`]: its name, its table, the command it is
/// for, and what it is `USING` and, where it has one, `WITH CHECK`.
type RlsPolicy = (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
);

/// The policies of [`setup_postgres_rls`].
fn rls_policies() -> [RlsPolicy; 8] {
    // A shared row has a policy for reading it and none for changing it:
    // the owner's `*_owner_all` is the only one that lets a row be updated.
    // `*_shared_delete` says nothing that `*_owner_all` does not, and is
    // left as it is.
    [
        // Folders
        (
            "folders_owner_all",
            "folders",
            "ALL",
            "owner = current_user",
            None,
        ),
        (
            "folders_shared_read",
            "folders",
            "SELECT",
            "visibility = 'shared'",
            None,
        ),
        (
            "folders_shared_delete",
            "folders",
            "DELETE",
            "visibility = 'shared' AND owner = current_user",
            None,
        ),
        // Sessions
        (
            "sessions_owner_all",
            "sessions",
            "ALL",
            "owner = current_user",
            None,
        ),
        (
            "sessions_shared_read",
            "sessions",
            "SELECT",
            "visibility = 'shared'",
            None,
        ),
        (
            "sessions_shared_delete",
            "sessions",
            "DELETE",
            "visibility = 'shared' AND owner = current_user",
            None,
        ),
        // Per-user mappings. A role has its own rows, and of other rows only
        // the placeholders below: there is no shared row here, and one policy
        // for every command, so the new row of an insert or update has only
        // this `WITH CHECK` to pass. It cannot be given to another role, and
        // it cannot be a placeholder, also not for a role named `local`, to
        // which every placeholder would otherwise look like a row of its
        // own to write. It must also be for a session the role can see: the
        // foreign key is checked without RLS, and its answer alone would say
        // whether a session with some id exists.
        //
        // The placeholder rows are the ones from before a mapping had an
        // owner. Which role wrote one is not on record, so any role may
        // read, claim or delete it, as it could before, but only for a
        // session it can see: the subquery runs as the role asking and so
        // under the RLS of `sessions`. Mappings for a personal session are
        // therefore left to that session's owner. See
        // `claim_legacy_mappings`. A placeholder is no role's own row, also
        // not that of a role named `local`, which is held to the sessions it
        // can see like any other.
        //
        // An existing policy is left as it is, so a change to these two
        // needs a new name.
        (
            "session_credentials_owner_all",
            "session_credentials",
            "ALL",
            "(owner = current_user AND owner <> 'local') OR (owner = 'local' AND EXISTS \
             (SELECT 1 FROM sessions s WHERE s.id = session_credentials.session_id))",
            Some(
                "owner = current_user AND owner <> 'local' AND EXISTS \
                 (SELECT 1 FROM sessions s WHERE s.id = session_credentials.session_id)",
            ),
        ),
        (
            "session_login_sequences_owner_all",
            "session_login_sequences",
            "ALL",
            "(owner = current_user AND owner <> 'local') OR (owner = 'local' AND EXISTS \
             (SELECT 1 FROM sessions s WHERE s.id = session_login_sequences.session_id))",
            Some(
                "owner = current_user AND owner <> 'local' AND EXISTS \
                 (SELECT 1 FROM sessions s WHERE s.id = session_login_sequences.session_id)",
            ),
        ),
    ]
}

/// Idempotent `RLS` setup for `PostgreSQL` multi-user isolation.
///
/// Its statements run with the session's `search_path`, which has `public`
/// in it, where other roles may be able to create things (every role before
/// `PostgreSQL` 15, and the users of Option B). `pg_catalog` is searched
/// first, but an operator or function elsewhere on the path whose argument
/// types match exactly is preferred to one in `pg_catalog` that needs a
/// cast. An `=` for `oid` and `regclass` in `public` would thus be what
/// `tgrelid = 'folders'::regclass` runs, as the owning role, and it could
/// answer as it liked. So wherever the statements here compare, both sides
/// have the same type, for which `pg_catalog` has the operator: the name of
/// a table, function or schema is cast on to `oid`, and a number that is
/// compared with an `oid` is written as one. Functions are named with
/// `pg_catalog`.
///
/// Called after `sqlx::migrate!()` succeeds, at every start. Only the role
/// that owns the tables can apply it, and for any other role it ends at the
/// check for that.
///
/// A start that finds everything in place takes no lock on a table beyond
/// what the two fix-up updates at the top need, which conflict with nothing
/// a reader or writer holds. Each step looks in the catalog first and
/// changes only what is missing. `ALTER TABLE`, `CREATE POLICY` and
/// `CREATE TRIGGER` lock the whole table before they find that there is
/// nothing to do: the first two against every other use of it, the third
/// against writers. A lock against every other use waits for every open
/// transaction that has touched the table, and while it waits, everything
/// else that wants the table waits behind it, plain reads included. One
/// transaction left open somewhere would then hold up every user for as
/// long as the owning role's start waits, at each of its starts. A step
/// that does have something to change still takes its lock, and waits no
/// longer than the session's `lock_timeout` (`db::postgres::pool_options`).
/// If it gives up there, the setup leaves the rest of that table's steps
/// and goes on with the next table, and the next start of the owning role
/// goes on from there. Whatever failed is reported together at the end.
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

    // The rest is the table owner's to apply, and no one else gets past
    // here. An `ALTER TABLE` used to be what stopped everyone else, at every
    // start; the steps below no longer run one when nothing is missing, so
    // the question is asked outright. It is the one `ALTER TABLE` asks: a
    // superuser, or a role with the privileges of the role that owns the
    // table. Without it, a role that may create functions in the schema
    // (every role before PostgreSQL 15, and the users of Option B) could get
    // as far as the trigger functions below on a database that does not have
    // them yet, and the triggers would then call functions of that role's.
    let tables = RLS_TABLES
        .map(|table| format!("'{table}'::pg_catalog.regclass::pg_catalog.oid"))
        .join(", ");
    let owns_tables: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) = {} AND bool_and(pg_catalog.pg_has_role(c.relowner, 'USAGE')) \
         FROM pg_catalog.pg_class c WHERE c.oid IN ({tables})",
        RLS_TABLES.len()
    )))
    .fetch_one(pool)
    .await
    .map_err(|e| format!("RLS setup (table ownership): {e}"))?;
    if !owns_tables {
        return Err(RLS_SETUP_NOT_OWNER.to_string());
    }

    // From here on, a step that fails does not stop the ones after it, with
    // the two exceptions below. The steps do not depend on each other (but
    // for the functions, which need their schema, and the triggers, which
    // need their functions), so one that cannot be
    // applied to this database, such as a constraint that existing rows
    // break, must not keep the others from it at every start. Whatever
    // failed is reported together at the end.
    //
    // The steps go a table at a time, and row-level security is each
    // table's last, in one statement with its policies. Until that step, a
    // table is as it was before the setup; after it, everything the setup
    // has for it is in place. A step that gives up on a lock ends its
    // table's steps, which would wait for the same lock and each spend
    // `lock_timeout` on it before the app has a window, and the setup goes
    // on with the next table. Whoever makes a step give up, by holding a
    // lock when the owning role starts, thus has no say in which part of a
    // table's protection is applied, and none over the other tables. The
    // next start goes on from there. The other exception is a step that
    // fails at the connection and not at the database: every further step
    // would meet the same, so the setup ends there (`RlsStepEnds`).
    let mut failed: Vec<String> = Vec::new();

    // The table of applied migrations is the owning role's to write and
    // everyone else's to read (`MIGRATIONS_TABLE`). The admin guide's grants
    // for regular roles are on all tables of the schema, and until this step
    // existed they included writing to this one, so whatever anyone but its
    // owner holds beyond `SELECT`, on the table or on one of its columns, is
    // revoked, at every start, together with what a role that could pass
    // its grant on has passed on. A
    // privilege is named as the server names it, so one that a later version
    // adds is revoked as well. Nothing is revoked where nothing is held, so
    // a start that finds the table as it should be takes no lock on it. A
    // `REVOKE` locks it as a reader does: it holds up no one, and waits only
    // for a lock against every other use, on which it gives up like any
    // other step.
    let writers = migrations_table_writers();
    let read_only = format!(
        "DO $$ DECLARE held record; BEGIN \
             FOR held IN \
                 SELECT DISTINCT w.grantee, w.privilege_type, w.col FROM {writers} w \
             LOOP \
                 EXECUTE pg_catalog.format( \
                     'REVOKE %s%s ON {MIGRATIONS_TABLE} FROM %s CASCADE', \
                     held.privilege_type, \
                     CASE WHEN held.col IS NULL THEN '' \
                          ELSE pg_catalog.format(' (%I)', held.col) END, \
                     CASE WHEN held.grantee = 0::pg_catalog.oid THEN 'PUBLIC' \
                          ELSE pg_catalog.format('%I', \
                              (SELECT r.rolname FROM pg_catalog.pg_roles r \
                               WHERE r.oid = held.grantee)) END); \
             END LOOP; \
         END $$"
    );
    if let Err(failure) = rls_setup_step(
        pool,
        &format!("privileges on {MIGRATIONS_TABLE}"),
        &read_only,
    )
    .await
    {
        failed.push(failure.message);
        if failure.ends == RlsStepEnds::Setup {
            return Err(failed.join("; "));
        }
    }

    // The schema for the trigger functions (`RLS_SCHEMA`) is made for the
    // role that owns the tables, also when a member of that role or a
    // superuser runs this. One that is there and is another role's is given
    // to it, which only a superuser can do, or the role it belongs to if it
    // is a member of the owning role. Where that fails, the schema is not to
    // be trusted with the functions: none is created, and the triggers are
    // left alone.
    let owning_role = "(SELECT c.relowner FROM pg_catalog.pg_class c \
         WHERE c.oid = 'folders'::pg_catalog.regclass::pg_catalog.oid)";
    let owning_role_name =
        format!("(SELECT r.rolname FROM pg_catalog.pg_roles r WHERE r.oid = {owning_role})");
    //
    // A schema that changes hands keeps what its earlier owner granted on
    // it, and one of the owning role's may have been granted on since.
    // `USAGE` would let a role name the functions in a trigger of its own,
    // and `CREATE` put things beside them, so whatever anyone but the owning
    // role holds on the schema is revoked, at every start, together with
    // what a role that could pass its grant on has passed on.
    let schema = format!(
        "DO $$ DECLARE schema_owner oid; grantee oid; BEGIN \
             SELECT n.nspowner INTO schema_owner FROM pg_catalog.pg_namespace n \
                 WHERE n.nspname = '{RLS_SCHEMA}'; \
             IF schema_owner IS NULL THEN \
                 BEGIN \
                     EXECUTE pg_catalog.format( \
                         'CREATE SCHEMA {RLS_SCHEMA} AUTHORIZATION %I', {owning_role_name}); \
                 EXCEPTION WHEN duplicate_schema OR unique_violation THEN NULL; \
                 END; \
                 SELECT n.nspowner INTO schema_owner FROM pg_catalog.pg_namespace n \
                     WHERE n.nspname = '{RLS_SCHEMA}'; \
             END IF; \
             IF schema_owner IS DISTINCT FROM {owning_role} THEN \
                 EXECUTE pg_catalog.format( \
                     'ALTER SCHEMA {RLS_SCHEMA} OWNER TO %I', {owning_role_name}); \
             END IF; \
             FOR grantee IN \
                 SELECT DISTINCT a.grantee FROM pg_catalog.pg_namespace n, \
                     pg_catalog.aclexplode(n.nspacl) a \
                 WHERE n.nspname = '{RLS_SCHEMA}' AND a.grantee <> {owning_role} \
             LOOP \
                 IF grantee = 0::pg_catalog.oid THEN \
                     EXECUTE 'REVOKE ALL ON SCHEMA {RLS_SCHEMA} FROM PUBLIC CASCADE'; \
                 ELSE \
                     EXECUTE pg_catalog.format( \
                         'REVOKE ALL ON SCHEMA {RLS_SCHEMA} FROM %I CASCADE', \
                         (SELECT r.rolname FROM pg_catalog.pg_roles r WHERE r.oid = grantee)); \
                 END IF; \
             END LOOP; \
         END $$"
    );
    let functions = rls_functions();
    let schema_is_owners =
        match rls_setup_step(pool, &format!("schema {RLS_SCHEMA}"), &schema).await {
            Ok(()) => true,
            Err(failure) => {
                failed.push(failure.message);
                if failure.ends == RlsStepEnds::Setup {
                    return Err(failed.join("; "));
                }
                false
            }
        };

    // Each function is replaced and made the owning role's in one statement,
    // so that one a member of that role creates is not left as the member's.
    // `CREATE OR REPLACE` fails for a function that is there and is not this
    // role's to replace, and with it the statement.
    let mut replaced: Vec<&str> = Vec::new();
    for (name, body) in &functions {
        if !schema_is_owners {
            break;
        }
        let sql = format!(
            "DO $do$ BEGIN \
                 CREATE OR REPLACE FUNCTION {RLS_SCHEMA}.{name}() RETURNS trigger \
                 LANGUAGE plpgsql SET search_path = {RLS_FUNCTION_PATH} AS $fn$ {body} $fn$; \
                 IF (SELECT f.proowner FROM pg_catalog.pg_proc f \
                     WHERE f.oid = '{RLS_SCHEMA}.{name}()'::pg_catalog.regprocedure::pg_catalog.oid) \
                    IS DISTINCT FROM {owning_role} THEN \
                     EXECUTE pg_catalog.format( \
                         'ALTER FUNCTION {RLS_SCHEMA}.{name}() OWNER TO %I', \
                         {owning_role_name}); \
                 END IF; \
             END $do$"
        );
        match rls_setup_step(pool, &format!("function {name}"), &sql).await {
            Ok(()) => replaced.push(*name),
            Err(failure) => {
                failed.push(failure.message);
                if failure.ends == RlsStepEnds::Setup {
                    return Err(failed.join("; "));
                }
            }
        }
    }
    let triggers = rls_triggers();

    // Policies and row-level security are each table's in one statement,
    // which is one transaction: a table gets its policies and has row-level
    // security switched on, or neither. With it on and no policy, a table
    // refuses everything to everyone; with it off, its policies do nothing.
    // FORCE makes the policies apply to the table owner as well.
    //
    // A policy that is there is left as it is, but for the ones in
    // `OBSOLETE_POLICIES`, which are dropped in the same statement. When
    // there is something to do, the block takes the table's lock and looks
    // again, since two starts of the owning role at the same time would
    // both find it to do, and the second would fail at a policy that is
    // there, or gone, by then.
    let policies = rls_policies();

    // A mapping's key includes its owner. The tables came with a primary key
    // of (session_id, user_ident): with it, a role that wrote a row under
    // someone else's `user_ident` would keep that user from mapping the
    // session, since their own row would collide with one they cannot see.
    // The migration that added `owner` also added a unique index with it,
    // which the app's upserts go by; here it becomes the primary key, and
    // the old one goes. A migration cannot do this: the migrations are
    // shared with SQLite, which cannot drop a primary key.
    //
    // The index can be the primary key because it is unique, covers whole
    // columns, and all three are NOT NULL. Once the swap is done, a start
    // only looks. A start that finds it to do takes the table's lock and
    // looks again, since two starts of the owning role at the same time
    // would both find it to do, and the second would fail at a primary key
    // that is no longer there.
    for table in RLS_TABLES {
        let mut steps: Vec<(String, String)> = Vec::new();

        // CHECK constraint on visibility values.
        if ["folders", "sessions"].contains(&table) {
            steps.push((
                format!("constraint {table}_vis_ck"),
                format!(
                    "DO $$ BEGIN \
                         IF NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint \
                                        WHERE conrelid = '{table}'::pg_catalog.regclass::pg_catalog.oid \
                                          AND conname = '{table}_vis_ck') THEN \
                             ALTER TABLE {table} ADD CONSTRAINT {table}_vis_ck \
                                 CHECK (visibility IN ('personal', 'shared')); \
                         END IF; \
                     EXCEPTION WHEN duplicate_object THEN NULL; \
                     END $$"
                ),
            ));
        }

        // The primary key of a mapping table; see above.
        if MAPPING_TABLES.contains(&table) {
            steps.push((
                format!("primary key of {table}"),
                format!(
                    "DO $$ DECLARE pk name; BEGIN \
                         SELECT conname INTO pk FROM pg_catalog.pg_constraint \
                             WHERE conrelid = '{table}'::pg_catalog.regclass::pg_catalog.oid AND contype = 'p'; \
                         IF pk IS DISTINCT FROM '{table}_owner_key' THEN \
                             LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE; \
                             SELECT conname INTO pk FROM pg_catalog.pg_constraint \
                                 WHERE conrelid = '{table}'::pg_catalog.regclass::pg_catalog.oid \
                                   AND contype = 'p'; \
                             IF pk IS DISTINCT FROM '{table}_owner_key' THEN \
                                 IF pk IS NOT NULL THEN \
                                     EXECUTE pg_catalog.format( \
                                         'ALTER TABLE {table} DROP CONSTRAINT %I', pk); \
                                 END IF; \
                                 ALTER TABLE {table} ADD CONSTRAINT {table}_owner_key \
                                     PRIMARY KEY USING INDEX {table}_owner_key; \
                             END IF; \
                         END IF; \
                     END $$"
                ),
            ));
        }

        // The table's triggers, where their function is this run's. A
        // trigger of the name that is there is left alone only if it calls
        // that function, is enabled, fires on the events and for the column
        // it should, and has the argument and a condition, or none, as it
        // should. What the condition says is not compared. Any other is
        // replaced: one of an earlier version, which called a function in
        // `public`, and one that someone with `TRIGGER` on the table (the
        // users of Option B) made under the name, who cannot have it call
        // the setup's function (`RLS_SCHEMA`). Only when there is something
        // to do does the block take the table's lock, and it then looks
        // again, for two starts of the owning role at the same time.
        //
        // The function a replaced trigger called is dropped if it is one of
        // an earlier version, which goes by its name, and is this role's to
        // drop. While another trigger still calls it, it stays for that
        // trigger's turn. A function under the name that is someone else's
        // stays where it is, with nothing calling it.
        for trigger in triggers
            .iter()
            .filter(|trigger| trigger.1 == table && replaced.contains(&trigger.2))
        {
            let (name, _, function, definition, _, _) = trigger;
            let shape = rls_trigger_shape(trigger);
            let look = format!(
                "SELECT t.oid, t.tgfoid, \
                        t.tgfoid = '{RLS_SCHEMA}.{function}()'::pg_catalog.regprocedure::pg_catalog.oid \
                            AND {shape} \
                 INTO have, calls, fits \
                 FROM pg_catalog.pg_trigger t \
                 WHERE t.tgrelid = '{table}'::pg_catalog.regclass::pg_catalog.oid \
                   AND t.tgname = '{name}' AND NOT t.tgisinternal;"
            );
            steps.push((
                format!("trigger {name}"),
                format!(
                    "DO $$ DECLARE have oid; calls oid; fits boolean; BEGIN \
                         {look} \
                         IF fits THEN \
                             RETURN; \
                         END IF; \
                         LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE; \
                         {look} \
                         IF fits THEN \
                             RETURN; \
                         END IF; \
                         IF have IS NOT NULL THEN \
                             DROP TRIGGER {name} ON {table}; \
                         END IF; \
                         CREATE TRIGGER {name} {definition}; \
                         IF EXISTS (SELECT 1 FROM pg_catalog.pg_proc f \
                                    WHERE f.oid = calls AND f.proname = '{function}' \
                                      AND f.pronamespace <> '{RLS_SCHEMA}'::pg_catalog.regnamespace::pg_catalog.oid) \
                         THEN \
                             BEGIN \
                                 EXECUTE pg_catalog.format('DROP FUNCTION %s', \
                                     calls::pg_catalog.regprocedure); \
                             EXCEPTION \
                                 WHEN insufficient_privilege OR dependent_objects_still_exist \
                                     OR undefined_function THEN NULL; \
                             END; \
                         END IF; \
                     END $$"
                ),
            ));
        }

        // Its policies and row-level security, last.
        let rls_off = format!(
            "NOT (SELECT c.relrowsecurity AND c.relforcerowsecurity \
                  FROM pg_catalog.pg_class c \
                  WHERE c.oid = '{table}'::pg_catalog.regclass::pg_catalog.oid)"
        );
        let mut anything_missing = vec![rls_off.clone()];
        let mut create_missing = Vec::new();
        for (name, _) in OBSOLETE_POLICIES.iter().filter(|policy| policy.1 == table) {
            let present = format!(
                "EXISTS (SELECT 1 FROM pg_catalog.pg_policy \
                         WHERE polrelid = '{table}'::pg_catalog.regclass::pg_catalog.oid \
                           AND polname = '{name}')"
            );
            create_missing.push(format!(
                "IF {present} THEN DROP POLICY {name} ON {table}; END IF; "
            ));
            anything_missing.push(present);
        }
        for (name, _, cmd, using_expr, check_expr) in
            policies.iter().filter(|policy| policy.1 == table)
        {
            let missing = format!(
                "NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy \
                             WHERE polrelid = '{table}'::pg_catalog.regclass::pg_catalog.oid \
                               AND polname = '{name}')"
            );
            let with_check = check_expr.map_or_else(String::new, |e| format!(" WITH CHECK ({e})"));
            create_missing.push(format!(
                "IF {missing} THEN \
                     CREATE POLICY {name} ON {table} FOR {cmd} \
                         USING ({using_expr}){with_check}; \
                 END IF; "
            ));
            anything_missing.push(missing);
        }
        let anything_missing = anything_missing.join(" OR ");
        let create_missing = create_missing.concat();
        steps.push((
            format!("policies and row level security on {table}"),
            format!(
                "DO $$ BEGIN \
                     IF {anything_missing} THEN \
                         LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE; \
                         {create_missing}\
                         IF {rls_off} THEN \
                             ALTER TABLE {table} ENABLE ROW LEVEL SECURITY; \
                             ALTER TABLE {table} FORCE ROW LEVEL SECURITY; \
                         END IF; \
                     END IF; \
                 END $$"
            ),
        ));

        for (what, sql) in &steps {
            let Err(failure) = rls_setup_step(pool, what, sql).await else {
                continue;
            };
            failed.push(failure.message);
            match failure.ends {
                RlsStepEnds::Step => {}
                RlsStepEnds::Table => break,
                RlsStepEnds::Setup => return Err(failed.join("; ")),
            }
        }
    }

    if failed.is_empty() {
        tracing::info!("PostgreSQL RLS setup complete");
        Ok(())
    } else {
        Err(failed.join("; "))
    }
}

/// What a step of [`setup_postgres_rls`] that failed puts an end to.
#[derive(Debug, PartialEq, Eq)]
enum RlsStepEnds {
    /// Itself only: the database refused it, and the next step is another
    /// matter.
    Step,
    /// The rest of its table's steps: it gave up waiting, on a lock
    /// (`lock_not_available`, which `lock_timeout` raises) or as a statement
    /// that took too long (`query_canceled`, which `statement_timeout`
    /// raises, and which comes first where the server sets it shorter than
    /// `lock_timeout`). The table's other steps would wait as long.
    Table,
    /// The setup: the failure is not the database's answer to the step but
    /// the connection's, and every further step would meet it again, each
    /// after the pool's wait for a connection.
    Setup,
}

/// A step of [`setup_postgres_rls`] that failed: what to report for it, and
/// what it ends.
struct RlsStepFailure {
    message: String,
    ends: RlsStepEnds,
}

/// Runs one step of [`setup_postgres_rls`], which is applied or was already
/// when this returns `Ok`.
async fn rls_setup_step(pool: &sqlx::PgPool, what: &str, sql: &str) -> Result<(), RlsStepFailure> {
    let Err(e) = sqlx::query(sqlx::AssertSqlSafe(sql)).execute(pool).await else {
        return Ok(());
    };
    let ends = match &e {
        sqlx::Error::Database(db) => match db.code().as_deref() {
            Some("55P03" | "57014") => RlsStepEnds::Table,
            _ => RlsStepEnds::Step,
        },
        _ => RlsStepEnds::Setup,
    };
    Err(RlsStepFailure {
        message: format!("RLS setup ({what}): {e}"),
        ends,
    })
}

/// Something [`check_postgres_rls`] did not find the way
/// [`setup_postgres_rls`] leaves it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RlsGap {
    /// The table it belongs to; `None` for a trigger function and for the
    /// schema they are in.
    table: Option<&'static str>,
    /// What it is, in the words the setup reports a failed step with.
    what: String,
}

/// Whether what [`setup_postgres_rls`] applies is in the database, and what
/// of it is not.
///
/// Only the role that owns the tables can apply the setup, and only a start
/// of it that gets through does. Every other role works against the database
/// as it is: before the owning role has started a release that changes the
/// setup, and after a start of it that gave up on a lock someone held. So
/// every start asks, as whatever role it is, since the catalog is everyone's
/// to read.
///
/// It asks what the setup's own steps ask before they change anything, and
/// so goes by what they go by: a policy, a constraint and a primary key by
/// their names, a trigger by the function it calls and by its shape
/// ([`rls_trigger_shape`]). Beyond that it asks about the two things the
/// setup does at every start without looking. It replaces each trigger
/// function, so the check compares what a `CREATE OR REPLACE` sets with what
/// this version creates: the body, the search path the function is pinned
/// to, and that it runs as its caller. A function that
/// differs is one a start of the owning role has still to replace, or one of
/// another version than this, a later one included. And it revokes whatever
/// anyone but the owning role holds on the functions' schema, so the check
/// asks whether anyone does. The same goes for the table of applied
/// migrations ([`MIGRATIONS_TABLE`]), on which the setup leaves everyone but
/// its owner no more than reading: the check asks whether anyone holds more,
/// and whether the table is the owning role's.
///
/// An empty answer says that the setup's parts are there, not that nothing
/// else is. What a policy of the setup's name says is not read, and neither
/// a trigger nor a policy of someone's own is looked for; the users of Option
/// B can make the first.
///
/// It is one statement, which reads the catalog and nothing else, and so
/// waits for no lock anyone holds on a table. As in the setup, the two sides
/// of each comparison have the same type (see there). A function is found by
/// its name and its schema's: a cast to `regprocedure`, as the setup uses,
/// takes `USAGE` on the schema, which only the owning role has.
async fn check_postgres_rls(pool: &sqlx::PgPool) -> Result<Vec<RlsGap>, String> {
    let oid = |table: &str| format!("'{table}'::pg_catalog.regclass::pg_catalog.oid");
    let owning_role = format!(
        "(SELECT c.relowner FROM pg_catalog.pg_class c WHERE c.oid = {})",
        oid("folders")
    );
    let function_named = |name: &str| {
        format!(
            "FROM pg_catalog.pg_proc p \
             JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = '{RLS_SCHEMA}' AND p.proname = '{name}' AND p.pronargs = 0"
        )
    };

    // Each part of the setup with the table it belongs to, what it is, and
    // a question that is true where it is in place.
    //
    // The schema is there and the owning role's, and no one else holds a
    // privilege on it. A schema nobody has been granted anything on has no
    // list of privileges to go through.
    let mut asked: Vec<(Option<&'static str>, String, String)> = vec![
        (
            None,
            format!("schema {RLS_SCHEMA}"),
            format!(
                "EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n \
                         WHERE n.nspname = '{RLS_SCHEMA}' AND n.nspowner = {owning_role})"
            ),
        ),
        (
            None,
            format!("schema {RLS_SCHEMA}, on which others hold privileges"),
            format!(
                "NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n, \
                                 pg_catalog.aclexplode(n.nspacl) a \
                             WHERE n.nspname = '{RLS_SCHEMA}' AND a.grantee <> {owning_role})"
            ),
        ),
    ];
    // A function is there, and the owning role's; and it is this version's,
    // which one that is not there is not asked. The body is the statement's
    // parameter of the function's number. The search path is kept as the
    // one setting of the function, written `name=value`; the white space in
    // it is left out of the comparison. A function without a setting has
    // none to compare, which is not this version's either.
    let functions = rls_functions();
    let pinned_path = format!("search_path={RLS_FUNCTION_PATH}").replace(' ', "");
    for (number, (name, _)) in (1..).zip(&functions) {
        let named = function_named(name);
        asked.push((
            None,
            format!("function {name}"),
            format!("EXISTS (SELECT 1 {named} AND p.proowner = {owning_role})"),
        ));
        asked.push((
            None,
            format!("function {name}, which is not this version's"),
            format!(
                "NOT EXISTS (SELECT 1 {named} AND NOT COALESCE( \
                     p.prosrc = ${number} AND NOT p.prosecdef \
                     AND pg_catalog.replace( \
                             pg_catalog.array_to_string(p.proconfig, ','), ' ', '') \
                         = '{pinned_path}', false))"
            ),
        ));
    }
    let triggers = rls_triggers();
    let policies = rls_policies();
    for table in RLS_TABLES {
        let on_table = oid(table);
        let constraint = |name: &str, kind: &str| {
            format!(
                "EXISTS (SELECT 1 FROM pg_catalog.pg_constraint \
                         WHERE conrelid = {on_table} AND conname = '{name}'{kind})"
            )
        };
        if ["folders", "sessions"].contains(&table) {
            asked.push((
                Some(table),
                format!("constraint {table}_vis_ck"),
                constraint(&format!("{table}_vis_ck"), ""),
            ));
        }
        if MAPPING_TABLES.contains(&table) {
            asked.push((
                Some(table),
                format!("primary key of {table}"),
                constraint(&format!("{table}_owner_key"), " AND contype = 'p'"),
            ));
        }
        for trigger in triggers.iter().filter(|trigger| trigger.1 == table) {
            let (name, _, function, ..) = trigger;
            let named = function_named(function);
            let shape = rls_trigger_shape(trigger);
            asked.push((
                Some(table),
                format!("trigger {name}"),
                format!(
                    "EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t \
                             WHERE t.tgrelid = {on_table} AND t.tgname = '{name}' \
                               AND NOT t.tgisinternal \
                               AND t.tgfoid = (SELECT p.oid {named}) AND {shape})"
                ),
            ));
        }
        let policy = |name: &str| {
            format!(
                "EXISTS (SELECT 1 FROM pg_catalog.pg_policy \
                         WHERE polrelid = {on_table} AND polname = '{name}')"
            )
        };
        for (name, _) in OBSOLETE_POLICIES.iter().filter(|policy| policy.1 == table) {
            asked.push((
                Some(table),
                format!("policy {name}, which is to go"),
                format!("NOT {}", policy(name)),
            ));
        }
        for (name, ..) in policies.iter().filter(|policy| policy.1 == table) {
            asked.push((Some(table), format!("policy {name}"), policy(name)));
        }
        asked.push((
            Some(table),
            format!("row level security on {table}"),
            format!(
                "(SELECT c.relrowsecurity AND c.relforcerowsecurity \
                  FROM pg_catalog.pg_class c WHERE c.oid = {on_table})"
            ),
        ));
    }

    // The table of applied migrations is the owning role's, and no one else
    // holds more on it than reading it.
    asked.push((
        Some(MIGRATIONS_TABLE),
        format!("table {MIGRATIONS_TABLE}, which is not the owning role's"),
        format!(
            "EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
                     WHERE c.oid = {} AND c.relowner = {owning_role})",
            oid(MIGRATIONS_TABLE)
        ),
    ));
    asked.push((
        Some(MIGRATIONS_TABLE),
        format!("privileges on {MIGRATIONS_TABLE}"),
        format!(
            "NOT EXISTS (SELECT 1 FROM {} w)",
            migrations_table_writers()
        ),
    ));

    // Each question has an answer; one that came back without would be a
    // part that is not in place. A table that is not there fails the whole
    // statement, at the cast of its name.
    let questions = asked
        .iter()
        .map(|(_, _, question)| format!("COALESCE(({question}), false)"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut query = sqlx::query_scalar::<_, Vec<bool>>(sqlx::AssertSqlSafe(format!(
        "SELECT ARRAY[{questions}]"
    )));
    // As the setup's `CREATE FUNCTION` has the body, between its quotes.
    for (_, body) in &functions {
        query = query.bind(format!(" {body} "));
    }
    let in_place = query
        .fetch_one(pool)
        .await
        .map_err(|e| format!("RLS check: {e}"))?;
    if in_place.len() != asked.len() {
        return Err("RLS check: the answer does not fit the questions".to_string());
    }
    Ok(asked
        .into_iter()
        .zip(in_place)
        .filter(|(_, in_place)| !in_place)
        .map(|((table, what, _), _)| RlsGap { table, what })
        .collect())
}

/// What the window is told of `gaps`: the tables they are on, in the order
/// of [`RLS_TABLES`] and with [`MIGRATIONS_TABLE`] after them, and whether
/// the trigger functions are among them.
fn rls_gaps_status(gaps: &[RlsGap]) -> commands::RlsSetupStatus {
    commands::RlsSetupStatus::Incomplete {
        tables: RLS_TABLES
            .into_iter()
            .chain([MIGRATIONS_TABLE])
            .filter(|table| gaps.iter().any(|gap| gap.table == Some(*table)))
            .map(str::to_string)
            .collect(),
        functions: gaps.iter().any(|gap| gap.table.is_none()),
    }
}

/// Runs [`check_postgres_rls`] and says what it found: in the application
/// log with every part, and as its return value for the window, which shows
/// it ([`commands::DbStatus`]). A check that failed is reported as that, and
/// not as a setup in place.
async fn rls_setup_status(pool: &sqlx::PgPool) -> commands::RlsSetupStatus {
    match check_postgres_rls(pool).await {
        Ok(gaps) if gaps.is_empty() => {
            tracing::info!("PostgreSQL RLS setup: every part this version applies was found");
            commands::RlsSetupStatus::InPlace
        }
        // The parts are named by this app, with nothing of the database's
        // in them.
        Ok(gaps) => {
            let parts = gaps
                .iter()
                .map(|gap| gap.what.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            tracing::error!(
                "PostgreSQL RLS setup is not in the database as this version applies it. What is missing is not in effect until a start of the role that owns the tables gets through with this version; a function that is not this version's can also be a later version's (ADMIN_GUIDE.md §5). Missing or different: {parts}"
            );
            rls_gaps_status(&gaps)
        }
        // The error can quote what the database says, so it is logged
        // escaped.
        Err(e) => {
            tracing::error!("Could not check whether the PostgreSQL RLS setup is in place: {e:?}");
            commands::RlsSetupStatus::Unchecked
        }
    }
}

/// Claim the per-user mappings written under `user_ident` before a mapping
/// had an owner, and return how many there were.
///
/// The migration that added `owner` left those rows with the placeholder
/// `'local'`, and nothing connects with a row that is still a placeholder.
/// Each client takes the ones under its own `user_ident`, at startup and
/// when that name is set ([`claim_legacy_mappings_and_log`]), as
/// `setup_postgres_rls` does for placeholder folders and sessions. Unlike
/// that setup, this needs no more than a regular user's privileges.
///
/// A placeholder row was written when any role could write any row, so
/// whether its user wrote it cannot be known: this takes it by its name.
/// It is limited to sessions the role can see, like the policy that allows
/// it. A session the user has mapped again since keeps that mapping, and
/// its placeholder is deleted, so that it does not stay behind for others
/// to read.
async fn claim_legacy_mappings(pool: &sqlx::PgPool, user_ident: &str) -> Result<u64, String> {
    // A row of this role's own for the same session and name.
    let mapped_again = |table: &str| {
        format!(
            "EXISTS (SELECT 1 FROM {table} AS own \
                 WHERE own.session_id = legacy.session_id \
                   AND own.owner = current_user \
                   AND own.user_ident = legacy.user_ident)"
        )
    };
    let mut claimed = 0;
    for table in MAPPING_TABLES {
        let mapped_again = mapped_again(table);
        let placeholder = "legacy.owner = 'local' AND legacy.user_ident = $1 \
             AND EXISTS (SELECT 1 FROM sessions s WHERE s.id = legacy.session_id)";
        let claim = format!(
            "UPDATE {table} AS legacy SET owner = current_user \
             WHERE {placeholder} AND NOT {mapped_again}"
        );
        claimed += sqlx::query(sqlx::AssertSqlSafe(claim.as_str()))
            .bind(user_ident)
            .execute(pool)
            .await
            .map_err(|e| format!("Claiming legacy rows of {table}: {e}"))?
            .rows_affected();
        let clear = format!("DELETE FROM {table} AS legacy WHERE {placeholder} AND {mapped_again}");
        sqlx::query(sqlx::AssertSqlSafe(clear.as_str()))
            .bind(user_ident)
            .execute(pool)
            .await
            .map_err(|e| format!("Clearing legacy rows of {table}: {e}"))?;
    }
    Ok(claimed)
}

/// [`claim_legacy_mappings`], with its outcome in the application log. A
/// claim that fails is not fatal: the mappings stay placeholders, and the
/// next start tries again.
pub(crate) async fn claim_legacy_mappings_and_log(pool: &sqlx::PgPool, user_ident: &str) {
    match claim_legacy_mappings(pool, user_ident).await {
        Ok(0) => {}
        Ok(claimed) => tracing::info!(
            claimed,
            "Claimed per-user session mappings from before they had an owner"
        ),
        Err(e) => tracing::warn!(
            "Per-user session mappings from before they had an owner were not claimed: {e}"
        ),
    }
}

/// Menu `ID` of the app's own quit item; see [`build_app_menu`].
const QUIT_MENU_ID: &str = "app_quit";

/// Build the application menu, mirroring the Tauri default but using a custom
/// Help submenu passed by the caller.
///
/// The quit item is the app's own. `PredefinedMenuItem::quit` ends the
/// process on the spot (`terminate:` on macOS, `PostQuitMessage` on Windows)
/// with no event the app could hold it on, which would leave the sessions
/// for the operating system to close. This one quits through
/// `commands::quit_app`, which closes them first. Like the predefined item,
/// it does not ask to confirm: the confirmation lives in the webview, and the
/// item must still quit when the webview is hung.
fn build_app_menu(
    app: &tauri::AppHandle,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    about: &tauri::menu::AboutMetadata,
    help_menu: &Submenu<tauri::Wry>,
) -> Result<Menu<tauri::Wry>, Box<dyn std::error::Error>> {
    #[cfg(target_os = "macos")]
    let app_name = about.name.as_deref().unwrap_or("ShellStation");
    #[cfg(target_os = "macos")]
    let quit = MenuItemBuilder::with_id(QUIT_MENU_ID, format!("Quit {app_name}"))
        .accelerator("CmdOrCtrl+Q")
        .build(app)?;
    // Labelled as the predefined item was; it has no accelerator here either,
    // since Ctrl+Q reaches the webview.
    #[cfg(not(any(
        target_os = "macos",
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd"
    )))]
    let quit = MenuItemBuilder::with_id(QUIT_MENU_ID, "&Exit").build(app)?;

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
                app_name,
                true,
                &[
                    &PredefinedMenuItem::about(app, None, Some(about.clone()))?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::services(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::hide(app, None)?,
                    &PredefinedMenuItem::hide_others(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &quit,
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
                    &quit,
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
/// a file layer that writes to a file for each day,
/// `<log_directory>/shellstation.YYYY-MM-DD.log` (default
/// `<config_dir>/logs/`), which the app opens itself: see [`app_log`]. The
/// returned `WorkerGuard` must be held for the process lifetime — dropping
/// it flushes pending writes and shuts down the background writer thread.
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
    let Some(AppLogFile {
        writer,
        dir: log_dir,
        dir_problem,
        file_note,
    }) = log_file
    else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(stdout_layer)
            .init();
        return None;
    };

    // Lossy, which is the default: the writer logs from the thread that
    // writes the log, and must never wait for room in its own queue.
    let (non_blocking, guard) = tracing_appender::non_blocking(writer);
    let file_layer = fmt::layer().with_ansi(false).with_writer(non_blocking);
    tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(file_layer)
        .init();
    // The directory is whatever the config file holds, so it is logged
    // escaped: a line break in it must not start a log line.
    tracing::info!(
        log_dir = ?log_dir,
        level = %level,
        "Application file logging enabled (daily rotation)"
    );
    // Said in the log itself as well: stderr is seldom where a user looks.
    if let Some(problem) = dir_problem {
        tracing::warn!(log_dir = ?log_dir, "{problem}");
    }
    if let Some(note) = file_note {
        tracing::warn!(log_dir = ?log_dir, "{note}");
    }
    Some(guard)
}

/// The application log as [`open_app_log_file`] opens it.
struct AppLogFile {
    writer: app_log::DailyLog,
    /// The directory the log is in.
    dir: std::path::PathBuf,
    /// Why the directory could not be restricted to its owner, if it could
    /// not: the log is written there all the same.
    dir_problem: Option<String>,
    /// That the log is not in the file of the day's own name, and why, if
    /// that name held something that is not appended to.
    file_note: Option<String>,
}

/// Open the application log's file of the day for `init_tracing`.
///
/// Returns `None` when the configured directory is invalid or the directory
/// or the file cannot be created, so that the caller falls back to
/// stdout-only logging. The reason goes straight to stderr, because the
/// tracing subscriber is not installed yet.
fn open_app_log_file(
    config_dir: &std::path::Path,
    cfg: &config::AppLoggingConfig,
) -> Option<AppLogFile> {
    let log_dir = match &cfg.log_directory {
        Some(p) if !p.is_empty() => match validate_config_path(p, "Application log directory") {
            Ok(path) => path,
            Err(e) => {
                eprintln!("{e} — application logging disabled for this session.");
                return None;
            }
        },
        _ => config_dir.join("logs"),
    };
    // The log directory is treated as the session logger treats its own,
    // whether it is the default or one the user chose: a missing one is
    // created for its owner alone (0700 on Unix, an access list of its own
    // on Windows), as is every directory above it that is missing too, and
    // one that is already there is restricted to its owner
    // on Unix and left as it is on Windows. The log's files are created for
    // their owner alone as well (`app_log`), so a directory that could not
    // be restricted shows others their names and no more.
    if let Err(e) = crate::private_fs::create_private_dir(&log_dir) {
        eprintln!(
            "Failed to create application log directory {}: {e} — \
             application logging disabled for this session.",
            log_dir.display()
        );
        return None;
    }
    // A log directory may be where others can put a link, so one at its
    // name is not followed for this, and a directory that belongs to another
    // user is refused. That goes for the default directory as well, as it
    // does in the session logger: it is in the config directory, where only
    // the user can put a link, but only as long as that directory could be
    // restricted itself. A file system without Unix modes can refuse this
    // too. The directory is still usable, so logging goes ahead, into a
    // directory that others may list.
    let restricted = crate::private_fs::restrict_dir_to_owner_nofollow(&log_dir);
    let dir_problem = restricted.err().map(|e| {
        format!(
            "Failed to restrict the application log directory to its owner: {e} — \
             others may be able to list it, and to read the log where the file \
             system has no Unix modes"
        )
    });
    if let Some(problem) = &dir_problem {
        eprintln!("{problem} ({})", log_dir.display());
    }
    // Creating the directory can succeed and opening the file still fail
    // (directory not writable, disk full); that falls back the same way.
    match app_log::DailyLog::open(&log_dir) {
        Ok((writer, file_note)) => {
            if let Some(note) = &file_note {
                eprintln!("{note} ({})", log_dir.display());
            }
            Some(AppLogFile {
                writer,
                dir: log_dir,
                dir_problem,
                file_note,
            })
        }
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
    // First: it changes the environment, which is safe only while this is
    // the process's one thread. Nothing before it may start one. Logged once
    // there is a log.
    let pg_hostaddr_taken = config::take_pg_hostaddr_env();

    let config_dir = config::app_config_dir();
    // Owner-only on Unix, and on Windows when it is created here: it holds
    // the config, the local database and the logs. The subscriber is not
    // installed yet, so a problem goes to stderr now and to the log once
    // there is one.
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
    // A tracing call inside load_config would be a no-op here (no subscriber
    // yet), so it returns what it has to say as notes, which are logged
    // right after init_tracing.
    let (mut app_config, config_notes) = config::load_config(&config_dir);
    let _log_guard = init_tracing(&config_dir, &app_config.app_logging);
    if let Some(problem) = config_dir_problem {
        tracing::warn!(path = %config_dir.display(), "{problem}");
    }
    // The host is whatever the config file holds, so it is logged escaped:
    // a line break in it must not start a log line.
    tracing::info!(
        backend = ?app_config.db_backend,
        pg_host = ?app_config.postgres.host,
        pg_port = app_config.postgres.port,
        "ShellStation starting"
    );
    for note in &config_notes {
        note.log();
    }
    if pg_hostaddr_taken {
        tracing::warn!(
            "PGHOSTADDR is set in the environment and is not used for the database \
             connection, which goes to the host in Settings; local shells still get it"
        );
    }
    // After init_tracing, so that a keychain problem is logged.
    config::load_postgres_password(&mut app_config);

    tauri::Builder::default()
        .manage(PtyState::default())
        .manage(SshState::default())
        .manage(TelnetState::default())
        .manage(TerminalReadyState::default())
        .manage(commands::SearchGate::default())
        .on_page_load(|webview, payload| {
            // Only the main window holds terminal tabs. A load there, a
            // reload included, starts the frontend over without them. Any
            // navigation in that window counts, so the frontend must never
            // navigate it (no in-app routing by URL, no links opened in it).
            if webview.label() == "main" && payload.event() == tauri::webview::PageLoadEvent::Started
            {
                commands::page_load_started(webview.app_handle());
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
                    // Straight from here, not through the frontend, so that
                    // the item still quits when the webview is hung.
                    QUIT_MENU_ID => commands::quit_app(handle),
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

            // What the user has confirmed of other users' shared sessions,
            // kept with the local database whose profiles it is about.
            app.manage(login_pins::LoginPinState::new(
                config_dir.join("session-logins.json"),
            ));

            // Initialize session logger
            let log_dir = match &app_config.logging.log_directory {
                Some(p) if !p.is_empty() => validate_config_path(p, "Session log directory")
                    .unwrap_or_else(|e| {
                        eprintln!("{e} — using default log directory.");
                        config_dir.join("logs")
                    }),
                _ => config_dir.join("logs"),
            };
            let mut log_manager = session_logger::SessionLogManager::new(
                app_config.logging.enabled,
                log_dir,
                app_config.logging.filename_format.clone(),
            );
            log_manager.limits = session_logger::LogLimits::from(&app_config.logging);
            // A session that goes on without a log says so in its tab, which
            // shows a toast (`Terminal.tsx`). The logger calls this from a
            // log's writer thread, where a session's reader waits for its
            // log, or under its lock, which the output of every session
            // waits for, so the event is sent from a task of its own:
            // sending takes locks of Tauri's and hands work to the webview.
            let notify_handle = app.handle().clone();
            log_manager.notify_when_given_up(move |id| {
                let handle = notify_handle.clone();
                let id = id.to_string();
                tauri::async_runtime::spawn(async move {
                    let event = session_logger::not_logged_event(&id);
                    if let Err(e) = handle.emit(&event, ()) {
                        tracing::warn!(
                            connection_id = %id,
                            error = %e,
                            "Could not tell the window that a session is not logged"
                        );
                    }
                });
            });
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
                            // The error can quote the path, which is whatever
                            // the config file holds, so it is logged escaped.
                            tracing::error!(
                                error = ?e.to_string(),
                                "Custom SQLite path failed — falling back to default"
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
                        provider.clone() as Arc<dyn db::DatabaseProvider>
                    ));
                    // One database: a session row names its highlight
                    // profile, and the foreign key holds it to one.
                    app.manage(HighlightDbState(
                        provider as Arc<dyn db::DatabaseProvider>,
                    ));
                    app.manage(SessionHighlightState(None));
                    app.manage(TreeOrderState(None));
                    app.manage(PgPoolState(None));
                    app.manage(PgUserState(None));
                    app.manage(DbStatusState(commands::DbStatus {
                        backend: "sqlite".to_string(),
                        healthy: db_error.is_none(),
                        error: db_error,
                        pg_user: None,
                        rls_setup: None,
                    }));
                }
                DbBackend::Postgres => {
                    // Credentials always stay in local SQLite. FK enforcement is
                    // OFF because sessions live in PostgreSQL — the credentials
                    // table's FK to sessions would fail cross-database.
                    let local_pool = init_local_sqlite(&config_dir, false, None)?;
                    // Highlight profiles stay local as well, and with them
                    // which profile each session is shown with: a profile's
                    // patterns run in the window of whoever uses it, so no
                    // other role of the shared database gets to write them.
                    // Without them the app does not start, as without the
                    // local database itself: there is no going on without
                    // the user's choices that would not fall back on what
                    // the rows of the shared database name.
                    let session_highlights = tauri::async_runtime::block_on(
                        SessionHighlights::open(local_pool.clone()),
                    )
                    .map_err(std::io::Error::other)?;
                    // Where other users' folders and sessions come among the
                    // user's own is the user's as well, and kept locally.
                    let tree_order =
                        tauri::async_runtime::block_on(TreeOrder::open(local_pool.clone()))
                            .map_err(std::io::Error::other)?;
                    let cred_provider =
                        Arc::new(SqliteProvider::new(local_pool)) as Arc<dyn db::DatabaseProvider>;
                    app.manage(CredentialDbState(cred_provider.clone()));
                    app.manage(LoginSequenceDbState(cred_provider.clone()));
                    app.manage(HighlightDbState(cred_provider.clone()));
                    app.manage(SessionHighlightState(Some(session_highlights.clone())));
                    app.manage(TreeOrderState(Some(tree_order)));

                    // Host and database are whatever the config file holds,
                    // so they are logged escaped.
                    tracing::info!(
                        host = ?app_config.postgres.host,
                        port = app_config.postgres.port,
                        database = ?app_config.postgres.database,
                        ssl_mode = %app_config.postgres.ssl_mode,
                        password_configured = !app_config.postgres.password.is_empty(),
                        "Connecting to PostgreSQL…"
                    );
                    let password_configured = !app_config.postgres.password.is_empty();
                    let pg_opts = app_config.postgres.connect_options();
                    match tauri::async_runtime::block_on(async {
                        // With the session timeouts, so that neither the
                        // statements below nor the setup after them wait
                        // without end for a lock someone holds.
                        let pool = db::postgres::pool_options()
                            .connect_with(pg_opts)
                            .await?;
                        apply_postgres_migrations(&pool).await?;
                        Ok::<sqlx::PgPool, Box<dyn std::error::Error>>(pool)
                    }) {
                        Ok(pool) => {
                            // Run post-migration RLS setup (idempotent).
                            match tauri::async_runtime::block_on(setup_postgres_rls(&pool)) {
                                Ok(()) => {}
                                Err(e) if e == RLS_SETUP_NOT_OWNER => tracing::warn!(
                                    "PostgreSQL RLS setup skipped; only the role that owns the tables can apply it (ADMIN_GUIDE.md §5)"
                                ),
                                // The setup stopped part of the way, such
                                // as at a lock someone holds (it gives up
                                // on one after `lock_timeout`). For the
                                // owning role, what it had still to apply
                                // is not in effect until a start of it gets
                                // through. For any other role this is one
                                // of the two fix-ups before the ownership
                                // check, and nothing of the setup was its
                                // to apply. The error can quote what the
                                // database says, object names and the text
                                // of an exception among it, so it is logged
                                // escaped.
                                Err(e) => tracing::error!(
                                    "PostgreSQL RLS setup did not finish. If this role owns the tables, what the setup had still to apply is not in effect until a start with it gets through; see ADMIN_GUIDE.md §5, and §7 if it keeps failing: {e:?}"
                                ),
                            }
                            // Whether the setup is in the database, for
                            // the window to say where it is not: after a
                            // setup that did not finish, and for every role
                            // that the setup is not for, which works against
                            // whatever the owning role's last start left.
                            let rls_setup =
                                tauri::async_runtime::block_on(rls_setup_status(&pool));
                            tauri::async_runtime::block_on(
                                warn_of_migrations_not_of_this_build(&pool),
                            );
                            // Mappings from before a mapping had an owner
                            // (see `claim_legacy_mappings`). After the setup
                            // above, since the table owner's first start
                            // makes the policy that allows the claim, but
                            // whether or not that setup was this role's to
                            // apply.
                            if let Some(ident) = app_config
                                .user_ident
                                .as_deref()
                                .filter(|s| !s.is_empty())
                            {
                                tauri::async_runtime::block_on(
                                    claim_legacy_mappings_and_log(&pool, ident),
                                );
                            }
                            // Highlight profiles from before they were
                            // each user's own, which the shared database
                            // still holds: taken over once.
                            tauri::async_runtime::block_on(
                                take_over_central_highlights(&session_highlights, &pool),
                            );
                            // Query current_user for ownership tracking.
                            let pg_user = tauri::async_runtime::block_on(async {
                                sqlx::query_scalar::<_, String>("SELECT current_user")
                                    .fetch_one(&pool)
                                    .await
                            });
                            // Without it the app takes no item for the
                            // user's own: the window shows none as theirs to
                            // edit, and a connect goes by no session row's
                            // own credential profile (`row_defaults_apply`).
                            let pg_user = match pg_user {
                                Ok(pg_user) => Some(pg_user),
                                Err(e) => {
                                    tracing::warn!(
                                        "Could not ask PostgreSQL which role this is; no item counts as this user's own until the next start: {e:?}"
                                    );
                                    None
                                }
                            };
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
                                rls_setup: Some(rls_setup),
                            }));
                        }
                        Err(e) => {
                            // Sanitize before logging — raw errors may contain
                            // connection strings or credentials.
                            //
                            // Without a password, a missing one is the
                            // likely cause of whatever failed, and the text
                            // says to look at the configuration. A migration
                            // that gave up on a lock or on a long statement
                            // got past connecting, so it has its own text
                            // either way.
                            let raw = e.to_string();
                            let gave_up =
                                raw.contains("lock timeout") || raw.contains("statement timeout");
                            let safe_msg = if password_configured || gave_up {
                                sanitize_pg_error(&raw)
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
                                rls_setup: None,
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
            // The page the sessions belong to
            commands::page_announce,
            // Quitting, once every session is closed
            commands::app_quit,
            // Installing an update, once every session is closed
            commands::update_install_begin,
            commands::update_install_failed,
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
            // What the user confirmed of another user's session (multi-user)
            commands::session_login_confirm,
            commands::session_login_confirmed,
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
            import::inspect_securecrt,
            // Highlight profiles
            commands::highlight_profile_create,
            commands::highlight_profile_list,
            commands::highlight_profile_get,
            commands::highlight_profile_update,
            commands::highlight_profile_delete,
            commands::session_set_highlight_profile,
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
    use sqlx::postgres::PgPoolOptions;

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

    /// The commands in `run`'s `generate_handler!`, read from this file. The
    /// first occurrence is the one in `run`; the test module comes after it.
    /// It expects one command per line, `//` comments only and no `]` inside
    /// the list. Anything else makes the test below fail, not pass.
    fn registered_commands() -> std::collections::BTreeSet<String> {
        let (_, after) = include_str!("lib.rs")
            .split_once("generate_handler![")
            .unwrap();
        let (list, _) = after.split_once(']').unwrap();
        list.lines()
            .map(|line| line.split("//").next().unwrap_or_default())
            .map(|path| path.trim().trim_end_matches(','))
            .filter(|path| !path.is_empty())
            .map(|path| path.rsplit("::").next().unwrap_or(path).to_string())
            .collect()
    }

    /// Tauri checks the app's own commands against the capabilities only
    /// when the app has a manifest (`permissions/`). Without one, any local
    /// webview can call every command. With one, a command that no
    /// capability grants is refused, so a command registered in
    /// `generate_handler!` but missing from the manifest would fail at run
    /// time. This resolves what the build script wrote for
    /// `generate_context!`, as Tauri does.
    #[test]
    fn app_commands_are_granted_to_the_main_window_only() {
        use std::collections::{BTreeMap, BTreeSet};
        use tauri::utils::acl::{
            capability::Capability, manifest::Manifest, resolved::Resolved, ExecutionContext,
            APP_ACL_KEY,
        };

        let acl: BTreeMap<String, Manifest> = serde_json::from_str(include_str!(concat!(
            env!("OUT_DIR"),
            "/acl-manifests.json"
        )))
        .unwrap();
        let capabilities: BTreeMap<String, Capability> =
            serde_json::from_str(include_str!(concat!(env!("OUT_DIR"), "/capabilities.json")))
                .unwrap();
        assert!(acl.contains_key(APP_ACL_KEY), "the app has no manifest");

        let resolved = Resolved::resolve(
            &acl,
            capabilities,
            tauri::utils::platform::Target::current(),
        )
        .unwrap();
        assert!(resolved.has_app_acl);

        // A plugin's commands are listed as `plugin:<name>|<command>`.
        let granted: BTreeSet<String> = resolved
            .allowed_commands
            .keys()
            .filter(|command| !command.starts_with("plugin:"))
            .cloned()
            .collect();
        let registered = registered_commands();
        assert!(registered.contains("ssh_connect"), "{registered:?}");
        let not_granted: Vec<_> = registered.difference(&granted).collect();
        let not_registered: Vec<_> = granted.difference(&registered).collect();
        assert!(
            not_granted.is_empty() && not_registered.is_empty(),
            "missing from permissions/app-commands.toml: {not_granted:?}, \
             missing from generate_handler!: {not_registered:?}"
        );

        for command in &granted {
            for grant in &resolved.allowed_commands[command] {
                assert_eq!(grant.context, ExecutionContext::Local, "{command}");
                let windows: Vec<String> = grant.windows.iter().map(ToString::to_string).collect();
                assert_eq!(windows, ["main"], "{command}");
                assert!(grant.webviews.is_empty(), "{command}");
            }
        }
        // Nothing takes one of them away again either.
        assert!(resolved
            .denied_commands
            .keys()
            .all(|command| command.starts_with("plugin:")));
    }

    /// The rules for configured paths, which startup and saving the `SQLite`
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
        let opened = open_app_log_file(&std::env::temp_dir(), &log_config(&dir)).unwrap();
        assert_eq!(opened.dir, dir);
        assert_eq!(opened.dir_problem, None);
        assert_eq!(opened.file_note, None);
        drop(opened);
        // A chosen directory that had to be created is its owner's alone,
        // as the session logger's is, and so is the day's file in it.
        private_fs::assert_private(&dir);
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(files.len(), 1);
        private_fs::assert_private(&files[0].as_ref().unwrap().path());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A chosen directory that is already there is restricted to its owner
    /// on Unix, as the session logger restricts its own, so that others
    /// cannot list the log's files or put anything at their names.
    #[cfg(unix)]
    #[test]
    fn open_app_log_file_restricts_a_chosen_directory_already_there() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("ss-applog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let opened = open_app_log_file(&std::env::temp_dir(), &log_config(&dir)).unwrap();
        assert_eq!(opened.dir_problem, None);
        drop(opened);
        private_fs::assert_private(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A link at the name of the log directory is not followed to restrict
    /// what it leads to, since others may be able to put one there. The log
    /// is still written, and the problem is returned to be logged. That
    /// goes for a directory the user chose and for the default one.
    #[cfg(unix)]
    #[test]
    fn open_app_log_file_does_not_restrict_through_a_link() {
        use std::os::unix::fs::PermissionsExt;
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o7777;
        let root = std::env::temp_dir().join(format!("ss-applog-{}", uuid::Uuid::new_v4()));
        let elsewhere = root.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o755)).unwrap();

        let chosen = root.join("chosen");
        std::os::unix::fs::symlink(&elsewhere, &chosen).unwrap();
        let opened = open_app_log_file(&root, &log_config(&chosen)).unwrap();
        let problem = opened.dir_problem.clone().unwrap();
        assert!(problem.contains("symbolic link"), "{problem}");
        drop(opened);
        assert_eq!(mode(&elsewhere), 0o755);

        let config_dir = root.join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::os::unix::fs::symlink(&elsewhere, config_dir.join("logs")).unwrap();
        let cfg = config::AppLoggingConfig::default();
        let opened = open_app_log_file(&config_dir, &cfg).unwrap();
        let problem = opened.dir_problem.clone().unwrap();
        assert!(problem.contains("symbolic link"), "{problem}");
        drop(opened);
        assert_eq!(mode(&elsewhere), 0o755);
        // The log was written where the links lead all the same.
        assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The default directory gets the same treatment.
    #[test]
    fn open_app_log_file_creates_the_default_directory_owner_only() {
        let config_dir = std::env::temp_dir().join(format!("ss-applog-{}", uuid::Uuid::new_v4()));
        let cfg = config::AppLoggingConfig::default();
        let opened = open_app_log_file(&config_dir, &cfg).unwrap();
        let logs = config_dir.join("logs");
        assert_eq!(opened.dir, logs);
        assert_eq!(opened.dir_problem, None);
        drop(opened);
        private_fs::assert_private(&logs);
        let _ = std::fs::remove_dir_all(&config_dir);
    }

    /// A day's name that holds something the log is not appended to is
    /// passed over for the next, and the note of it is returned to be
    /// logged. `app_log` has the tests for what is passed over, and for a
    /// day with no name left.
    #[test]
    fn open_app_log_file_passes_over_a_name_that_is_taken() {
        // Today and tomorrow (UTC, as the files are named) both, so a run
        // across midnight still hits one.
        let dir = std::env::temp_dir().join(format!("ss-applog-{}", uuid::Uuid::new_v4()));
        let today = chrono::Utc::now().date_naive();
        let days = [today, today + chrono::Days::new(1)];
        for day in days {
            std::fs::create_dir_all(dir.join(format!("shellstation.{day}.log"))).unwrap();
        }
        let opened = open_app_log_file(&std::env::temp_dir(), &log_config(&dir)).unwrap();
        let note = opened.file_note.clone().unwrap();
        assert!(note.contains("could not be used"), "{note}");
        drop(opened);
        assert!(days
            .iter()
            .any(|day| dir.join(format!("shellstation.{day}_1.log")).is_file()));
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
        // The limits a migration runs under are not a connection that timed
        // out.
        let lock = sanitize_pg_error(
            "error returned from database: canceling statement due to lock timeout",
        );
        assert!(lock.contains("lock that another session holds"), "{lock}");
        let statement = sanitize_pg_error(
            "error returned from database: canceling statement due to statement timeout",
        );
        assert!(statement.contains("took longer"), "{statement}");
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

    /// The refusals of the three triggers; RLS refuses with the same code.
    const BY_OWNER_TRIGGER: &str = "only the table owner can change the owner";
    const BY_MOVE_TRIGGER: &str = "only its owner can move";
    const BY_FOLDER_TRIGGER: &str = "can only be put into one's own folders";
    const BY_RING: &str = "cannot be put into itself or below itself";

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
        with_rls_test_database(check_shared_rows_keep_their_owner).await;
    }

    /// The per-user mappings against a real server: `setup_postgres_rls`,
    /// `claim_legacy_mappings` and the statements of the mapping commands.
    /// Same server as for `shared_rows_keep_their_owner`; run with
    /// `--ignored per_user_mappings`.
    #[tokio::test]
    #[ignore = "needs a throwaway PostgreSQL server in SHELLSTATION_TEST_PG_URL"]
    async fn per_user_mappings_are_private() {
        with_rls_test_database(check_per_user_mappings_are_private).await;
    }

    /// Locks another session holds, against a real server: the app's own
    /// connections give up on them, and wait for fewer of them. Same server
    /// as for `shared_rows_keep_their_owner`; run with
    /// `--ignored held_locks`.
    #[tokio::test]
    #[ignore = "needs a throwaway PostgreSQL server in SHELLSTATION_TEST_PG_URL"]
    async fn held_locks_do_not_hang_others() {
        with_rls_test_database(check_held_locks_do_not_hang_others).await;
    }

    /// Where the trigger functions are, whose they are, and what the setup
    /// does about a trigger that calls something else, against a real
    /// server. Same server as for `shared_rows_keep_their_owner`; run with
    /// `--ignored trigger_functions`.
    #[tokio::test]
    #[ignore = "needs a throwaway PostgreSQL server in SHELLSTATION_TEST_PG_URL"]
    async fn trigger_functions_are_the_owning_roles() {
        with_rls_test_database(check_trigger_functions_are_the_owning_roles).await;
    }

    /// A session's jump host is a session of its owner's, against a real
    /// server: the trigger for it, the app's own statements, and what the
    /// foreign key does when a jump host is deleted. Same server as for
    /// `shared_rows_keep_their_owner`; run with `--ignored jump_hosts`.
    #[tokio::test]
    #[ignore = "needs a throwaway PostgreSQL server in SHELLSTATION_TEST_PG_URL"]
    async fn jump_hosts_stay_with_their_owner() {
        with_rls_test_database(check_jump_hosts_stay_with_their_owner).await;
    }

    /// Every statement that goes down the folders below a folder, against a
    /// real server: on folders that name each other as their parent it ends,
    /// and covers what is there once. Same server as for
    /// `shared_rows_keep_their_owner`; run with `--ignored folder_walks`.
    #[tokio::test]
    #[ignore = "needs a throwaway PostgreSQL server in SHELLSTATION_TEST_PG_URL"]
    async fn folder_walks_end_on_a_loop() {
        with_rls_test_database(check_folder_walks_end_on_a_loop).await;
    }

    /// Two moves of the same moment against a real server, each of a folder
    /// into one below the other, as one role from two clients: one of the
    /// two fails, so the folders are put into no ring, and afterwards the
    /// move that would make one is refused. Same server as for
    /// `shared_rows_keep_their_owner`; run with `--ignored two_moves`.
    #[tokio::test]
    #[ignore = "needs a throwaway PostgreSQL server in SHELLSTATION_TEST_PG_URL"]
    async fn two_moves_at_once_make_no_ring() {
        with_rls_test_database(check_two_moves_at_once_make_no_ring).await;
    }

    async fn check_two_moves_at_once_make_no_ring(
        opts: sqlx::postgres::PgConnectOptions,
        admin: String,
        alice: String,
        bob: String,
    ) {
        use db::models::NewFolder;
        use db::DatabaseProvider;

        let server = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone())
            .await
            .unwrap();
        prepare_rls_test_database(&server, &admin, &alice, &bob).await;
        pg_exec(&server, "RESET ROLE").await.unwrap();
        let (alice_pool, _) = app_connection_as(&opts, &alice).await;
        let app = PostgresProvider::new(alice_pool.clone());
        let folder = |name: &'static str, parent_id| {
            let app = &app;
            async move {
                app.create_folder(NewFolder {
                    name: name.to_string(),
                    parent_id,
                })
                .await
                .unwrap()
                .id
            }
        };
        let a = folder("a", None).await;
        let a1 = folder("a1", Some(a)).await;
        let b = folder("b", None).await;
        let b1 = folder("b1", Some(b)).await;

        // Alice from another client, in a transaction such as the app's
        // move: it looks above a1, finds no b there, and moves b into a1,
        // which `shellstation_own_folder` looks at the same way. It stays
        // open while the app moves a into b1, which on the tree as it was
        // before is no ring either, and locks no row the other locks.
        let other = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone())
            .await
            .unwrap();
        pg_exec(&other, &format!("SET ROLE {alice}")).await.unwrap();
        let above = "WITH RECURSIVE up AS ( \
                         SELECT f.id, f.parent_id FROM folders f WHERE f.id = $1 \
                         UNION \
                         SELECT f.id, f.parent_id FROM folders f JOIN up ON f.id = up.parent_id) \
                     SELECT EXISTS (SELECT 1 FROM up WHERE up.id = $2)";
        let mut tx = other
            .begin_with("BEGIN ISOLATION LEVEL SERIALIZABLE")
            .await
            .unwrap();
        let ring: bool = sqlx::query_scalar(above)
            .bind(a1.to_string())
            .bind(b.to_string())
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert!(!ring);
        sqlx::query("UPDATE folders SET parent_id = $1 WHERE id = $2")
            .bind(a1.to_string())
            .bind(b.to_string())
            .execute(&mut *tx)
            .await
            .unwrap();
        let moved = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            app.move_folder(a, Some(b1)),
        )
        .await
        .expect("the app's move waited for the other");
        let committed = tx.commit().await;
        other.close().await;
        // Exactly one of the two fails, and for the other: the app's move
        // after its three attempts, the other client's at its commit.
        match (&moved, &committed) {
            (Ok(()), Err(e)) => assert!(
                e.to_string().contains("could not serialize"),
                "the other client failed for another reason: {e}"
            ),
            (Err(e), Ok(())) => assert_eq!(e, db::postgres::MOVE_GIVEN_UP),
            other => panic!("one move, and only one, fails: {other:?}"),
        }

        // Each of the four folders still has the top level above it.
        for start in [a, a1, b, b1] {
            let reaches_top: bool = sqlx::query_scalar(
                "WITH RECURSIVE up AS ( \
                     SELECT f.id, f.parent_id FROM folders f WHERE f.id = $1 \
                     UNION \
                     SELECT f.id, f.parent_id FROM folders f JOIN up ON f.id = up.parent_id) \
                 SELECT EXISTS (SELECT 1 FROM up WHERE up.parent_id IS NULL)",
            )
            .bind(start.to_string())
            .fetch_one(&server)
            .await
            .unwrap();
            assert!(reaches_top, "{start} is in a ring");
        }

        // The move that would make the ring now is refused, whichever of
        // the two went through.
        let refused = if moved.is_ok() {
            app.move_folder(b, Some(a1)).await
        } else {
            app.move_folder(a, Some(b1)).await
        };
        assert_eq!(refused.unwrap_err(), db::INTO_ITSELF);

        // The owning role's moves are let through by the trigger, and
        // refused by the move itself.
        let (admin_pool, _) = app_connection_as(&opts, &admin).await;
        let admin_app = PostgresProvider::new(admin_pool.clone());
        let x = admin_app
            .create_folder(NewFolder {
                name: "x".to_string(),
                parent_id: None,
            })
            .await
            .unwrap()
            .id;
        let x1 = admin_app
            .create_folder(NewFolder {
                name: "x1".to_string(),
                parent_id: Some(x),
            })
            .await
            .unwrap()
            .id;
        assert_eq!(
            admin_app.move_folder(x, Some(x1)).await.unwrap_err(),
            db::INTO_ITSELF
        );
        admin_pool.close().await;
        alice_pool.close().await;
    }

    /// The search against a real server: a role finds its own folders and
    /// sessions and other users' shared ones, by `ILIKE` and with the query
    /// taken as written, and a statement of a search gives up at the
    /// search's own time limit, or at a shorter one that is in effect, which
    /// takes some 25 s to see. Same server as for
    /// `shared_rows_keep_their_owner`; run with `--ignored search_finds`.
    #[tokio::test]
    #[ignore = "needs a throwaway PostgreSQL server in SHELLSTATION_TEST_PG_URL"]
    async fn a_search_finds_what_the_role_can_see() {
        with_rls_test_database(check_a_search_finds_what_the_role_can_see).await;
    }

    async fn check_a_search_finds_what_the_role_can_see(
        opts: sqlx::postgres::PgConnectOptions,
        admin: String,
        alice: String,
        bob: String,
    ) {
        use db::DatabaseProvider;

        // One connection, so each SET ROLE holds for the statements after
        // it, the app's own included.
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone())
            .await
            .unwrap();
        prepare_rls_test_database(&pool, &admin, &alice, &bob).await;
        let app = PostgresProvider::new(pool.clone());

        // Folders of both users with "lab" in the name, bob's one shared and
        // one personal, and a session of each kind in bob's shared folder.
        // Made by a superuser, who may write any owner.
        let (shared, secret) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        for sql in [
            "RESET ROLE".to_string(),
            format!(
                "INSERT INTO folders (id, name, parent_id, sort_order, owner, visibility) VALUES \
                 ('{}', 'Core lab', NULL, 0, '{alice}', 'personal'), \
                 ('{shared}', 'Lab shared', NULL, 1, '{bob}', 'shared'), \
                 ('{secret}', 'lab secret', '{shared}', 0, '{bob}', 'personal'), \
                 ('{}', 'CORP\\net', NULL, 2, '{bob}', 'shared'), \
                 ('{}', '50%', NULL, 3, '{bob}', 'shared'), \
                 ('{}', 'a_b', NULL, 4, '{bob}', 'shared'), \
                 ('{}', 'axb', NULL, 5, '{bob}', 'shared')",
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4()
            ),
            format!(
                "INSERT INTO sessions (id, folder_id, name, hostname, username, owner, visibility) VALUES \
                 ('{}', '{shared}', 'lab-gw', 'host.example', '', '{bob}', 'shared'), \
                 ('{}', '{shared}', 'lab-db', 'host.example', '', '{bob}', 'personal')",
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4()
            ),
        ] {
            pg_exec(&pool, &sql).await.expect(&sql);
        }

        let folders = |query: &'static str| {
            let app = &app;
            async move {
                app.search_folders(query)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|f| f.name)
                    .collect::<Vec<_>>()
            }
        };
        let sessions = |query: &'static str| {
            let app = &app;
            async move {
                app.search_sessions(query)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|s| s.name)
                    .collect::<Vec<_>>()
            }
        };

        // Alice finds her own folder and the one bob shares, in the order
        // the folders are listed in, and not his personal one, nor his
        // personal session.
        pg_exec(&pool, &format!("SET ROLE {alice}")).await.unwrap();
        assert_eq!(folders("LAB").await, ["Core lab", "Lab shared"]);
        assert_eq!(sessions("lab").await, ["lab-gw"]);
        // The folder that was found is the row the tree has, with its owner.
        let found = app.search_folders("lab shared").await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(
            (found[0].id, found[0].parent_id, found[0].owner.as_str()),
            (shared, None, bob.as_str())
        );
        assert_eq!(found[0].visibility, "shared");
        // The backslash and the wildcards as written, and nothing for a
        // query with a NUL in it, which the server would refuse.
        assert_eq!(folders(r"corp\").await, [r"CORP\net"]);
        assert_eq!(folders("%").await, ["50%"]);
        assert_eq!(folders("a_b").await, ["a_b"]);
        assert!(folders(r"\%").await.is_empty());
        assert!(folders("lab\0").await.is_empty());
        // Not by the name of a session in it.
        assert!(folders("gw").await.is_empty());

        // Bob finds his own two, and not alice's personal one.
        pg_exec(&pool, &format!("SET ROLE {bob}")).await.unwrap();
        assert_eq!(folders("lab").await, ["lab secret", "Lab shared"]);
        assert_eq!(sessions("lab").await, ["lab-db", "lab-gw"]);

        // What the app searches by is the two together, and nothing for a
        // query with a NUL in it. The connection has no limit for a
        // statement or for a lock from here on, whatever the server sets,
        // which is not what the app's own have: those give up on a lock
        // after 5 s.
        for sql in ["SET statement_timeout = 0", "SET lock_timeout = 0"] {
            pg_exec(&pool, sql).await.expect(sql);
        }
        let search_finds = |query: &'static str| {
            let app = &app;
            async move {
                let (folders, sessions) = app.search(query).await.unwrap();
                (
                    folders.into_iter().map(|f| f.name).collect::<Vec<_>>(),
                    sessions.into_iter().map(|s| s.name).collect::<Vec<_>>(),
                )
            }
        };
        let both = (
            vec!["lab secret".to_string(), "Lab shared".to_string()],
            vec!["lab-db".to_string(), "lab-gw".to_string()],
        );
        assert_eq!(search_finds("lab").await, both);
        assert_eq!(search_finds("lab\0").await, (Vec::new(), Vec::new()));
        // The time limit of its statements ended with them.
        let limit = || async {
            sqlx::query_scalar::<_, String>("SHOW statement_timeout")
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        assert_eq!(limit().await, "0");

        // A statement of a search gives up at the search's own limit. With
        // the folders locked by another session, and no other limit in
        // effect, the search's first statement would wait without end. So
        // that a search without its limit fails this test, and does not
        // keep it waiting for a lock that is the test's own, a search is
        // given half a minute here. What comes back is how the search
        // failed and how long that took.
        let other = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        let search_meets_lock = || async {
            let mut holding = other.begin().await.unwrap();
            sqlx::query("LOCK TABLE folders IN ACCESS EXCLUSIVE MODE")
                .execute(&mut *holding)
                .await
                .unwrap();
            let started = std::time::Instant::now();
            let answer =
                tokio::time::timeout(std::time::Duration::from_secs(30), app.search("lab")).await;
            let waited = started.elapsed();
            holding.rollback().await.unwrap();
            let failed = answer
                .expect("a search that met a lock had not given up after 30 s")
                .unwrap_err();
            (failed, waited)
        };
        let seconds = std::time::Duration::from_secs;
        let (failed, waited) = search_meets_lock().await;
        assert!(
            failed.starts_with("Failed to search folders: ")
                && failed.contains("statement timeout"),
            "{failed}"
        );
        assert!(waited >= seconds(9) && waited < seconds(15), "{waited:?}");
        // The search's transaction is over: the connection has no limit
        // again, is bob's still, and the next search finds what it should.
        assert_eq!(limit().await, "0");
        assert_eq!(search_finds("lab").await, both);

        // A shorter limit that is in effect stays, for the search and after.
        pg_exec(&pool, "SET statement_timeout = '3s'")
            .await
            .unwrap();
        let (failed, waited) = search_meets_lock().await;
        assert!(failed.contains("statement timeout"), "{failed}");
        assert!(waited >= seconds(2) && waited < seconds(9), "{waited:?}");
        assert_eq!(limit().await, "3s");
        assert_eq!(search_finds("lab").await, both);

        // A longer one, which is what the app's own connections have, gives
        // way to the search's, and is back afterwards.
        pg_exec(&pool, "SET statement_timeout = '60s'")
            .await
            .unwrap();
        let (failed, waited) = search_meets_lock().await;
        assert!(failed.contains("statement timeout"), "{failed}");
        assert!(waited >= seconds(9) && waited < seconds(15), "{waited:?}");
        assert_eq!(limit().await, "1min");
        assert_eq!(search_finds("lab").await, both);

        // With a time limit for locks that is shorter, as the app's own
        // connections have one, a search that waits for a lock gives up at
        // that.
        pg_exec(&pool, "SET lock_timeout = '2s'").await.unwrap();
        let (failed, waited) = search_meets_lock().await;
        assert!(
            failed.starts_with("Failed to search folders: ") && failed.contains("lock timeout"),
            "{failed}"
        );
        assert!(waited >= seconds(1) && waited < seconds(9), "{waited:?}");
        assert_eq!(limit().await, "1min");
        assert_eq!(search_finds("lab").await, both);

        // What the search sets within its transaction, seen from inside
        // one.
        let mut tx = pool.begin().await.unwrap();
        db::postgres::limit_statement_time(&mut tx, 10_000)
            .await
            .unwrap();
        let within: String = sqlx::query_scalar("SHOW statement_timeout")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        assert_eq!(within, "10s");
        tx.commit().await.unwrap();
        assert_eq!(limit().await, "1min");
    }

    /// Where other users' rows come among a role's own after a reorder or a
    /// sort, against a real server: the role's own rows are numbered, the
    /// others' keep their owner's numbers, and the order kept locally lays
    /// the place out as asked. Same server as for
    /// `shared_rows_keep_their_owner`; run with `--ignored tree_order`.
    #[tokio::test]
    #[ignore = "needs a throwaway PostgreSQL server in SHELLSTATION_TEST_PG_URL"]
    async fn a_tree_order_holds_among_other_users_rows() {
        with_rls_test_database(check_a_tree_order_holds_among_other_users_rows).await;
    }

    async fn check_a_tree_order_holds_among_other_users_rows(
        opts: sqlx::postgres::PgConnectOptions,
        admin: String,
        alice: String,
        bob: String,
    ) {
        use db::tree_order::{Kind, TreeOrder};
        use db::DatabaseProvider;

        // One connection, so each SET ROLE holds for the statements after
        // it, the app's own included.
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone())
            .await
            .unwrap();
        prepare_rls_test_database(&pool, &admin, &alice, &bob).await;
        let app = PostgresProvider::new(pool.clone());
        let local = TreeOrder::open(
            sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .unwrap(),
        )
        .await
        .unwrap();

        // At the top level two folders of alice's and two that bob shares,
        // numbered by each owner, and in alice's first folder two sessions
        // of hers and one of bob's from before `shellstation_own_folder`.
        // Made by a superuser, who may write any owner and put a row in any
        // folder.
        let [delta, bravo, charlie, alpha] = [(); 4].map(|()| uuid::Uuid::new_v4());
        let [one, two, three] = [(); 3].map(|()| uuid::Uuid::new_v4());
        for sql in [
            "RESET ROLE".to_string(),
            format!(
                "INSERT INTO folders (id, name, parent_id, sort_order, owner, visibility) VALUES \
                 ('{delta}', 'delta', NULL, 0, '{alice}', 'personal'), \
                 ('{bravo}', 'bravo', NULL, 1, '{alice}', 'personal'), \
                 ('{charlie}', 'charlie', NULL, 0, '{bob}', 'shared'), \
                 ('{alpha}', 'alpha', NULL, 1, '{bob}', 'shared')"
            ),
            format!(
                "INSERT INTO sessions (id, folder_id, name, hostname, username, sort_order, owner, visibility) VALUES \
                 ('{one}', '{delta}', 'one', '10.0.0.2', '', 0, '{alice}', 'shared'), \
                 ('{two}', '{delta}', 'two', '10.0.0.10', '', 0, '{bob}', 'shared'), \
                 ('{three}', '{delta}', 'three', '10.0.0.1', '', 1, '{alice}', 'personal')"
            ),
            format!("SET ROLE {alice}"),
        ] {
            pg_exec(&pool, &sql).await.expect(&sql);
        }

        let top_level = || {
            let (app, local, alice) = (&app, &local, alice.as_str());
            async move {
                let mut folders = app.list_folders().await.unwrap();
                folders.retain(|f| f.parent_id.is_none());
                local.arrange_folders(&mut folders, Some(alice)).await;
                folders
                    .into_iter()
                    .map(|f| (f.name, f.sort_order))
                    .collect::<Vec<_>>()
            }
        };
        let in_delta = || {
            let (app, local, alice) = (&app, &local, alice.as_str());
            async move {
                let mut sessions = app.list_all_sessions().await.unwrap();
                sessions.retain(|s| s.folder_id == delta);
                local.arrange_sessions(&mut sessions, Some(alice)).await;
                sessions.into_iter().map(|s| s.name).collect::<Vec<_>>()
            }
        };
        let names =
            |listed: Vec<(String, i32)>| listed.into_iter().map(|(n, _)| n).collect::<Vec<_>>();

        // A reorder numbers alice's own by their place in the list and leaves
        // bob's numbers alone; the order kept puts bob's where she asked.
        let order = [alpha, bravo, charlie, delta];
        app.reorder_folders(None, order.to_vec()).await.unwrap();
        local.record(Kind::Folder, None, &order).await.unwrap();
        assert_eq!(
            top_level().await,
            [
                ("alpha".to_string(), 1),
                ("bravo".to_string(), 1),
                ("charlie".to_string(), 0),
                ("delta".to_string(), 3)
            ]
        );
        let order = [delta, charlie, alpha, bravo];
        app.reorder_folders(None, order.to_vec()).await.unwrap();
        local.record(Kind::Folder, None, &order).await.unwrap();
        assert_eq!(
            names(top_level().await),
            ["delta", "charlie", "alpha", "bravo"]
        );

        // A sort returns every folder there, bob's as well, in the order it
        // sorted them into.
        let sorted = app.sort_folders_alphabetically(None).await.unwrap();
        assert_eq!(sorted, [alpha, bravo, charlie, delta]);
        local.record(Kind::Folder, None, &sorted).await.unwrap();
        assert_eq!(
            names(top_level().await),
            ["alpha", "bravo", "charlie", "delta"]
        );

        // The same for sessions, in a folder of alice's own with one of
        // bob's in it. Bob holds the key share lock of a foreign key on
        // alice's shared session `one`, with a mapping for it in a
        // transaction he leaves open: that keeps the row from being
        // deleted, not from being numbered, and her reorder does not wait
        // for it.
        let bobs_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone())
            .await
            .unwrap();
        pg_exec(&bobs_pool, &format!("SET ROLE {bob}"))
            .await
            .unwrap();
        let mut bobs_tx = bobs_pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO session_credentials (session_id, owner, user_ident, credential_profile_id) \
             VALUES ($1, current_user, 'bob', $2)",
        )
        .bind(one.to_string())
        .bind(uuid::Uuid::new_v4().to_string())
        .execute(&mut *bobs_tx)
        .await
        .unwrap();
        let order = [three, two, one];
        let reordered = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            app.reorder_sessions(delta, order.to_vec()),
        )
        .await;
        // Bob's transaction ends before anything is judged, so that a
        // failure leaves no connection to keep the database from being
        // dropped.
        bobs_tx.rollback().await.unwrap();
        bobs_pool.close().await;
        reordered
            .expect("the reorder waited for bob's lock")
            .unwrap();
        local
            .record(Kind::Session, Some(delta), &order)
            .await
            .unwrap();
        assert_eq!(in_delta().await, ["three", "two", "one"]);
        let sorted = app.sort_sessions_by_hostname(delta).await.unwrap();
        assert_eq!(sorted, [three, one, two]);
        local
            .record(Kind::Session, Some(delta), &sorted)
            .await
            .unwrap();
        assert_eq!(in_delta().await, ["three", "one", "two"]);
        let sorted = app.sort_sessions_alphabetically(delta).await.unwrap();
        assert_eq!(sorted, [one, three, two]);
        local
            .record(Kind::Session, Some(delta), &sorted)
            .await
            .unwrap();
        assert_eq!(in_delta().await, ["one", "three", "two"]);

        // Bob's numbers are as he left them.
        let bobs: Vec<(String, i32)> =
            sqlx::query_as("SELECT name, sort_order FROM folders WHERE owner = $1 ORDER BY name")
                .bind(&bob)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(bobs, [("alpha".to_string(), 1), ("charlie".to_string(), 0)]);
        let two_sort: i32 = sqlx::query_scalar("SELECT sort_order FROM sessions WHERE id = $1")
            .bind(two.to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(two_sort, 0);
    }

    /// `check_postgres_rls` against a real server: it finds nothing missing
    /// after the setup, as a regular user, and names each part that is taken
    /// away or is not the setup's. Same server as for
    /// `shared_rows_keep_their_owner`; run with `--ignored rls_setup_check`.
    #[tokio::test]
    #[ignore = "needs a throwaway PostgreSQL server in SHELLSTATION_TEST_PG_URL"]
    async fn rls_setup_check_finds_what_is_missing() {
        with_rls_test_database(check_rls_setup_check_finds_what_is_missing).await;
    }

    /// The highlight profiles of the shared database, from before they were
    /// each user's own, against a real server: what a user's first start
    /// takes over, and that nothing written there afterwards is. Same server
    /// as for `shared_rows_keep_their_owner`; run with
    /// `--ignored central_highlight_profiles`.
    #[tokio::test]
    #[ignore = "needs a throwaway PostgreSQL server in SHELLSTATION_TEST_PG_URL"]
    async fn central_highlight_profiles_are_taken_over_once() {
        with_rls_test_database(check_central_highlight_profiles_are_taken_over_once).await;
    }

    async fn check_central_highlight_profiles_are_taken_over_once(
        opts: sqlx::postgres::PgConnectOptions,
        admin: String,
        alice: String,
        bob: String,
    ) {
        use db::DatabaseProvider;
        use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

        const RULES: &str =
            r##"[{"pattern":"error","color":"#ff0000","case_sensitive":false,"bold":true}]"##;

        // A local database as PostgreSQL mode has it, for a first start.
        async fn local_database() -> (SessionHighlights, SqliteProvider) {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    SqliteConnectOptions::new()
                        .filename(":memory:")
                        .pragma("foreign_keys", "OFF"),
                )
                .await
                .unwrap();
            sqlx::migrate!().run(&pool).await.unwrap();
            let highlights = SessionHighlights::open(pool.clone()).await.unwrap();
            (highlights, SqliteProvider::new(pool))
        }

        // One connection, so each SET ROLE holds for the statements after it.
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await
            .unwrap();
        for sql in [
            format!("GRANT ALL PRIVILEGES ON DATABASE {database} TO {admin}"),
            format!("GRANT ALL ON SCHEMA public TO {admin}"),
            format!("GRANT USAGE ON SCHEMA public TO {alice}, {bob}"),
            format!("SET ROLE {admin}"),
        ] {
            pg_exec(&pool, &sql).await.unwrap();
        }
        sqlx::migrate!().run(&pool).await.unwrap();

        // A database as the version before left it: profiles that are
        // nobody's, and session rows that name them. One profile is longer
        // than the app saves, and one is named by no session.
        let [cisco, linux, unused, oversized] = [(); 4].map(|()| uuid::Uuid::new_v4());
        let long_rules = format!("[{}]", " ".repeat(1_048_576));
        for (id, name, rules, order) in [
            (cisco, "Cisco", RULES, 0),
            (linux, "Linux", "[]", 1),
            (unused, "Unused", "[]", 2),
            (oversized, "Oversized", long_rules.as_str(), 3),
        ] {
            sqlx::query(
                "INSERT INTO highlight_profiles (id, name, rules, sort_order) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(id.to_string())
            .bind(name)
            .bind(rules)
            .bind(order)
            .execute(&pool)
            .await
            .unwrap();
        }
        let [of_alice, of_bob] = [(); 2].map(|()| uuid::Uuid::new_v4());
        for (folder, owner) in [(of_alice, &alice), (of_bob, &bob)] {
            pg_exec(
                &pool,
                &format!(
                    "INSERT INTO folders (id, name, sort_order, owner, visibility) \
                     VALUES ('{folder}', 'folder', 0, '{owner}', 'shared')"
                ),
            )
            .await
            .unwrap();
        }
        let [shared, personal, own, oversized_session] = [(); 4].map(|()| uuid::Uuid::new_v4());
        for (id, folder, owner, visibility, profile) in [
            (shared, of_alice, &alice, "shared", cisco),
            (personal, of_alice, &alice, "personal", linux),
            (own, of_bob, &bob, "personal", linux),
            (oversized_session, of_bob, &bob, "personal", oversized),
        ] {
            pg_exec(
                &pool,
                &format!(
                    "INSERT INTO sessions \
                         (id, folder_id, name, hostname, username, owner, visibility, \
                          highlight_profile_id) \
                     VALUES ('{id}', '{folder}', 'session', 'host.example', '', '{owner}', \
                             '{visibility}', '{profile}')"
                ),
            )
            .await
            .unwrap();
        }
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

        let (highlights, local) = local_database().await;

        // Bob's first start. Every profile the app could have saved comes
        // over, in the order the app showed them, and the one that was left
        // where it is is counted. A choice comes over for his own session
        // only: not for alice's shared one, where the profile was her
        // choice, nor for the session whose profile was not read.
        pg_exec(&pool, &format!("SET ROLE {bob}")).await.unwrap();
        let taken = highlights.take_over_central(&pool).await.unwrap().unwrap();
        assert_eq!(
            (
                taken.copied,
                taken.already_there,
                taken.left_out,
                taken.sessions,
                taken.not_read
            ),
            (3, 0, 0, 1, 1)
        );
        let profiles = local.list_highlight_profiles().await.unwrap();
        assert_eq!(
            profiles
                .iter()
                .map(|p| (p.id, p.name.as_str(), p.rules.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (cisco, "Cisco", RULES),
                (linux, "Linux", "[]"),
                (unused, "Unused", "[]")
            ]
        );
        let chosen = highlights.all().await.unwrap();
        assert_eq!(chosen, std::collections::HashMap::from([(own, linux)]));

        // The shared table is still every role's to write. Alice changes a
        // profile bob has, deletes another, adds one and has her shared
        // session name it. And she puts sixteen profiles of the longest
        // rules before all the others, more than a take-over reads.
        pg_exec(&pool, &format!("SET ROLE {alice}")).await.unwrap();
        let planted = uuid::Uuid::new_v4();
        for sql in [
            format!(
                "UPDATE highlight_profiles SET rules = '[{{\"pattern\":\"(a|a)*$\",\
                 \"color\":\"#000000\",\"case_sensitive\":true,\"bold\":false}}]' \
                 WHERE id = '{cisco}'"
            ),
            format!("DELETE FROM highlight_profiles WHERE id = '{unused}'"),
            format!(
                "INSERT INTO highlight_profiles (id, name, rules, sort_order) \
                 VALUES ('{planted}', 'Planted', '[]', 9)"
            ),
            format!("UPDATE sessions SET highlight_profile_id = '{planted}' WHERE id = '{shared}'"),
        ] {
            assert_eq!(
                pg_exec(&pool, &sql).await.unwrap().rows_affected(),
                1,
                "{sql}"
            );
        }
        let longest = format!("[{}]", " ".repeat(1_048_576 - 2));
        for n in 0..16 {
            sqlx::query(
                "INSERT INTO highlight_profiles (id, name, rules, sort_order) \
                 VALUES ($1, $2, $3, -1)",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            // Each with a name of its own: the same name and rules twice
            // would be taken once.
            .bind(format!("Filler {n}"))
            .bind(&longest)
            .execute(&pool)
            .await
            .unwrap();
        }

        // None of it reaches bob: not at his next start, and not through
        // what the app reads while it runs.
        pg_exec(&pool, &format!("SET ROLE {bob}")).await.unwrap();
        assert_eq!(highlights.take_over_central(&pool).await.unwrap(), None);
        // The time limit of the take-over's statements ended with them.
        let limit: String = sqlx::query_scalar("SHOW statement_timeout")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(limit, "0");
        let after = local.list_highlight_profiles().await.unwrap();
        assert_eq!(
            after
                .iter()
                .map(|p| (p.id, p.rules.as_str()))
                .collect::<Vec<_>>(),
            vec![(cisco, RULES), (linux, "[]"), (unused, "[]")]
        );
        assert_eq!(highlights.all().await.unwrap(), chosen);
        let provider = PostgresProvider::new(pool.clone());
        let mut sessions = provider.list_all_sessions().await.unwrap();
        // The row names what alice wrote; what bob's window gets is his own.
        let row = |sessions: &[db::models::Session], id| {
            sessions
                .iter()
                .find(|s| s.id == id)
                .unwrap()
                .highlight_profile_id
        };
        assert_eq!(row(&sessions, shared), Some(planted));
        highlights.overlay(&mut sessions).await;
        assert_eq!(row(&sessions, shared), None);
        assert_eq!(row(&sessions, own), Some(linux));
        assert_eq!(row(&sessions, oversized_session), None);

        // A first start of bob's on another machine, against the table as
        // alice left it. The profile his own session names is read before
        // anything else, so her sixteen cannot keep it out: fifteen of them
        // fit beside it, and what comes after them in the app's order is
        // not read. The rules read come to no more than the limit.
        let (elsewhere, elsewhere_local) = local_database().await;
        let taken = elsewhere.take_over_central(&pool).await.unwrap().unwrap();
        assert_eq!(
            (taken.copied, taken.left_out, taken.sessions, taken.not_read),
            // In the table: the sixteen, Cisco, Linux, Planted and Oversized.
            (16, 0, 1, 4)
        );
        let read = elsewhere_local.list_highlight_profiles().await.unwrap();
        assert!(read.iter().any(|p| p.id == linux));
        assert_eq!(
            read.iter().filter(|p| p.name.starts_with("Filler")).count(),
            15
        );
        assert!(read.iter().map(|p| p.rules.len()).sum::<usize>() <= 16 * 1_048_576);
        assert_eq!(
            elsewhere.all().await.unwrap(),
            std::collections::HashMap::from([(own, linux)])
        );
    }

    /// Run `checks` with a database of its own on the throwaway server and
    /// the names of three roles made for it: an admin and two users. All
    /// four are dropped again afterwards.
    async fn with_rls_test_database<F, Fut>(checks: F)
    where
        F: FnOnce(sqlx::postgres::PgConnectOptions, String, String, String) -> Fut,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
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
        let checks = tokio::spawn(checks(
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
        prepare_rls_test_database(&pool, &admin, &alice, &bob).await;
        let role = |role: &str| format!("SET ROLE {role}");
        let affected = |sql: String| {
            let pool = pool.clone();
            async move { pg_exec(&pool, &sql).await.unwrap().rows_affected() }
        };
        let new_session = |folder: uuid::Uuid, visibility: &str| {
            let id = uuid::Uuid::new_v4();
            let sql = format!(
                "INSERT INTO sessions (id, folder_id, name, hostname, username, owner, visibility) \
                 VALUES ('{id}', '{folder}', 'session', 'host.example', '', current_user, \
                         '{visibility}')"
            );
            (id, sql)
        };

        pg_exec(&pool, &role(&alice)).await.unwrap();
        let folder = insert_folder(&pool, None, "shared").await;
        let subfolder = insert_folder(&pool, Some(folder), "shared").await;
        let (session, sql) = new_session(folder, "shared");
        pg_exec(&pool, &sql).await.unwrap();
        let take_folder = format!("UPDATE folders SET owner = current_user WHERE id = '{folder}'");
        let take_session =
            format!("UPDATE sessions SET owner = current_user WHERE id = '{session}'");
        let set_host =
            |host: &str| format!("UPDATE sessions SET hostname = '{host}' WHERE id = '{session}'");
        let host = format!("SELECT count(*) FROM sessions WHERE id = '{session}' AND hostname = ");
        let rename = format!("UPDATE folders SET name = 'renamed' WHERE id = '{folder}'");
        let delete = format!("DELETE FROM folders WHERE id = '{folder}'");
        let obsolete = "SELECT count(*) FROM pg_policies \
                        WHERE policyname IN ('folders_shared_update', 'sessions_shared_update')";

        // A database whose owning role has not started this version yet: any
        // role can still update a shared row. The setup has just dropped the
        // two policies for that, so they are put back as they were.
        pg_exec(&pool, &role(&admin)).await.unwrap();
        assert_eq!(pg_count(&pool, obsolete).await, 0);
        for table in ["folders", "sessions"] {
            let sql = format!(
                "CREATE POLICY {table}_shared_update ON {table} FOR UPDATE \
                 USING (visibility = 'shared')"
            );
            pg_exec(&pool, &sql).await.unwrap();
        }

        // There the triggers are what keeps a shared row with its owner and
        // where its owner put it.
        pg_exec(&pool, &role(&bob)).await.unwrap();
        assert!(refused(
            pg_exec(&pool, &take_folder).await,
            BY_OWNER_TRIGGER
        ));
        assert!(refused(
            pg_exec(&pool, &take_session).await,
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
        pg_exec(&pool, "DROP TABLE pg_class").await.unwrap();
        assert_eq!(affected(delete.clone()).await, 0);

        // Moved under a folder of bob's, alice's rows would be hidden from
        // everyone else and deleted with it: the foreign keys cascade, and a
        // cascade is not subject to RLS.
        let own = insert_folder(&pool, None, "personal").await;
        let move_session =
            format!("UPDATE sessions SET folder_id = '{own}' WHERE id = '{session}'");
        let move_folder =
            format!("UPDATE folders SET parent_id = '{own}' WHERE id = '{subfolder}'");
        assert!(refused(
            pg_exec(&pool, &move_session).await,
            BY_MOVE_TRIGGER
        ));
        assert!(refused(pg_exec(&pool, &move_folder).await, BY_MOVE_TRIGGER));

        // But everything else a shared row says was anyone's to rewrite,
        // such as where a session connects to. This is what the policies
        // were dropped for.
        assert_eq!(affected(rename.clone()).await, 1);
        assert_eq!(affected(set_host("elsewhere.example")).await, 1);
        assert_eq!(
            pg_count(&pool, &format!("{host}'elsewhere.example'")).await,
            1
        );

        // The owning role starts this version, and the two policies go.
        pg_exec(&pool, &role(&admin)).await.unwrap();
        assert_eq!(pg_count(&pool, obsolete).await, 2);
        setup_postgres_rls(&pool).await.unwrap();
        assert_eq!(pg_count(&pool, obsolete).await, 0);

        // Now a shared row is its owner's to change and no one else's. To
        // bob it is a row he can read: his updates find nothing to update,
        // whatever they set, and he cannot lock it.
        pg_exec(&pool, &role(&bob)).await.unwrap();
        for sql in [
            set_host("again.example"),
            rename.clone(),
            take_folder.clone(),
            take_session.clone(),
            move_session.clone(),
            move_folder.clone(),
            format!("UPDATE sessions SET visibility = 'personal' WHERE id = '{session}'"),
            format!("UPDATE sessions SET legacy_algorithms = 1 WHERE id = '{session}'"),
            delete.clone(),
        ] {
            assert_eq!(affected(sql.clone()).await, 0, "{sql}");
        }
        assert_eq!(
            pg_count(&pool, &format!("{host}'elsewhere.example'")).await,
            1
        );
        for (lock, rows) in [
            ("", 1),
            ("FOR UPDATE", 0),
            ("FOR SHARE", 0),
            ("FOR KEY SHARE", 0),
        ] {
            let sql = format!("SELECT id FROM sessions WHERE id = '{session}' {lock}");
            let found = sqlx::query(sqlx::AssertSqlSafe(sql))
                .fetch_all(&pool)
                .await
                .unwrap();
            assert_eq!(found.len(), rows, "{lock}");
        }

        // Nor can he put something of his own into a folder of alice's,
        // where it would show among hers: not a new session or folder, and
        // not one of his that he moves there. In a folder of his own, and at
        // the top level, he can.
        let (in_own, sql) = new_session(own, "shared");
        pg_exec(&pool, &sql).await.unwrap();
        let below_own = insert_folder(&pool, Some(own), "shared").await;
        let folder_into = |parent: uuid::Uuid| {
            format!(
                "INSERT INTO folders (id, name, parent_id, sort_order, owner, visibility) \
                 VALUES ('{}', 'folder', '{parent}', 0, current_user, 'shared')",
                uuid::Uuid::new_v4()
            )
        };
        for sql in [
            new_session(folder, "shared").1,
            new_session(folder, "personal").1,
            folder_into(folder),
            format!("UPDATE sessions SET folder_id = '{folder}' WHERE id = '{in_own}'"),
            format!("UPDATE folders SET parent_id = '{subfolder}' WHERE id = '{below_own}'"),
        ] {
            assert!(
                refused(pg_exec(&pool, &sql).await, BY_FOLDER_TRIGGER),
                "{sql}"
            );
        }
        // Nor into a folder of his own that is itself in a folder of
        // alice's, as one may be from before this was refused, or put there
        // by a superuser, as here. One is in her shared folder, one in a
        // personal folder of hers that he cannot even see.
        pg_exec(&pool, "RESET ROLE").await.unwrap();
        let (hers, planted, hidden) = (
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        );
        for (id, parent, owner, visibility) in [
            (hers, None, &alice, "personal"),
            (planted, Some(subfolder), &bob, "shared"),
            (hidden, Some(hers), &bob, "shared"),
        ] {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO folders (id, name, parent_id, sort_order, owner, visibility) \
                 VALUES ('{id}', 'planted', $1, 0, '{owner}', '{visibility}')"
            )))
            .bind(parent.map(|parent: uuid::Uuid| parent.to_string()))
            .execute(&pool)
            .await
            .unwrap();
        }
        pg_exec(&pool, &role(&bob)).await.unwrap();
        for target in [planted, hidden] {
            for sql in [new_session(target, "shared").1, folder_into(target)] {
                assert!(
                    refused(pg_exec(&pool, &sql).await, BY_FOLDER_TRIGGER),
                    "{sql}"
                );
            }
        }
        // What he has there is still his to change where it is, and to take
        // out of it, after which it is a folder like any other of his.
        let keep_place = format!(
            "UPDATE folders SET parent_id = parent_id, name = 'his' WHERE id = '{planted}'"
        );
        assert_eq!(affected(keep_place).await, 1);
        let take_out = format!("UPDATE folders SET parent_id = NULL WHERE id = '{planted}'");
        assert_eq!(affected(take_out).await, 1);
        pg_exec(&pool, &new_session(planted, "shared").1)
            .await
            .unwrap();

        // A folder does not go into itself, nor into one below it: the look
        // up the tree comes to an end in a ring, and refuses to make one.
        let top = insert_folder(&pool, None, "personal").await;
        let below = insert_folder(&pool, Some(top), "personal").await;
        for (folder, parent) in [(top, top), (top, below)] {
            let sql = format!("UPDATE folders SET parent_id = '{parent}' WHERE id = '{folder}'");
            assert!(refused(pg_exec(&pool, &sql).await, BY_RING), "{sql}");
        }

        // A ring that is there already, as one may be from before this was
        // refused, and as a superuser makes here: the look up the tree still
        // comes to an end in it, and what is in it can be taken out.
        pg_exec(&pool, "RESET ROLE").await.unwrap();
        let ring = format!("UPDATE folders SET parent_id = '{below}' WHERE id = '{top}'");
        assert_eq!(affected(ring).await, 1);
        pg_exec(&pool, &role(&bob)).await.unwrap();
        pg_exec(&pool, &new_session(below, "personal").1)
            .await
            .unwrap();
        let out = format!("UPDATE folders SET parent_id = NULL WHERE id = '{top}'");
        assert_eq!(affected(out).await, 1);

        // An update that leaves the folder as it is, is not a move.
        let stay = format!(
            "UPDATE sessions SET folder_id = folder_id, name = 'mine' WHERE id = '{in_own}'"
        );
        assert_eq!(affected(stay).await, 1);
        let to_top = format!("UPDATE folders SET parent_id = NULL WHERE id = '{below_own}'");
        assert_eq!(affected(to_top).await, 1);

        // Deleting his folder deletes what is his, and nothing of alice's.
        assert_eq!(
            affected(format!("DELETE FROM folders WHERE id = '{own}'")).await,
            1
        );
        let left = format!("SELECT count(*) FROM sessions WHERE id = '{session}'");
        assert_eq!(pg_count(&pool, &left).await, 1);

        // The owner still changes her row, and moves it between folders of
        // her own. She cannot hand it to someone else, nor put it into a
        // folder of someone else's.
        pg_exec(&pool, &role(&alice)).await.unwrap();
        assert_eq!(affected(set_host("host.example")).await, 1);
        assert_eq!(affected(rename.clone()).await, 1);
        assert!(refused(
            pg_exec(
                &pool,
                &format!("UPDATE folders SET owner = '{bob}' WHERE id = '{folder}'"),
            )
            .await,
            BY_OWNER_TRIGGER
        ));
        let other = insert_folder(&pool, None, "personal").await;
        let move_own = format!("UPDATE sessions SET folder_id = '{other}' WHERE id = '{session}'");
        assert_eq!(affected(move_own).await, 1);
        let to_bob =
            format!("UPDATE sessions SET folder_id = '{below_own}' WHERE id = '{session}'");
        assert!(refused(pg_exec(&pool, &to_bob).await, BY_FOLDER_TRIGGER));

        // The admin owns the tables, and row-level security is forced for it
        // too: it reaches no row of alice's. Reassigning rows takes a
        // superuser (admin guide §4), who also puts a row where it likes.
        let reassign = format!("UPDATE folders SET owner = '{admin}' WHERE id = '{folder}'");
        pg_exec(&pool, &role(&admin)).await.unwrap();
        assert_eq!(affected(reassign.clone()).await, 0);
        pg_exec(&pool, "RESET ROLE").await.unwrap();
        assert_eq!(affected(reassign).await, 1);
        let anywhere =
            format!("UPDATE sessions SET folder_id = '{below_own}' WHERE id = '{session}'");
        assert_eq!(affected(anywhere).await, 1);

        pool.close().await;
    }

    /// A database as the admin guide sets it up under Option A, through a
    /// pool of one connection, which is left as the admin: the admin migrates
    /// and applies the RLS setup, twice to show it is idempotent, and the two
    /// users get DML.
    async fn prepare_rls_test_database(pool: &sqlx::PgPool, admin: &str, alice: &str, bob: &str) {
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(pool)
            .await
            .unwrap();
        for sql in [
            format!("GRANT ALL PRIVILEGES ON DATABASE {database} TO {admin}"),
            format!("GRANT ALL ON SCHEMA public TO {admin}"),
            format!("GRANT USAGE ON SCHEMA public TO {alice}, {bob}"),
            format!("SET ROLE {admin}"),
        ] {
            pg_exec(pool, &sql).await.unwrap();
        }
        sqlx::migrate!().run(pool).await.unwrap();
        setup_postgres_rls(pool).await.unwrap();
        setup_postgres_rls(pool).await.unwrap();
        for sql in [
            format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public \
                 TO {alice}, {bob}"
            ),
            format!("REVOKE INSERT, UPDATE, DELETE ON _sqlx_migrations FROM {alice}, {bob}"),
        ] {
            pg_exec(pool, &sql).await.unwrap();
        }
    }

    /// One connection as `role`, made the way the app makes its own, and
    /// with what it reports for each of the session timeouts.
    async fn app_connection_as(
        opts: &sqlx::postgres::PgConnectOptions,
        role: &str,
    ) -> (sqlx::PgPool, Vec<String>) {
        let pool = db::postgres::pool_options()
            .max_connections(1)
            .connect_with(opts.clone())
            .await
            .unwrap();
        pg_exec(&pool, &format!("SET ROLE {role}")).await.unwrap();
        let mut timeouts = Vec::new();
        for name in [
            "lock_timeout",
            "statement_timeout",
            "idle_in_transaction_session_timeout",
        ] {
            let shown: String = sqlx::query_scalar("SELECT current_setting($1)")
                .bind(name)
                .fetch_one(&pool)
                .await
                .unwrap();
            timeouts.push(shown);
        }
        (pool, timeouts)
    }

    /// What `PostgreSQL` says when a statement gives up on a lock.
    const BY_LOCK_TIMEOUT: &str = "lock timeout";

    /// Whether `result` failed at a lock, and soon: within a second of the
    /// 300 ms the test sets, far from the 5 s of the app, let alone no end.
    fn gave_up_on_lock<T: std::fmt::Debug>(
        result: &Result<T, String>,
        started: std::time::Instant,
    ) -> bool {
        matches!(result, Err(e) if e.contains(BY_LOCK_TIMEOUT))
            && started.elapsed() < std::time::Duration::from_secs(2)
    }

    async fn check_held_locks_do_not_hang_others(
        opts: sqlx::postgres::PgConnectOptions,
        admin: String,
        alice: String,
        bob: String,
    ) {
        use db::models::{NewFolder, NewSession};
        use db::DatabaseProvider;

        let server = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone())
            .await
            .unwrap();
        prepare_rls_test_database(&server, &admin, &alice, &bob).await;
        pg_exec(&server, "RESET ROLE").await.unwrap();

        // The app's connections set the three timeouts.
        let (alice_pool, timeouts) = app_connection_as(&opts, &alice).await;
        assert_eq!(timeouts, ["5s", "1min", "30s"]);
        let (admin_pool, _) = app_connection_as(&opts, &admin).await;
        // Shorter from here on, so that the test does not take the 5 s for
        // each lock it runs into.
        for pool in [&alice_pool, &admin_pool] {
            pg_exec(pool, "SET lock_timeout = '300ms'").await.unwrap();
        }
        let alice_db = PostgresProvider::new(alice_pool.clone());
        let new_folder = |name: &str| NewFolder {
            name: name.to_string(),
            parent_id: None,
        };

        // Alice and bob each have a shared folder at the top level.
        let of_alice = alice_db
            .create_folder(new_folder("alice"))
            .await
            .unwrap()
            .id;
        pg_exec(
            &alice_pool,
            &format!("UPDATE folders SET visibility = 'shared' WHERE id = '{of_alice}'"),
        )
        .await
        .unwrap();
        // Bob is not the app: a session with its own statements and no
        // timeouts, as from any other client.
        let bob_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone())
            .await
            .unwrap();
        pg_exec(&bob_pool, &format!("SET ROLE {bob}"))
            .await
            .unwrap();
        insert_folder(&bob_pool, None, "shared").await;
        // And a shared session of his in alice's folder. Neither the app nor
        // a statement of his own puts one there; a superuser can, and one
        // may be left from before the triggers refused it.
        pg_exec(
            &server,
            &format!(
                "INSERT INTO sessions (id, folder_id, name, hostname, username, owner, visibility) \
                 VALUES ('{}', '{of_alice}', 'guest', 'host.example', '', '{bob}', 'shared')",
                uuid::Uuid::new_v4()
            ),
        )
        .await
        .unwrap();
        let new_session = |name: &str, folder_id| NewSession {
            folder_id,
            name: name.to_string(),
            hostname: "host.example".to_string(),
            port: 22,
            protocol: "ssh".to_string(),
            username: String::new(),
            auth_method: "password".to_string(),
            jump_host_id: None,
            tags: "[]".to_string(),
            icon: "desktop".to_string(),
            highlight_profile_id: None,
            credential_profile_id: None,
            legacy_algorithms: false,
            login_sequence_id: None,
        };

        // Bob holds a row lock on his own shared folder and on his session.
        // The folder is a sibling of every top-level folder, and the session
        // one of every session in alice's folder, but neither is one alice
        // has to wait for to make or move something of her own.
        let mut held = bob_pool.begin().await.unwrap();
        for table in ["folders", "sessions"] {
            let sql = format!("SELECT id FROM {table} WHERE owner = current_user FOR UPDATE");
            let locked = sqlx::query(sqlx::AssertSqlSafe(sql))
                .fetch_all(&mut *held)
                .await
                .unwrap();
            assert_eq!(locked.len(), 1, "{table}");
        }
        let own = alice_db.create_folder(new_folder("own")).await.unwrap().id;
        alice_db.move_folder(own, Some(of_alice)).await.unwrap();
        alice_db.move_folder(own, None).await.unwrap();
        let session = alice_db
            .create_session(new_session("mine", own))
            .await
            .unwrap()
            .id;
        alice_db.move_session(session, of_alice).await.unwrap();
        alice_db
            .create_session(new_session("another", of_alice))
            .await
            .unwrap();

        // A row lock on everything he can lock is no more than that: only
        // its owner can lock a shared row, so alice's shared folder is not
        // his to hold, and she changes it while he waits.
        let locked = sqlx::query("SELECT id FROM folders FOR UPDATE")
            .fetch_all(&mut *held)
            .await
            .unwrap();
        assert_eq!(locked.len(), 1);
        alice_db.rename_folder(of_alice, "renamed").await.unwrap();
        // The owning role's start finds its setup in place and changes
        // nothing, so it does not wait for bob either. An `ALTER TABLE`
        // would: it wants the whole table, and with it waiting, so would
        // every reader after it.
        setup_postgres_rls(&admin_pool).await.unwrap();
        assert_eq!(alice_db.list_folders().await.unwrap().len(), 3);

        // What he can hold against her is the table. Locked against
        // writers, her change gives up instead of waiting for him. Her
        // connection is as good as before, and reading is no writing.
        sqlx::query("LOCK TABLE folders IN SHARE ROW EXCLUSIVE MODE")
            .execute(&mut *held)
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let renamed = alice_db.rename_folder(of_alice, "again").await;
        assert!(gave_up_on_lock(&renamed, started), "{renamed:?}");
        assert_eq!(alice_db.list_folders().await.unwrap().len(), 3);
        held.rollback().await.unwrap();
        alice_db.rename_folder(of_alice, "again").await.unwrap();

        // Locked against everything, which takes no more than the
        // privileges the app needs either, nothing gets through, but
        // everything gives up: a read, and the start of the owning role,
        // which fails at its first statement.
        let mut held = bob_pool.begin().await.unwrap();
        sqlx::query("LOCK TABLE folders IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *held)
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let listed = alice_db.list_folders().await;
        assert!(gave_up_on_lock(&listed, started), "{listed:?}");
        let started = std::time::Instant::now();
        let set_up = setup_postgres_rls(&admin_pool).await;
        assert!(gave_up_on_lock(&set_up, started), "{set_up:?}");
        held.rollback().await.unwrap();
        assert_eq!(alice_db.list_folders().await.unwrap().len(), 3);
        setup_postgres_rls(&admin_pool).await.unwrap();

        // A table gets its policies and has row-level security switched on
        // in one step, which puts back whichever of the two is missing.
        let applied = "SELECT (SELECT count(*) FROM pg_policies WHERE tablename = 'folders' \
                               AND policyname = 'folders_shared_read') \
                            + (SELECT count(*) FROM pg_class WHERE oid = 'folders'::regclass \
                               AND relrowsecurity AND relforcerowsecurity)";
        let policy_on_sessions = "SELECT count(*) FROM pg_policies WHERE tablename = 'sessions' \
                                  AND policyname = 'sessions_shared_read'";
        for missing in [
            "DROP POLICY folders_shared_read ON folders",
            "ALTER TABLE folders NO FORCE ROW LEVEL SECURITY",
            "ALTER TABLE folders DISABLE ROW LEVEL SECURITY",
        ] {
            pg_exec(&admin_pool, missing).await.unwrap();
            assert_eq!(pg_count(&server, applied).await, 1, "{missing}");
            setup_postgres_rls(&admin_pool).await.unwrap();
            assert_eq!(pg_count(&server, applied).await, 2, "{missing}");
        }

        // Here both are missing on `folders`, and a policy on `sessions` as
        // well, and bob holds a row lock on a folder when the owning role
        // starts. The setup gives up on `folders`, which is as it was and
        // not half protected, and still goes on to `sessions`.
        for sql in [
            "DROP POLICY folders_shared_read ON folders",
            "ALTER TABLE folders NO FORCE ROW LEVEL SECURITY",
            "DROP POLICY sessions_shared_read ON sessions",
        ] {
            pg_exec(&admin_pool, sql).await.unwrap();
        }
        let mut held = bob_pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM folders WHERE owner = current_user FOR UPDATE")
            .fetch_all(&mut *held)
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let set_up = setup_postgres_rls(&admin_pool).await;
        assert!(gave_up_on_lock(&set_up, started), "{set_up:?}");
        assert_eq!(pg_count(&server, applied).await, 0);
        assert_eq!(pg_count(&server, policy_on_sessions).await, 1);
        held.rollback().await.unwrap();
        setup_postgres_rls(&admin_pool).await.unwrap();
        assert_eq!(pg_count(&server, applied).await, 2);

        // A step that cannot be applied does not keep the others from the
        // database: a row breaks the constraint that is missing, and a
        // trigger is missing as well. The setup reports the constraint and
        // still puts the trigger back.
        for sql in [
            "ALTER TABLE folders DROP CONSTRAINT folders_vis_ck",
            "DROP TRIGGER folders_keep_place ON folders",
        ] {
            pg_exec(&admin_pool, sql).await.unwrap();
        }
        let set_visibility = |visibility: &str| {
            format!("UPDATE folders SET visibility = '{visibility}' WHERE id = '{own}'")
        };
        pg_exec(&server, &set_visibility("odd")).await.unwrap();
        let failure = setup_postgres_rls(&admin_pool).await.unwrap_err();
        assert!(failure.contains("constraint folders_vis_ck"), "{failure}");
        assert!(!failure.contains(BY_LOCK_TIMEOUT), "{failure}");
        let in_place = "SELECT (SELECT count(*) FROM pg_trigger WHERE tgname = 'folders_keep_place') \
                             + (SELECT count(*) FROM pg_constraint WHERE conname = 'folders_vis_ck')";
        assert_eq!(pg_count(&server, in_place).await, 1);
        pg_exec(&server, &set_visibility("personal")).await.unwrap();
        setup_postgres_rls(&admin_pool).await.unwrap();
        assert_eq!(pg_count(&server, in_place).await, 2);

        // The setup is for whoever `ALTER TABLE` would take for the owner:
        // a member of the owning role that inherits its privileges, but not
        // one that does not. Both are made here and dropped again.
        let suffix = admin.trim_start_matches("ss_admin_");
        let (heir, guest) = (format!("ss_heir_{suffix}"), format!("ss_guest_{suffix}"));
        for sql in [
            format!("CREATE ROLE {heir} NOLOGIN INHERIT"),
            format!("CREATE ROLE {guest} NOLOGIN NOINHERIT"),
            format!("GRANT {admin} TO {heir}, {guest}"),
            // What a regular user has, which the one that inherits nothing
            // needs for the fix-up before the check.
            format!("GRANT USAGE ON SCHEMA public TO {guest}"),
            format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO {guest}"
            ),
            format!("SET ROLE {heir}"),
        ] {
            pg_exec(&server, &sql).await.unwrap();
        }
        setup_postgres_rls(&server).await.unwrap();
        pg_exec(&server, &format!("SET ROLE {guest}"))
            .await
            .unwrap();
        assert_eq!(
            setup_postgres_rls(&server).await,
            Err(RLS_SETUP_NOT_OWNER.to_string())
        );
        for sql in [
            "RESET ROLE".to_string(),
            format!("DROP OWNED BY {guest}"),
            format!("DROP ROLE {heir}, {guest}"),
        ] {
            pg_exec(&server, &sql).await.unwrap();
        }

        // A timeout the server sets is kept, and the others are still set.
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&server)
            .await
            .unwrap();
        pg_exec(
            &server,
            &format!("ALTER DATABASE {database} SET lock_timeout = '2s'"),
        )
        .await
        .unwrap();
        let (kept_pool, timeouts) = app_connection_as(&opts, &alice).await;
        assert_eq!(timeouts, ["2s", "1min", "30s"]);

        for pool in [server, alice_pool, admin_pool, bob_pool, kept_pool] {
            pool.close().await;
        }
    }

    async fn check_trigger_functions_are_the_owning_roles(
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
        prepare_rls_test_database(&pool, &admin, &alice, &bob).await;
        let role = |role: &str| format!("SET ROLE {role}");
        let run = |sql: String| {
            let pool = pool.clone();
            async move { pg_exec(&pool, &sql).await.unwrap() }
        };

        // The schema and the four functions in it are the owning role's,
        // and the seven triggers call them and are enabled.
        let schema_owner = format!(
            "SELECT count(*) FROM pg_namespace n JOIN pg_roles r ON r.oid = n.nspowner \
             WHERE n.nspname = '{RLS_SCHEMA}' AND r.rolname = '{admin}'"
        );
        let functions_owner = format!(
            "SELECT count(*) FROM pg_proc f JOIN pg_roles r ON r.oid = f.proowner \
             WHERE f.pronamespace = '{RLS_SCHEMA}'::regnamespace AND r.rolname = '{admin}'"
        );
        let triggers = format!(
            "SELECT count(*) FROM pg_trigger t JOIN pg_proc f ON f.oid = t.tgfoid \
             WHERE NOT t.tgisinternal AND t.tgenabled = 'O' \
               AND f.pronamespace = '{RLS_SCHEMA}'::regnamespace \
               AND t.tgrelid IN ('folders'::regclass, 'sessions'::regclass)"
        );
        let in_public = "SELECT count(*) FROM pg_proc \
                         WHERE pronamespace = 'public'::regnamespace \
                           AND proname LIKE 'shellstation\\_%'";
        assert_eq!(pg_count(&pool, &schema_owner).await, 1);
        assert_eq!(pg_count(&pool, &functions_owner).await, 4);
        assert_eq!(pg_count(&pool, &triggers).await, 7);
        assert_eq!(pg_count(&pool, in_public).await, 0);

        // A start that finds the triggers as they should be leaves them: a
        // trigger that is replaced is a new one, with a new oid, and
        // replacing takes the table's lock.
        let trigger_oids = "SELECT string_agg(oid::text, ',' ORDER BY oid) FROM pg_trigger \
                            WHERE NOT tgisinternal";
        let oids = |pool: sqlx::PgPool| async move {
            sqlx::query_scalar::<_, String>(trigger_oids)
                .fetch_one(&pool)
                .await
                .unwrap()
        };
        let before = oids(pool.clone()).await;
        setup_postgres_rls(&pool).await.unwrap();
        assert_eq!(oids(pool.clone()).await, before);

        // A database of an earlier version: the function is in `public`, and
        // two triggers call it. The setup has them call its own, and drops
        // the old one once neither calls it any more.
        let does_nothing = |name: &str| {
            format!(
                "CREATE FUNCTION public.{name}() RETURNS trigger LANGUAGE plpgsql \
                 AS 'BEGIN RETURN NEW; END'"
            )
        };
        run(does_nothing("shellstation_keep_owner")).await;
        for table in ["folders", "sessions"] {
            run(format!("DROP TRIGGER {table}_keep_owner ON {table}")).await;
            run(format!(
                "CREATE TRIGGER {table}_keep_owner BEFORE UPDATE OF owner ON {table} \
                 FOR EACH ROW WHEN (OLD.owner IS DISTINCT FROM NEW.owner) \
                 EXECUTE FUNCTION public.shellstation_keep_owner()"
            ))
            .await;
        }
        assert_eq!(pg_count(&pool, &triggers).await, 5);
        setup_postgres_rls(&pool).await.unwrap();
        assert_eq!(pg_count(&pool, &triggers).await, 7);
        assert_eq!(pg_count(&pool, in_public).await, 0);

        // While something else still calls the old function, it stays.
        run(does_nothing("shellstation_keep_place")).await;
        run("DROP TRIGGER folders_keep_place ON folders".to_string()).await;
        for (name, table, column) in [
            ("folders_keep_place", "folders", "parent_id"),
            ("another", "sessions", "name"),
        ] {
            run(format!(
                "CREATE TRIGGER {name} BEFORE UPDATE OF {column} ON {table} \
                 FOR EACH ROW WHEN (OLD.{column} IS DISTINCT FROM NEW.{column}) \
                 EXECUTE FUNCTION public.shellstation_keep_place()"
            ))
            .await;
        }
        setup_postgres_rls(&pool).await.unwrap();
        assert_eq!(pg_count(&pool, &triggers).await, 7);
        assert_eq!(pg_count(&pool, in_public).await, 1);
        run("DROP TRIGGER another ON sessions".to_string()).await;
        run("DROP FUNCTION public.shellstation_keep_place()".to_string()).await;

        // A trigger that calls the right function for the wrong column is
        // replaced too.
        let right_column = "SELECT count(*) FROM pg_trigger t JOIN pg_attribute a \
                                ON a.attrelid = t.tgrelid AND a.attnum::text = t.tgattr::text \
                            WHERE t.tgname = 'sessions_keep_place' AND a.attname = 'folder_id'";
        run("DROP TRIGGER sessions_keep_place ON sessions".to_string()).await;
        run(format!(
            "CREATE TRIGGER sessions_keep_place BEFORE UPDATE OF name ON sessions \
             FOR EACH ROW WHEN (OLD.name IS DISTINCT FROM NEW.name) \
             EXECUTE FUNCTION {RLS_SCHEMA}.shellstation_keep_place()"
        ))
        .await;
        assert_eq!(pg_count(&pool, right_column).await, 0);
        setup_postgres_rls(&pool).await.unwrap();
        assert_eq!(pg_count(&pool, right_column).await, 1);

        // So is one with the wrong argument, which tells the function which
        // column to look at, and one that has lost its condition.
        let argument = "SELECT count(*) FROM pg_trigger \
                        WHERE tgname = 'sessions_own_folder' \
                          AND encode(tgargs, 'escape') = E'folder_id\\\\000'";
        let condition = "SELECT count(*) FROM pg_trigger \
                         WHERE tgname = 'folders_keep_owner' AND tgqual IS NOT NULL";
        run("DROP TRIGGER sessions_own_folder ON sessions".to_string()).await;
        run(format!(
            "CREATE TRIGGER sessions_own_folder BEFORE INSERT OR UPDATE OF folder_id \
             ON sessions FOR EACH ROW \
             EXECUTE FUNCTION {RLS_SCHEMA}.shellstation_own_folder('jump_host_id')"
        ))
        .await;
        run("DROP TRIGGER folders_keep_owner ON folders".to_string()).await;
        run(format!(
            "CREATE TRIGGER folders_keep_owner BEFORE UPDATE OF owner ON folders \
             FOR EACH ROW EXECUTE FUNCTION {RLS_SCHEMA}.shellstation_keep_owner()"
        ))
        .await;
        assert_eq!(pg_count(&pool, argument).await, 0);
        assert_eq!(pg_count(&pool, condition).await, 0);
        setup_postgres_rls(&pool).await.unwrap();
        assert_eq!(pg_count(&pool, argument).await, 1);
        assert_eq!(pg_count(&pool, condition).await, 1);

        // A trigger that is switched off is put back as well.
        run("ALTER TABLE folders DISABLE TRIGGER folders_keep_place".to_string()).await;
        assert_eq!(pg_count(&pool, &triggers).await, 6);
        setup_postgres_rls(&pool).await.unwrap();
        assert_eq!(pg_count(&pool, &triggers).await, 7);

        // The jump host's trigger is held to a shape of its own: for new
        // rows as well as updates of its column, with a condition and
        // without an argument. One that calls the right function and misses
        // any one of these is replaced.
        let as_it_should_be = "SELECT count(*) FROM pg_trigger t JOIN pg_attribute a \
                 ON a.attrelid = t.tgrelid AND a.attnum::text = t.tgattr::text \
             WHERE t.tgname = 'sessions_own_jump_host' AND a.attname = 'jump_host_id' \
               AND t.tgtype = 23 AND t.tgnargs = 0 AND t.tgqual IS NOT NULL";
        assert_eq!(pg_count(&pool, as_it_should_be).await, 1);
        let named = "WHEN (NEW.jump_host_id IS NOT NULL)";
        for (when_and_for, condition, argument) in [
            ("BEFORE UPDATE OF jump_host_id", named, ""),
            ("BEFORE INSERT OR UPDATE OF folder_id", named, ""),
            ("BEFORE INSERT OR UPDATE OF jump_host_id", "", ""),
            (
                "BEFORE INSERT OR UPDATE OF jump_host_id",
                named,
                "'jump_host_id'",
            ),
        ] {
            run("DROP TRIGGER sessions_own_jump_host ON sessions".to_string()).await;
            run(format!(
                "CREATE TRIGGER sessions_own_jump_host {when_and_for} ON sessions \
                 FOR EACH ROW {condition} \
                 EXECUTE FUNCTION {RLS_SCHEMA}.shellstation_own_jump_host({argument})"
            ))
            .await;
            assert_eq!(pg_count(&pool, as_it_should_be).await, 0, "{when_and_for}");
            setup_postgres_rls(&pool).await.unwrap();
            assert_eq!(pg_count(&pool, as_it_should_be).await, 1, "{when_and_for}");
        }
        assert_eq!(pg_count(&pool, &triggers).await, 7);

        // Bob may create functions in `public`, as every role could before
        // PostgreSQL 15, and triggers on `sessions`, as under Option B. He
        // gets there first: a function under the setup's name that lets
        // everything through, and the trigger of the setup's name calling
        // it, on a database that does not have that trigger yet.
        pg_exec(&pool, &role(&alice)).await.unwrap();
        let of_alice = insert_folder(&pool, None, "shared").await;
        pg_exec(&pool, "RESET ROLE").await.unwrap();
        run(format!("GRANT CREATE ON SCHEMA public TO {bob}")).await;
        run(format!("GRANT TRIGGER ON sessions TO {bob}")).await;
        run("DROP TRIGGER sessions_own_folder ON sessions".to_string()).await;
        pg_exec(&pool, &role(&bob)).await.unwrap();
        run(does_nothing("shellstation_own_folder")).await;
        run(
            "CREATE TRIGGER sessions_own_folder BEFORE INSERT OR UPDATE OF folder_id \
             ON sessions FOR EACH ROW \
             EXECUTE FUNCTION public.shellstation_own_folder('folder_id')"
                .to_string(),
        )
        .await;
        let into_alices = || {
            format!(
                "INSERT INTO sessions (id, folder_id, name, hostname, username, owner, visibility) \
                 VALUES ('{}', '{of_alice}', 'planted', 'host.example', '', current_user, 'shared')",
                uuid::Uuid::new_v4()
            )
        };
        assert_eq!(run(into_alices()).await.rows_affected(), 1);
        // In the owning role's schema he can create nothing.
        let in_schema = format!(
            "CREATE FUNCTION {RLS_SCHEMA}.shellstation_own_folder() RETURNS trigger \
             LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END'"
        );
        assert!(refused(
            pg_exec(&pool, &in_schema).await,
            "permission denied for schema"
        ));
        // Nor can a trigger of his call a function that is there, which is
        // why a trigger that does call one is taken for the owning role's.
        let calls_it = format!(
            "CREATE TRIGGER his BEFORE UPDATE ON sessions FOR EACH ROW \
             EXECUTE FUNCTION {RLS_SCHEMA}.shellstation_keep_owner()"
        );
        assert!(refused(
            pg_exec(&pool, &calls_it).await,
            "permission denied for schema"
        ));

        // The owning role's start replaces his trigger by one that calls its
        // own function. His function stays, with nothing calling it.
        pg_exec(&pool, &role(&admin)).await.unwrap();
        assert_eq!(pg_count(&pool, &triggers).await, 6);
        setup_postgres_rls(&pool).await.unwrap();
        assert_eq!(pg_count(&pool, &triggers).await, 7);
        assert_eq!(pg_count(&pool, in_public).await, 1);
        pg_exec(&pool, &role(&bob)).await.unwrap();
        assert!(refused(
            pg_exec(&pool, &into_alices()).await,
            BY_FOLDER_TRIGGER
        ));

        // A member of the owning role that inherits its privileges can apply
        // the setup. The schema and the functions it creates are the owning
        // role's all the same, not the member's.
        pg_exec(&pool, "RESET ROLE").await.unwrap();
        let heir = format!("ss_heir_{}", admin.trim_start_matches("ss_admin_"));
        run(format!("CREATE ROLE {heir} NOLOGIN INHERIT")).await;
        run(format!("GRANT {admin} TO {heir}")).await;
        run(format!("DROP SCHEMA {RLS_SCHEMA} CASCADE")).await;
        let any_trigger = "SELECT count(*) FROM pg_trigger WHERE NOT tgisinternal";
        assert_eq!(pg_count(&pool, any_trigger).await, 0);
        pg_exec(&pool, &role(&heir)).await.unwrap();
        setup_postgres_rls(&pool).await.unwrap();
        assert_eq!(pg_count(&pool, &schema_owner).await, 1);
        assert_eq!(pg_count(&pool, &functions_owner).await, 4);
        assert_eq!(pg_count(&pool, &triggers).await, 7);
        pg_exec(&pool, "RESET ROLE").await.unwrap();
        run(format!("DROP ROLE {heir}")).await;

        // A schema of the name that is someone else's is not used: the
        // owning role's setup says so and leaves functions and triggers as
        // they are. A superuser's start gives the schema to the owning role.
        run(format!("ALTER SCHEMA {RLS_SCHEMA} OWNER TO {bob}")).await;
        pg_exec(&pool, &role(&admin)).await.unwrap();
        let failure = setup_postgres_rls(&pool).await.unwrap_err();
        assert!(
            failure.contains(&format!("schema {RLS_SCHEMA}")),
            "{failure}"
        );
        assert!(!failure.contains("function "), "{failure}");
        assert_eq!(pg_count(&pool, &schema_owner).await, 0);
        assert_eq!(pg_count(&pool, &triggers).await, 7);
        pg_exec(&pool, "RESET ROLE").await.unwrap();
        setup_postgres_rls(&pool).await.unwrap();
        assert_eq!(pg_count(&pool, &schema_owner).await, 1);
        assert_eq!(pg_count(&pool, &functions_owner).await, 4);
        assert_eq!(pg_count(&pool, &triggers).await, 7);

        // What anyone else was granted on the schema is taken back.
        let granted = format!(
            "SELECT count(*) FROM pg_namespace n, aclexplode(n.nspacl) a \
             WHERE n.nspname = '{RLS_SCHEMA}' AND a.grantee <> n.nspowner"
        );
        run(format!(
            "GRANT USAGE, CREATE ON SCHEMA {RLS_SCHEMA} TO PUBLIC, {bob}"
        ))
        .await;
        assert_eq!(pg_count(&pool, &granted).await, 4);
        pg_exec(&pool, &role(&admin)).await.unwrap();
        setup_postgres_rls(&pool).await.unwrap();
        assert_eq!(pg_count(&pool, &granted).await, 0);

        // Bob puts operators into `public` for what the setup compares: an
        // oid with the name of a table, a function or a schema, and with a
        // number. An operator whose types match exactly is found before one
        // in `pg_catalog` that takes a cast, and would run as the owning
        // role. These fail whatever calls them. The setup has something to
        // do on every kind of step, and gets through: it compares oids with
        // oids.
        pg_exec(&pool, &role(&bob)).await.unwrap();
        for (name, right) in [
            ("class", "regclass"),
            ("procedure", "regprocedure"),
            ("namespace", "regnamespace"),
            ("number", "integer"),
        ] {
            run(format!(
                "CREATE FUNCTION public.planted_{name}(oid, {right}) RETURNS boolean \
                 LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''planted operator''; END'"
            ))
            .await;
            for operator in ["=", "<>"] {
                run(format!(
                    "CREATE OPERATOR public.{operator} (LEFTARG = oid, RIGHTARG = {right}, \
                     FUNCTION = public.planted_{name})"
                ))
                .await;
            }
        }
        pg_exec(&pool, &role(&admin)).await.unwrap();
        for sql in [
            "DROP TRIGGER folders_keep_place ON folders",
            "DROP POLICY folders_shared_read ON folders",
            "ALTER TABLE folders DROP CONSTRAINT folders_vis_ck",
            "ALTER TABLE sessions NO FORCE ROW LEVEL SECURITY",
        ] {
            pg_exec(&pool, sql).await.unwrap();
        }
        pg_exec(&pool, "RESET ROLE").await.unwrap();
        run(format!("GRANT USAGE ON SCHEMA {RLS_SCHEMA} TO PUBLIC")).await;
        pg_exec(&pool, &role(&admin)).await.unwrap();
        setup_postgres_rls(&pool).await.unwrap();
        pg_exec(&pool, "RESET ROLE").await.unwrap();
        for name in ["class", "procedure", "namespace", "number"] {
            run(format!("DROP FUNCTION public.planted_{name} CASCADE")).await;
        }
        assert_eq!(pg_count(&pool, &triggers).await, 7);
        assert_eq!(pg_count(&pool, &granted).await, 0);
        let restored = "SELECT (SELECT count(*) FROM pg_policies \
                                WHERE policyname = 'folders_shared_read') \
                             + (SELECT count(*) FROM pg_constraint WHERE conname = 'folders_vis_ck') \
                             + (SELECT count(*) FROM pg_class WHERE oid = 'sessions'::regclass \
                                AND relforcerowsecurity)";
        assert_eq!(pg_count(&pool, restored).await, 3);

        pool.close().await;
    }

    /// The refusal of the trigger for jump hosts.
    const BY_JUMP_HOST_TRIGGER: &str = "can only name one of one's own sessions as its jump host";

    async fn check_jump_hosts_stay_with_their_owner(
        opts: sqlx::postgres::PgConnectOptions,
        admin: String,
        alice: String,
        bob: String,
    ) {
        use db::models::{NewSession, UpdateSession};
        use db::{BulkSessionEdit, DatabaseProvider};

        // One connection, so each SET ROLE holds for the statements after
        // it, the app's own included.
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        prepare_rls_test_database(&pool, &admin, &alice, &bob).await;
        let app = PostgresProvider::new(pool.clone());
        let role = |role: &str| format!("SET ROLE {role}");
        let affected = |sql: String| {
            let pool = pool.clone();
            async move { pg_exec(&pool, &sql).await.unwrap().rows_affected() }
        };
        let new_session = |folder: uuid::Uuid, visibility: &str, jump: Option<uuid::Uuid>| {
            let id = uuid::Uuid::new_v4();
            let jump = jump.map_or_else(|| "NULL".to_string(), |jump| format!("'{jump}'"));
            let sql = format!(
                "INSERT INTO sessions \
                     (id, folder_id, name, hostname, username, jump_host_id, owner, visibility) \
                 VALUES ('{id}', '{folder}', 'session', 'host.example', '', {jump}, \
                         current_user, '{visibility}')"
            );
            (id, sql)
        };
        let set_jump = |session: uuid::Uuid, jump: uuid::Uuid| {
            format!("UPDATE sessions SET jump_host_id = '{jump}' WHERE id = '{session}'")
        };
        let jump_of = |session: uuid::Uuid| {
            let pool = pool.clone();
            async move {
                let sql = format!("SELECT jump_host_id FROM sessions WHERE id = '{session}'");
                sqlx::query_scalar::<_, Option<String>>(sqlx::AssertSqlSafe(sql))
                    .fetch_one(&pool)
                    .await
                    .unwrap()
            }
        };

        // Bob has two bastions, one shared and one that only he sees.
        pg_exec(&pool, &role(&bob)).await.unwrap();
        let of_bob = insert_folder(&pool, None, "shared").await;
        let (his_shared, sql) = new_session(of_bob, "shared", None);
        pg_exec(&pool, &sql).await.unwrap();
        let (his_personal, sql) = new_session(of_bob, "personal", None);
        pg_exec(&pool, &sql).await.unwrap();

        // Alice names a bastion of her own as a jump host, for a new session
        // and for one she has.
        pg_exec(&pool, &role(&alice)).await.unwrap();
        let of_alice = insert_folder(&pool, None, "shared").await;
        let (hers, sql) = new_session(of_alice, "personal", None);
        pg_exec(&pool, &sql).await.unwrap();
        let (target, sql) = new_session(of_alice, "shared", Some(hers));
        pg_exec(&pool, &sql).await.unwrap();
        let (other, sql) = new_session(of_alice, "shared", None);
        pg_exec(&pool, &sql).await.unwrap();
        assert_eq!(affected(set_jump(other, hers)).await, 1);

        // She names none of bob's: not the one he shares, and not the one
        // she cannot see, for which the foreign key alone would have said
        // that it exists. A session that is not there gets the same answer,
        // from the trigger and not from the foreign key.
        let nowhere = uuid::Uuid::new_v4();
        for jump in [his_shared, his_personal, nowhere] {
            for sql in [
                new_session(of_alice, "shared", Some(jump)).1,
                new_session(of_alice, "personal", Some(jump)).1,
                set_jump(target, jump),
            ] {
                assert!(
                    refused(pg_exec(&pool, &sql).await, BY_JUMP_HOST_TRIGGER),
                    "{sql}"
                );
            }
        }
        assert_eq!(jump_of(target).await, Some(hers.to_string()));
        // Taking a jump host away is always hers to do, and so is naming
        // another of her own in place of the one a session has.
        let clear = format!("UPDATE sessions SET jump_host_id = NULL WHERE id = '{other}'");
        assert_eq!(affected(clear).await, 1);
        let (hers_too, sql) = new_session(of_alice, "personal", None);
        pg_exec(&pool, &sql).await.unwrap();
        assert_eq!(affected(set_jump(target, hers_too)).await, 1);
        assert_eq!(jump_of(target).await, Some(hers_too.to_string()));
        assert_eq!(affected(set_jump(target, hers)).await, 1);

        // The app's own statements say so themselves, before the trigger
        // does, and take a jump host of hers.
        let new_in_app = |jump: uuid::Uuid| NewSession {
            folder_id: of_alice,
            name: "from the app".to_string(),
            hostname: "host.example".to_string(),
            port: 22,
            protocol: "ssh".to_string(),
            username: String::new(),
            auth_method: "profile".to_string(),
            jump_host_id: Some(jump),
            tags: "[]".to_string(),
            icon: "desktop".to_string(),
            highlight_profile_id: None,
            credential_profile_id: None,
            legacy_algorithms: false,
            login_sequence_id: None,
        };
        let jump_in_app = |jump: uuid::Uuid| UpdateSession {
            jump_host_id: Some(Some(jump)),
            ..UpdateSession::default()
        };
        let bulk_in_app = |jump: uuid::Uuid| BulkSessionEdit {
            jump_host_id: Some(Some(jump)),
            ..BulkSessionEdit::default()
        };
        let by_app = db::postgres::NOT_OWN_JUMP_HOST;
        for jump in [his_shared, his_personal, nowhere] {
            assert_eq!(
                app.create_session(new_in_app(jump)).await.unwrap_err(),
                by_app
            );
            assert_eq!(
                app.update_session(other, jump_in_app(jump))
                    .await
                    .unwrap_err(),
                by_app
            );
            assert_eq!(
                app.bulk_edit_sessions(of_alice, bulk_in_app(jump))
                    .await
                    .unwrap_err(),
                by_app
            );
        }
        assert_eq!(jump_of(other).await, None);
        let created = app.create_session(new_in_app(hers)).await.unwrap().id;
        assert_eq!(jump_of(created).await, Some(hers.to_string()));
        app.update_session(other, jump_in_app(hers)).await.unwrap();
        assert_eq!(jump_of(other).await, Some(hers.to_string()));

        // A session that names a jump host of bob's from before this was
        // refused, as a superuser makes one here: alice still changes it,
        // with her own statement and with the app's, which sends the jump
        // host along with every change. She cannot name another of his.
        pg_exec(&pool, "RESET ROLE").await.unwrap();
        assert_eq!(affected(set_jump(target, his_shared)).await, 1);
        pg_exec(&pool, &role(&alice)).await.unwrap();
        let keep = format!(
            "UPDATE sessions SET jump_host_id = jump_host_id, name = 'renamed' \
             WHERE id = '{target}'"
        );
        assert_eq!(affected(keep).await, 1);
        app.update_session(
            target,
            UpdateSession {
                hostname: Some("other.example".to_string()),
                ..jump_in_app(his_shared)
            },
        )
        .await
        .unwrap();
        assert_eq!(jump_of(target).await, Some(his_shared.to_string()));
        assert_eq!(
            app.update_session(target, jump_in_app(his_personal))
                .await
                .unwrap_err(),
            by_app
        );
        assert!(refused(
            pg_exec(&pool, &set_jump(target, his_personal)).await,
            BY_JUMP_HOST_TRIGGER
        ));

        // Bob deletes that bastion, and the foreign key takes it from her
        // session, which the trigger does not stand in the way of. This is
        // what a jump host of another user's meant: he decides that her
        // session has none.
        pg_exec(&pool, &role(&bob)).await.unwrap();
        let delete = format!("DELETE FROM sessions WHERE id = '{his_shared}'");
        assert_eq!(affected(delete).await, 1);
        assert_eq!(jump_of(target).await, None);

        // Her own bastion she deletes herself, with the same outcome for the
        // sessions behind it, which are hers.
        pg_exec(&pool, &role(&alice)).await.unwrap();
        assert_eq!(
            affected(format!("DELETE FROM sessions WHERE id = '{hers}'")).await,
            1
        );
        assert_eq!(jump_of(other).await, None);
        assert_eq!(jump_of(created).await, None);

        // A database whose owning role has not started this version yet has
        // no such trigger: a statement of alice's own names bob's bastion,
        // and the app's statements still refuse to.
        pg_exec(&pool, &role(&admin)).await.unwrap();
        pg_exec(&pool, "DROP TRIGGER sessions_own_jump_host ON sessions")
            .await
            .unwrap();
        pg_exec(&pool, &role(&alice)).await.unwrap();
        assert_eq!(affected(set_jump(other, his_personal)).await, 1);
        assert_eq!(
            app.update_session(target, jump_in_app(his_personal))
                .await
                .unwrap_err(),
            by_app
        );
        assert_eq!(
            app.create_session(new_in_app(his_personal))
                .await
                .unwrap_err(),
            by_app
        );
        assert_eq!(
            app.bulk_edit_sessions(of_alice, bulk_in_app(his_personal))
                .await
                .unwrap_err(),
            by_app
        );
        assert_eq!(jump_of(target).await, None);

        // The owning role's start puts the trigger back.
        pg_exec(&pool, &role(&admin)).await.unwrap();
        setup_postgres_rls(&pool).await.unwrap();
        pg_exec(&pool, &role(&alice)).await.unwrap();
        assert!(refused(
            pg_exec(&pool, &set_jump(target, his_personal)).await,
            BY_JUMP_HOST_TRIGGER
        ));

        // The table owner gets past the function, as past the others, for
        // rows of its own, which are all that row-level security leaves it.
        // It names bob's bastion for a new row and for one it has; alice's
        // session is not its to update.
        pg_exec(&pool, &role(&admin)).await.unwrap();
        let of_admin = insert_folder(&pool, None, "personal").await;
        let (_, sql) = new_session(of_admin, "personal", Some(his_personal));
        pg_exec(&pool, &sql).await.unwrap();
        let (its_own, sql) = new_session(of_admin, "personal", None);
        pg_exec(&pool, &sql).await.unwrap();
        assert_eq!(affected(set_jump(its_own, his_personal)).await, 1);
        assert_eq!(jump_of(its_own).await, Some(his_personal.to_string()));
        assert_eq!(affected(set_jump(target, his_personal)).await, 0);

        pool.close().await;
    }

    /// What row-level security says when it refuses a new row.
    const BY_ROW_SECURITY: &str = "row-level security policy";

    async fn pg_count(pool: &sqlx::PgPool, sql: &str) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn check_per_user_mappings_are_private(
        opts: sqlx::postgres::PgConnectOptions,
        admin: String,
        alice: String,
        bob: String,
    ) {
        use commands::{
            bulk_set_user_credentials, bulk_set_user_login_sequences, query_user_credential,
            query_user_login_sequence, set_user_credential, set_user_login_sequence,
        };

        // One connection, so each SET ROLE holds for the statements after it.
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await
            .unwrap();
        for sql in [
            format!("GRANT ALL PRIVILEGES ON DATABASE {database} TO {admin}"),
            format!("GRANT ALL ON SCHEMA public TO {admin}"),
            format!("GRANT USAGE ON SCHEMA public TO {alice}, {bob}"),
            format!("SET ROLE {admin}"),
        ] {
            pg_exec(&pool, &sql).await.unwrap();
        }
        sqlx::migrate!().run(&pool).await.unwrap();

        // A database from before a mapping had an owner, written while row-
        // level security is not yet on: alice has a shared and a personal
        // session, and there are mappings under her name for both and under
        // bob's for the shared one. Written without `owner`, as the previous
        // version wrote them.
        let (shared, personal) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        let folder = uuid::Uuid::new_v4();
        pg_exec(
            &pool,
            &format!(
                "INSERT INTO folders (id, name, sort_order, owner, visibility) \
                 VALUES ('{folder}', 'folder', 0, '{alice}', 'shared')"
            ),
        )
        .await
        .unwrap();
        for (id, visibility) in [(shared, "shared"), (personal, "personal")] {
            pg_exec(
                &pool,
                &format!(
                    "INSERT INTO sessions (id, folder_id, name, hostname, username, owner, visibility) \
                     VALUES ('{id}', '{folder}', '{visibility}', 'host.example', '', '{alice}', '{visibility}')"
                ),
            )
            .await
            .unwrap();
        }
        let tables = [
            ("session_credentials", "credential_profile_id"),
            ("session_login_sequences", "login_sequence_id"),
        ];
        assert_eq!(tables.map(|(table, _)| table), MAPPING_TABLES);
        let (of_alice, of_bob) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        for (table, column) in tables {
            pg_exec(
                &pool,
                &format!(
                    "INSERT INTO {table} (session_id, user_ident, {column}) VALUES \
                     ('{shared}', 'alice', '{of_alice}'), ('{personal}', 'alice', '{of_alice}'), \
                     ('{shared}', 'bob', '{of_bob}')"
                ),
            )
            .await
            .unwrap();
        }

        // RLS setup, twice to show it is idempotent, then DML for the users
        // (admin guide, Option A).
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
        for (table, _) in tables {
            // The owner is in the primary key now, and nothing else is one.
            assert_eq!(
                pg_count(
                    &pool,
                    &format!(
                        "SELECT count(*) FROM pg_constraint WHERE conrelid = '{table}'::regclass \
                         AND contype = 'p' AND conname = '{table}_owner_key'"
                    ),
                )
                .await,
                1
            );
            assert_eq!(
                pg_count(
                    &pool,
                    &format!(
                        "SELECT count(*) FROM pg_constraint WHERE conrelid = '{table}'::regclass \
                         AND contype IN ('p', 'u')"
                    ),
                )
                .await,
                1
            );
            // FORCE: the table owner sees the placeholders of the session it
            // can see, and no more.
            assert_eq!(
                pg_count(&pool, &format!("SELECT count(*) FROM {table}")).await,
                2
            );
        }
        let (shared, personal) = (shared.to_string(), personal.to_string());

        // Bob, before anyone has claimed anything. The setup is not his to
        // apply, as at every start of a regular user.
        pg_exec(&pool, &format!("SET ROLE {bob}")).await.unwrap();
        assert_eq!(
            setup_postgres_rls(&pool).await,
            Err(RLS_SETUP_NOT_OWNER.to_string())
        );
        for (table, column) in tables {
            // The placeholders of the shared session can be read, as before;
            // the one of alice's personal session cannot.
            assert_eq!(
                pg_count(&pool, &format!("SELECT count(*) FROM {table}")).await,
                2
            );
            // A placeholder cannot be rewritten in place, and none added.
            let mine = uuid::Uuid::new_v4();
            for sql in [
                format!("UPDATE {table} SET {column} = '{mine}' WHERE user_ident = 'alice'"),
                format!(
                    "INSERT INTO {table} (session_id, user_ident, {column}) \
                     VALUES ('{shared}', 'carol', '{mine}')"
                ),
                // Nor a row for another role.
                format!(
                    "INSERT INTO {table} (session_id, owner, user_ident, {column}) \
                     VALUES ('{shared}', '{alice}', 'alice', '{mine}')"
                ),
                // Nor a row of his own for a session he cannot see, or for
                // none: both are refused alike, so the answer does not say
                // which ids are sessions.
                format!(
                    "INSERT INTO {table} (session_id, owner, user_ident, {column}) \
                     VALUES ('{personal}', current_user, 'bob', '{mine}')"
                ),
                format!(
                    "INSERT INTO {table} (session_id, owner, user_ident, {column}) \
                     VALUES ('{mine}', current_user, 'bob', '{mine}')"
                ),
            ] {
                assert!(
                    refused(pg_exec(&pool, &sql).await, BY_ROW_SECURITY),
                    "{sql}"
                );
            }
        }
        // The commands refuse the same, and say the same for both.
        let unseen = [personal.clone(), uuid::Uuid::new_v4().to_string()];
        for session in &unseen {
            let expected = Err(format!("Session {session} not found"));
            assert_eq!(
                set_user_credential(&pool, session, "bob", Some(&shared)).await,
                expected
            );
            assert_eq!(
                set_user_login_sequence(&pool, session, "bob", Some(&shared)).await,
                expected
            );
        }
        // Nothing connects with a placeholder.
        assert_eq!(
            query_user_credential(&pool, &shared, "bob").await.unwrap(),
            None
        );
        assert_eq!(
            query_user_login_sequence(&pool, &shared, "bob")
                .await
                .unwrap(),
            None
        );
        // Bob claims his own, one in each table, and a second claim finds
        // nothing left.
        assert_eq!(claim_legacy_mappings(&pool, "bob").await.unwrap(), 2);
        assert_eq!(claim_legacy_mappings(&pool, "bob").await.unwrap(), 0);
        assert_eq!(
            query_user_credential(&pool, &shared, "bob").await.unwrap(),
            Some(of_bob)
        );
        assert_eq!(
            query_user_login_sequence(&pool, &shared, "bob")
                .await
                .unwrap(),
            Some(of_bob)
        );
        // Bob maps the shared session under alice's name as well. With the
        // old primary key this row would have kept alice from mapping it.
        let planted = uuid::Uuid::new_v4();
        let planted_text = planted.to_string();
        set_user_credential(&pool, &shared, "alice", Some(&planted_text))
            .await
            .unwrap();
        set_user_login_sequence(&pool, &shared, "alice", Some(&planted_text))
            .await
            .unwrap();
        // Setting it again updates the row instead of adding one.
        set_user_credential(&pool, &shared, "alice", Some(&planted_text))
            .await
            .unwrap();
        for (table, _) in tables {
            assert_eq!(
                pg_count(
                    &pool,
                    &format!("SELECT count(*) FROM {table} WHERE owner = current_user"),
                )
                .await,
                2
            );
        }

        // Alice has mapped her personal session's credential again since.
        // She claims the rest: three of her four placeholders, though bob
        // has a row for the shared session under her name. The fourth is
        // deleted, and her new mapping stays.
        pg_exec(&pool, &format!("SET ROLE {alice}")).await.unwrap();
        let again = uuid::Uuid::new_v4();
        set_user_credential(&pool, &personal, "alice", Some(&again.to_string()))
            .await
            .unwrap();
        assert_eq!(claim_legacy_mappings(&pool, "alice").await.unwrap(), 3);
        for (session, credential) in [(&shared, of_alice), (&personal, again)] {
            assert_eq!(
                query_user_credential(&pool, session, "alice")
                    .await
                    .unwrap(),
                Some(credential)
            );
            assert_eq!(
                query_user_login_sequence(&pool, session, "alice")
                    .await
                    .unwrap(),
                Some(of_alice)
            );
        }
        for (table, _) in tables {
            // She sees her own rows, no placeholder and nothing of bob's,
            // and can neither hand a row to him nor make it a placeholder
            // again.
            assert_eq!(
                pg_count(&pool, &format!("SELECT count(*) FROM {table}")).await,
                2
            );
            assert_eq!(
                pg_count(
                    &pool,
                    &format!("SELECT count(*) FROM {table} WHERE owner <> current_user"),
                )
                .await,
                0
            );
            for owner in [bob.as_str(), "local"] {
                let sql = format!("UPDATE {table} SET owner = '{owner}'");
                assert!(
                    refused(pg_exec(&pool, &sql).await, BY_ROW_SECURITY),
                    "{sql}"
                );
            }
        }
        // The statements of the commands, as alice: for one session, for a
        // folder, and clearing both.
        let other = uuid::Uuid::new_v4();
        let other_text = other.to_string();
        let folder = folder.to_string();
        set_user_credential(&pool, &shared, "alice", Some(&other_text))
            .await
            .unwrap();
        assert_eq!(
            query_user_credential(&pool, &shared, "alice")
                .await
                .unwrap(),
            Some(other)
        );
        set_user_credential(&pool, &shared, "alice", None)
            .await
            .unwrap();
        assert_eq!(
            query_user_credential(&pool, &shared, "alice")
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            bulk_set_user_credentials(&pool, &folder, "alice", Some(&other_text))
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            bulk_set_user_login_sequences(&pool, &folder, "alice", Some(&other_text))
                .await
                .unwrap(),
            2
        );
        for session in [&shared, &personal] {
            assert_eq!(
                query_user_credential(&pool, session, "alice")
                    .await
                    .unwrap(),
                Some(other)
            );
            assert_eq!(
                query_user_login_sequence(&pool, session, "alice")
                    .await
                    .unwrap(),
                Some(other)
            );
        }
        assert_eq!(
            bulk_set_user_login_sequences(&pool, &folder, "alice", None)
                .await
                .unwrap(),
            2
        );
        set_user_login_sequence(&pool, &shared, "alice", Some(&other_text))
            .await
            .unwrap();
        set_user_login_sequence(&pool, &shared, "alice", None)
            .await
            .unwrap();
        assert_eq!(
            pg_count(&pool, "SELECT count(*) FROM session_login_sequences").await,
            0
        );

        // Bob again: his rows are as he left them, alice's are out of reach,
        // and a folder-wide change of his finds no folder in one of alice's,
        // shared though it is.
        pg_exec(&pool, &format!("SET ROLE {bob}")).await.unwrap();
        assert_eq!(
            query_user_credential(&pool, &shared, "alice")
                .await
                .unwrap(),
            Some(planted)
        );
        for (table, column) in tables {
            assert_eq!(
                pg_count(
                    &pool,
                    &format!("SELECT count(*) FROM {table} WHERE owner <> current_user"),
                )
                .await,
                0
            );
            for sql in [
                format!("UPDATE {table} SET {column} = '{planted}' WHERE owner = '{alice}'"),
                format!("DELETE FROM {table} WHERE owner = '{alice}'"),
            ] {
                assert_eq!(
                    pg_exec(&pool, &sql).await.unwrap().rows_affected(),
                    0,
                    "{sql}"
                );
            }
        }
        for to in [Some(other_text.as_str()), None] {
            assert_eq!(
                bulk_set_user_credentials(&pool, &folder, "bob", to)
                    .await
                    .unwrap(),
                0
            );
            assert_eq!(
                bulk_set_user_login_sequences(&pool, &folder, "bob", to)
                    .await
                    .unwrap(),
                0
            );
        }
        assert_eq!(
            query_user_credential(&pool, &shared, "bob").await.unwrap(),
            Some(of_bob)
        );
        assert_eq!(
            query_user_login_sequence(&pool, &shared, "bob")
                .await
                .unwrap(),
            Some(of_bob)
        );

        // And alice's are untouched by all of that.
        pg_exec(&pool, &format!("SET ROLE {alice}")).await.unwrap();
        for session in [&shared, &personal] {
            assert_eq!(
                query_user_credential(&pool, session, "alice")
                    .await
                    .unwrap(),
                Some(other)
            );
        }

        // What a folder takes no more, put there by a superuser as rows from
        // before that rule are: in alice's folder a shared session of bob's
        // and a shared folder of his, with a session of hers in it. And two
        // folders of hers that name each other as their parent, with a
        // session in one. A statement that does not end fails here after 5 s.
        let (bobs_in_hers, hers_in_his, in_ring) = (
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        let (his_folder, ring_a, ring_b) = (
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
            uuid::Uuid::new_v4().to_string(),
        );
        let session_in = |id: &str, folder: &str, owner: &str| {
            format!(
                "INSERT INTO sessions (id, folder_id, name, hostname, username, owner, visibility) \
                 VALUES ('{id}', '{folder}', 'later', 'host.example', '', '{owner}', 'shared')"
            )
        };
        for sql in [
            "RESET ROLE".to_string(),
            session_in(&bobs_in_hers, &folder, &bob),
            format!(
                "INSERT INTO folders (id, name, parent_id, sort_order, owner, visibility) VALUES \
                 ('{his_folder}', 'his', '{folder}', 0, '{bob}', 'shared'), \
                 ('{ring_a}', 'ring a', NULL, 0, '{alice}', 'personal'), \
                 ('{ring_b}', 'ring b', '{ring_a}', 0, '{alice}', 'personal')"
            ),
            format!("UPDATE folders SET parent_id = '{ring_b}' WHERE id = '{ring_a}'"),
            session_in(&hers_in_his, &his_folder, &alice),
            session_in(&in_ring, &ring_b, &alice),
            "SET statement_timeout = '5s'".to_string(),
            format!("SET ROLE {alice}"),
        ] {
            pg_exec(&pool, &sql).await.expect(&sql);
        }
        // Alice can see all three sessions, and assigns to bob's by itself,
        // as My Login does, and to her own in his folder.
        let single = uuid::Uuid::new_v4();
        let apart = [&bobs_in_hers, &hers_in_his];
        for session in apart {
            set_user_credential(&pool, session, "alice", Some(&single.to_string()))
                .await
                .unwrap();
            set_user_login_sequence(&pool, session, "alice", Some(&single.to_string()))
                .await
                .unwrap();
        }
        // Her folder-wide change is for her own sessions in her own folders:
        // the two from before, and neither bob's session nor what is below
        // his folder. What she assigned to those two stays, when she assigns
        // to the folder and when she clears it.
        let wide = uuid::Uuid::new_v4();
        let wide_text = wide.to_string();
        for to in [Some(wide_text.as_str()), None] {
            assert_eq!(
                bulk_set_user_credentials(&pool, &folder, "alice", to)
                    .await
                    .unwrap(),
                2
            );
            assert_eq!(
                bulk_set_user_login_sequences(&pool, &folder, "alice", to)
                    .await
                    .unwrap(),
                2
            );
            for session in [&shared, &personal] {
                assert_eq!(
                    query_user_credential(&pool, session, "alice")
                        .await
                        .unwrap(),
                    to.map(|_| wide)
                );
                assert_eq!(
                    query_user_login_sequence(&pool, session, "alice")
                        .await
                        .unwrap(),
                    to.map(|_| wide)
                );
            }
            // Bob's folder in hers is no folder of hers by itself either.
            assert_eq!(
                bulk_set_user_credentials(&pool, &his_folder, "alice", to)
                    .await
                    .unwrap(),
                0
            );
            assert_eq!(
                bulk_set_user_login_sequences(&pool, &his_folder, "alice", to)
                    .await
                    .unwrap(),
                0
            );
            for session in apart {
                assert_eq!(
                    query_user_credential(&pool, session, "alice")
                        .await
                        .unwrap(),
                    Some(single)
                );
                assert_eq!(
                    query_user_login_sequence(&pool, session, "alice")
                        .await
                        .unwrap(),
                    Some(single)
                );
            }
        }
        // The walk ends on the ring, from either of its folders.
        for ring in [&ring_a, &ring_b] {
            for to in [Some(wide_text.as_str()), None] {
                assert_eq!(
                    bulk_set_user_credentials(&pool, ring, "alice", to).await,
                    Ok(1)
                );
                assert_eq!(
                    bulk_set_user_login_sequences(&pool, ring, "alice", to).await,
                    Ok(1)
                );
            }
        }

        pool.close().await;
    }

    async fn check_folder_walks_end_on_a_loop(
        opts: sqlx::postgres::PgConnectOptions,
        admin: String,
        alice: String,
        bob: String,
    ) {
        use commands::{
            bulk_set_user_credentials, bulk_set_user_login_sequences, set_item_visibility,
        };
        use db::{BulkSessionEdit, DatabaseProvider};

        // One connection, so each SET ROLE holds for the statements after
        // it, the app's own included.
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .unwrap();
        prepare_rls_test_database(&pool, &admin, &alice, &bob).await;
        let app = PostgresProvider::new(pool.clone());

        // Two folders of alice's that name each other as their parent and a
        // third below one of them, with a session in the first and in the
        // third. Made by a superuser, since the trigger keeps alice from it.
        // And a folder of hers apart from those, with a session. Depending
        // on how the server goes about a statement, it reads the whole walk
        // first, or it looks for each of her sessions or folders among the
        // folders of the walk and stops at the first it finds, and then only
        // one that is not among them has it walk to the end: without these
        // two, every statement here returned from a walk that does not end.
        // A statement that does not end fails here after 5 s.
        let (ring_a, ring_b, below, apart) = (
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4(),
        );
        for sql in [
            "RESET ROLE".to_string(),
            format!(
                "INSERT INTO folders (id, name, parent_id, sort_order, owner, visibility) VALUES \
                 ('{ring_a}', 'ring a', NULL, 0, '{alice}', 'personal'), \
                 ('{ring_b}', 'ring b', '{ring_a}', 0, '{alice}', 'personal'), \
                 ('{below}', 'below', '{ring_b}', 0, '{alice}', 'personal'), \
                 ('{apart}', 'apart', NULL, 0, '{alice}', 'personal')"
            ),
            format!("UPDATE folders SET parent_id = '{ring_b}' WHERE id = '{ring_a}'"),
            format!(
                "INSERT INTO sessions (id, folder_id, name, hostname, username, owner, visibility) VALUES \
                 ('{}', '{ring_a}', 'in the ring', 'host.example', '', '{alice}', 'personal'), \
                 ('{}', '{below}', 'below it', 'host.example', '', '{alice}', 'personal'), \
                 ('{}', '{apart}', 'apart', 'host.example', '', '{alice}', 'personal')",
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4(),
                uuid::Uuid::new_v4()
            ),
            "SET statement_timeout = '5s'".to_string(),
            format!("SET ROLE {alice}"),
        ] {
            pg_exec(&pool, &sql).await.expect(&sql);
        }
        assert!(refused(
            pg_exec(
                &pool,
                &format!("UPDATE folders SET parent_id = '{below}' WHERE id = '{ring_b}'")
            )
            .await,
            BY_RING
        ));

        // From either folder of the ring, each statement covers the two
        // sessions there, and the three folders where it changes folders,
        // and neither the folder apart nor its session.
        for start in [ring_a, ring_b] {
            let (profile, sequence) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
            // The session rows' own profile and login sequence.
            assert_eq!(
                app.bulk_set_session_credential_profile(start, Some(profile))
                    .await,
                Ok(2)
            );
            assert_eq!(
                app.bulk_set_session_login_sequence(start, Some(sequence))
                    .await,
                Ok(2)
            );
            // Bulk Edit, in both of its passes.
            let edit = BulkSessionEdit {
                login_sequence_id: Some(Some(sequence)),
                jump_host_id: Some(None),
                ..BulkSessionEdit::default()
            };
            assert_eq!(app.bulk_edit_sessions(start, edit).await, Ok(2));
            // A folder's visibility, for the folders below it and for the
            // sessions.
            for (visibility, shared) in [("shared", 5), ("personal", 0)] {
                assert_eq!(
                    set_item_visibility(&pool, start, "folder", visibility).await,
                    Ok(())
                );
                assert_eq!(
                    pg_count(
                        &pool,
                        "SELECT (SELECT count(*) FROM folders WHERE visibility = 'shared') \
                              + (SELECT count(*) FROM sessions WHERE visibility = 'shared')",
                    )
                    .await,
                    shared
                );
            }
            // The per-user mappings, set and cleared.
            let (start, profile) = (start.to_string(), profile.to_string());
            for to in [Some(profile.as_str()), None] {
                assert_eq!(
                    bulk_set_user_credentials(&pool, &start, "alice", to).await,
                    Ok(2)
                );
                assert_eq!(
                    bulk_set_user_login_sequences(&pool, &start, "alice", to).await,
                    Ok(2)
                );
            }
        }

        pool.close().await;
    }

    async fn check_rls_setup_check_finds_what_is_missing(
        opts: sqlx::postgres::PgConnectOptions,
        admin: String,
        alice: String,
        bob: String,
    ) {
        use commands::RlsSetupStatus;

        // Three connections: the superuser's, which takes things away and
        // puts back what the setup cannot; the owning role's, which migrates
        // and runs the setup; and a regular user's, made as the app makes
        // its own, which asks.
        let server = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone())
            .await
            .unwrap();
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&server)
            .await
            .unwrap();
        for sql in [
            format!("GRANT ALL PRIVILEGES ON DATABASE {database} TO {admin}"),
            format!("GRANT ALL ON SCHEMA public TO {admin}"),
            format!("GRANT USAGE ON SCHEMA public TO {alice}, {bob}"),
        ] {
            pg_exec(&server, &sql).await.unwrap();
        }
        let owner = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(opts.clone())
            .await
            .unwrap();
        pg_exec(&owner, &format!("SET ROLE {admin}")).await.unwrap();
        sqlx::migrate!().run(&owner).await.unwrap();
        pg_exec(
            &owner,
            &format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public \
                 TO {alice}, {bob}"
            ),
        )
        .await
        .unwrap();
        let (user, _) = app_connection_as(&opts, &alice).await;
        let missing = |pool: &sqlx::PgPool| {
            let pool = pool.clone();
            async move {
                check_postgres_rls(&pool)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|gap| gap.what)
                    .collect::<Vec<_>>()
            }
        };

        // A database the setup has never been applied to, as one is whose
        // first setup failed: the schema and the four functions, and on the
        // tables two constraints, two primary keys, seven triggers, eight
        // policies and row-level security four times. None of it is there
        // in another version's shape, and nothing is there that is to go.
        // And the grant above, as the admin guide had it, lets the two users
        // write to the table of applied migrations.
        let gaps = check_postgres_rls(&user).await.unwrap();
        assert_eq!(gaps.len(), 5 + 2 + 2 + 7 + 8 + 4 + 1, "{gaps:?}");
        assert!(gaps.iter().all(|gap| !gap.what.contains(", which is")));
        assert_eq!(
            gaps.last().map(|gap| gap.what.as_str()),
            Some("privileges on _sqlx_migrations")
        );
        assert_eq!(
            rls_gaps_status(&gaps),
            RlsSetupStatus::Incomplete {
                tables: RLS_TABLES
                    .into_iter()
                    .chain([MIGRATIONS_TABLE])
                    .map(str::to_string)
                    .collect(),
                functions: true,
            }
        );
        // Which they can, until the setup has run: one of them marks a
        // migration as failed, and every start would go into the migrations.
        let fail_one = "UPDATE _sqlx_migrations SET success = false \
                        WHERE version = (SELECT min(version) FROM _sqlx_migrations)";
        pg_exec(&user, fail_one).await.unwrap();
        assert!(!pg_migrations_up_to_date(&user).await.unwrap());
        pg_exec(&server, "UPDATE _sqlx_migrations SET success = true")
            .await
            .unwrap();

        // After the setup nothing is missing, whoever asks.
        setup_postgres_rls(&owner).await.unwrap();
        for pool in [&user, &owner, &server] {
            assert_eq!(missing(pool).await, Vec::<String>::new());
        }
        assert_eq!(rls_setup_status(&user).await, RlsSetupStatus::InPlace);

        // A regular user cannot name the functions, which is why the check
        // finds them by their names in the catalog.
        assert!(refused(
            pg_exec(
                &user,
                &format!("SELECT '{RLS_SCHEMA}.shellstation_keep_owner()'::regprocedure"),
            )
            .await,
            "permission denied for schema",
        ));

        // Each of these takes a part of the setup away, or leaves one in
        // its place that is not the setup's, and the check names that part
        // and no other. The setup then puts it back, after the statements
        // that put back what it cannot.
        let does_nothing = "RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END'";
        let cases: Vec<(Vec<String>, Vec<&str>, Vec<String>)> = vec![
            (
                vec!["ALTER TABLE folders NO FORCE ROW LEVEL SECURITY".into()],
                vec!["row level security on folders"],
                vec![],
            ),
            (
                vec!["ALTER TABLE sessions DISABLE ROW LEVEL SECURITY".into()],
                vec!["row level security on sessions"],
                vec![],
            ),
            (
                vec!["DROP POLICY sessions_shared_read ON sessions".into()],
                vec!["policy sessions_shared_read"],
                vec![],
            ),
            (
                vec!["DROP POLICY session_credentials_owner_all ON session_credentials".into()],
                vec!["policy session_credentials_owner_all"],
                vec![],
            ),
            // The policy of an earlier version that let anyone update a
            // shared row.
            (
                vec!["CREATE POLICY folders_shared_update ON folders FOR UPDATE \
                     USING (visibility = 'shared')"
                    .into()],
                vec!["policy folders_shared_update, which is to go"],
                vec![],
            ),
            (
                vec!["ALTER TABLE folders DROP CONSTRAINT folders_vis_ck".into()],
                vec!["constraint folders_vis_ck"],
                vec![],
            ),
            (
                vec!["ALTER TABLE session_login_sequences \
                     RENAME CONSTRAINT session_login_sequences_owner_key TO another_key"
                    .into()],
                vec!["primary key of session_login_sequences"],
                vec!["ALTER TABLE session_login_sequences \
                     RENAME CONSTRAINT another_key TO session_login_sequences_owner_key"
                    .into()],
            ),
            // A constraint of the primary key's name that is none. The
            // setup makes the key from the index of that name, as the
            // migration leaves it.
            (
                vec![
                    "ALTER TABLE session_credentials \
                     DROP CONSTRAINT session_credentials_owner_key"
                        .into(),
                    "ALTER TABLE session_credentials ADD CONSTRAINT session_credentials_owner_key \
                     UNIQUE (session_id, owner, user_ident)"
                        .into(),
                ],
                vec!["primary key of session_credentials"],
                vec![
                    "ALTER TABLE session_credentials \
                     DROP CONSTRAINT session_credentials_owner_key"
                        .into(),
                    "CREATE UNIQUE INDEX session_credentials_owner_key \
                     ON session_credentials(session_id, owner, user_ident)"
                        .into(),
                ],
            ),
            (
                vec!["DROP TRIGGER sessions_own_jump_host ON sessions".into()],
                vec!["trigger sessions_own_jump_host"],
                vec![],
            ),
            (
                vec!["ALTER TABLE folders DISABLE TRIGGER folders_keep_owner".into()],
                vec!["trigger folders_keep_owner"],
                vec![],
            ),
            // A trigger of the name that calls a function elsewhere.
            (
                vec![
                    format!("CREATE FUNCTION public.shellstation_keep_place() {does_nothing}"),
                    "DROP TRIGGER folders_keep_place ON folders".into(),
                    "CREATE TRIGGER folders_keep_place BEFORE UPDATE OF parent_id ON folders \
                     FOR EACH ROW WHEN (OLD.parent_id IS DISTINCT FROM NEW.parent_id) \
                     EXECUTE FUNCTION public.shellstation_keep_place()"
                        .into(),
                ],
                vec!["trigger folders_keep_place"],
                vec![],
            ),
            // One that calls the setup's function, for another column.
            (
                vec![
                    "DROP TRIGGER sessions_keep_place ON sessions".into(),
                    format!(
                        "CREATE TRIGGER sessions_keep_place BEFORE UPDATE OF name ON sessions \
                         FOR EACH ROW WHEN (OLD.name IS DISTINCT FROM NEW.name) \
                         EXECUTE FUNCTION {RLS_SCHEMA}.shellstation_keep_place()"
                    ),
                ],
                vec!["trigger sessions_keep_place"],
                vec![],
            ),
            // A function with another body, as one is that a start of the
            // owning role has still to replace. Its triggers still call it.
            (
                vec![format!(
                    "CREATE OR REPLACE FUNCTION {RLS_SCHEMA}.shellstation_own_folder() \
                     {does_nothing}"
                )],
                vec!["function shellstation_own_folder, which is not this version's"],
                vec![],
            ),
            // One that calls it with another argument, which tells the
            // function which column to look at; one without its condition;
            // and one that does not fire for new rows.
            (
                vec![
                    "DROP TRIGGER sessions_own_folder ON sessions".into(),
                    format!(
                        "CREATE TRIGGER sessions_own_folder \
                         BEFORE INSERT OR UPDATE OF folder_id ON sessions FOR EACH ROW \
                         EXECUTE FUNCTION {RLS_SCHEMA}.shellstation_own_folder('name')"
                    ),
                ],
                vec!["trigger sessions_own_folder"],
                vec![],
            ),
            (
                vec![
                    "DROP TRIGGER folders_keep_owner ON folders".into(),
                    format!(
                        "CREATE TRIGGER folders_keep_owner BEFORE UPDATE OF owner ON folders \
                         FOR EACH ROW EXECUTE FUNCTION {RLS_SCHEMA}.shellstation_keep_owner()"
                    ),
                ],
                vec!["trigger folders_keep_owner"],
                vec![],
            ),
            (
                vec![
                    "DROP TRIGGER folders_own_folder ON folders".into(),
                    format!(
                        "CREATE TRIGGER folders_own_folder BEFORE UPDATE OF parent_id ON folders \
                         FOR EACH ROW \
                         EXECUTE FUNCTION {RLS_SCHEMA}.shellstation_own_folder('parent_id')"
                    ),
                ],
                vec!["trigger folders_own_folder"],
                vec![],
            ),
            // The other way round: an argument where there is none to have,
            // and a condition on the one that looks at the column itself.
            (
                vec![
                    "DROP TRIGGER sessions_keep_owner ON sessions".into(),
                    format!(
                        "CREATE TRIGGER sessions_keep_owner BEFORE UPDATE OF owner ON sessions \
                         FOR EACH ROW WHEN (OLD.owner IS DISTINCT FROM NEW.owner) \
                         EXECUTE FUNCTION {RLS_SCHEMA}.shellstation_keep_owner('owner')"
                    ),
                ],
                vec!["trigger sessions_keep_owner"],
                vec![],
            ),
            (
                vec![
                    "DROP TRIGGER folders_own_folder ON folders".into(),
                    format!(
                        "CREATE TRIGGER folders_own_folder \
                         BEFORE INSERT OR UPDATE OF parent_id ON folders FOR EACH ROW \
                         WHEN (NEW.parent_id IS NOT NULL) \
                         EXECUTE FUNCTION {RLS_SCHEMA}.shellstation_own_folder('parent_id')"
                    ),
                ],
                vec!["trigger folders_own_folder"],
                vec![],
            ),
            // A function that is no longer pinned to its search path, one
            // pinned to another, and one that runs as its owner: what the
            // setup's `CREATE OR REPLACE` puts back.
            (
                vec![format!(
                    "ALTER FUNCTION {RLS_SCHEMA}.shellstation_keep_place() RESET search_path"
                )],
                vec!["function shellstation_keep_place, which is not this version's"],
                vec![],
            ),
            (
                vec![format!(
                    "ALTER FUNCTION {RLS_SCHEMA}.shellstation_keep_place() \
                     SET search_path = public, pg_catalog"
                )],
                vec!["function shellstation_keep_place, which is not this version's"],
                vec![],
            ),
            (
                vec![format!(
                    "ALTER FUNCTION {RLS_SCHEMA}.shellstation_own_jump_host() SECURITY DEFINER"
                )],
                vec!["function shellstation_own_jump_host, which is not this version's"],
                vec![],
            ),
            // A function and the schema that are another role's, who could
            // then change what the triggers call.
            (
                vec![format!(
                    "ALTER FUNCTION {RLS_SCHEMA}.shellstation_keep_owner() OWNER TO {bob}"
                )],
                vec!["function shellstation_keep_owner"],
                vec![format!(
                    "ALTER FUNCTION {RLS_SCHEMA}.shellstation_keep_owner() OWNER TO {admin}"
                )],
            ),
            (
                vec![format!("ALTER SCHEMA {RLS_SCHEMA} OWNER TO {bob}")],
                vec!["schema shellstation_rls"],
                vec![format!("ALTER SCHEMA {RLS_SCHEMA} OWNER TO {admin}")],
            ),
            // A function that is gone, and its trigger with it.
            (
                vec![format!(
                    "DROP FUNCTION {RLS_SCHEMA}.shellstation_own_jump_host() CASCADE"
                )],
                vec![
                    "function shellstation_own_jump_host",
                    "trigger sessions_own_jump_host",
                ],
                vec![],
            ),
            // A privilege on the schema for everyone, which lets a role name
            // the functions; the setup revokes it.
            (
                vec![format!("GRANT USAGE ON SCHEMA {RLS_SCHEMA} TO PUBLIC")],
                vec!["schema shellstation_rls, on which others hold privileges"],
                vec![],
            ),
            (
                vec![format!("GRANT CREATE ON SCHEMA {RLS_SCHEMA} TO {bob}")],
                vec!["schema shellstation_rls, on which others hold privileges"],
                vec![],
            ),
            // More than reading on the table of applied migrations: for one
            // role, for everyone, and as the admin guide's grant on all
            // tables gave it. The setup revokes it.
            (
                vec![format!("GRANT UPDATE ON _sqlx_migrations TO {bob}")],
                vec!["privileges on _sqlx_migrations"],
                vec![],
            ),
            (
                vec!["GRANT ALL PRIVILEGES ON _sqlx_migrations TO PUBLIC".into()],
                vec!["privileges on _sqlx_migrations"],
                vec![],
            ),
            (
                vec![format!(
                    "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public \
                     TO {alice}, {bob}"
                )],
                vec!["privileges on _sqlx_migrations"],
                vec![],
            ),
            // On one column alone, which the server keeps apart from what is
            // held on the table.
            (
                vec![format!(
                    "GRANT UPDATE (success) ON _sqlx_migrations TO {bob}"
                )],
                vec!["privileges on _sqlx_migrations"],
                vec![],
            ),
            // Passed on by a role that was allowed to: both lose it.
            (
                vec![
                    format!("GRANT DELETE ON _sqlx_migrations TO {bob} WITH GRANT OPTION"),
                    format!("SET ROLE {bob}"),
                    format!("GRANT DELETE ON _sqlx_migrations TO {alice}"),
                    "RESET ROLE".into(),
                ],
                vec!["privileges on _sqlx_migrations"],
                vec![],
            ),
            // The table as another role's, who writes to it whatever is
            // revoked. Only a superuser gives it back.
            (
                vec![format!("ALTER TABLE _sqlx_migrations OWNER TO {bob}")],
                vec!["table _sqlx_migrations, which is not the owning role's"],
                vec![format!("ALTER TABLE _sqlx_migrations OWNER TO {admin}")],
            ),
        ];
        for (taken_away, named, put_back) in cases {
            for sql in &taken_away {
                pg_exec(&server, sql).await.unwrap();
            }
            assert_eq!(missing(&user).await, named, "{taken_away:?}");
            for sql in &put_back {
                pg_exec(&server, sql).await.unwrap();
            }
            setup_postgres_rls(&owner).await.unwrap();
            assert_eq!(missing(&user).await, Vec::<String>::new(), "{taken_away:?}");
        }

        // A regular user reads the table of applied migrations and cannot
        // write to it any more: not mark one as failed, not take one away,
        // not add one of a later release.
        for sql in [
            fail_one,
            "DELETE FROM _sqlx_migrations",
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
             VALUES (99990101000000, 'later', true, '\\x00', 0)",
        ] {
            assert!(
                refused(pg_exec(&user, sql).await, "permission denied"),
                "{sql}"
            );
        }
        assert!(pg_migrations_up_to_date(&user).await.unwrap());
        // Nor lock it against anyone who reads it, which takes more than
        // reading it oneself.
        let mut locking = user.begin().await.unwrap();
        let locked = sqlx::query("LOCK TABLE _sqlx_migrations IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *locking)
            .await;
        assert!(refused(locked, "permission denied"));
        locking.rollback().await.unwrap();

        // Nor does a start need to: one that finds every migration applied
        // leaves a checksum alone that differs by its line endings, and the
        // owning role puts it right, as it does before it migrates.
        let migrator = sqlx::migrate!();
        let first = migrator.iter().next().unwrap();
        let with_lf = first.sql.as_str().replace("\r\n", "\n");
        let other_endings = [with_lf.clone(), with_lf.replace('\n', "\r\n")]
            .into_iter()
            .map(|sql| Sha384::digest(sql.as_bytes()).to_vec())
            .find(|hash| hash.as_slice() != first.checksum.as_ref())
            .unwrap();
        let version = first.version;
        let stored = || {
            let server = server.clone();
            async move {
                sqlx::query_scalar::<_, Vec<u8>>(
                    "SELECT checksum FROM _sqlx_migrations WHERE version = $1",
                )
                .bind(version)
                .fetch_one(&server)
                .await
                .unwrap()
            }
        };
        sqlx::query("UPDATE _sqlx_migrations SET checksum = $1 WHERE version = $2")
            .bind(&other_endings)
            .bind(version)
            .execute(&server)
            .await
            .unwrap();
        assert!(apply_postgres_migrations(&user).await.is_ok());
        assert_eq!(stored().await, other_endings);
        assert!(fix_crlf_migration_checksums(&owner).await.is_ok());
        assert_eq!(stored().await, first.checksum.as_ref());

        // Revoking waits for no lock that a writer holds on the table.
        pg_exec(
            &server,
            &format!("GRANT UPDATE ON _sqlx_migrations TO {bob}"),
        )
        .await
        .unwrap();
        let mut held = server.begin().await.unwrap();
        sqlx::query("LOCK TABLE _sqlx_migrations IN EXCLUSIVE MODE")
            .execute(&mut *held)
            .await
            .unwrap();
        let set_up = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            setup_postgres_rls(&owner),
        )
        .await;
        assert!(matches!(set_up, Ok(Ok(()))), "{set_up:?}");
        held.rollback().await.unwrap();
        assert_eq!(missing(&user).await, Vec::<String>::new());

        // For a lock against every other use it does wait, and gives up as
        // any other step does. The check says what is then left.
        pg_exec(
            &server,
            &format!("GRANT UPDATE ON _sqlx_migrations TO {bob}"),
        )
        .await
        .unwrap();
        pg_exec(&owner, "SET lock_timeout = '300ms'").await.unwrap();
        let mut held = server.begin().await.unwrap();
        sqlx::query("LOCK TABLE _sqlx_migrations IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *held)
            .await
            .unwrap();
        let started = std::time::Instant::now();
        let set_up = setup_postgres_rls(&owner).await;
        assert!(gave_up_on_lock(&set_up, started), "{set_up:?}");
        assert!(set_up
            .unwrap_err()
            .contains("privileges on _sqlx_migrations"));
        assert_eq!(missing(&user).await, ["privileges on _sqlx_migrations"]);
        held.rollback().await.unwrap();
        pg_exec(&owner, "RESET lock_timeout").await.unwrap();
        setup_postgres_rls(&owner).await.unwrap();
        assert_eq!(missing(&user).await, Vec::<String>::new());

        // A row for a migration that this build does not carry, as a later
        // version leaves one, or as someone could add one while the table
        // was theirs to write: it is found, and counts for nothing here.
        assert_eq!(
            migrations_not_of_this_build(&user).await.unwrap(),
            Vec::<i64>::new()
        );
        pg_exec(
            &server,
            "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) \
             VALUES (99990101000000, 'later', true, '\\x00', 0)",
        )
        .await
        .unwrap();
        assert_eq!(
            migrations_not_of_this_build(&user).await.unwrap(),
            [99_990_101_000_000]
        );
        assert!(pg_migrations_up_to_date(&user).await.unwrap());
        pg_exec(
            &server,
            "DELETE FROM _sqlx_migrations WHERE version = 99990101000000",
        )
        .await
        .unwrap();

        // What the window is told: the tables, and whether the functions.
        let on = |table: &'static str, what: &str| RlsGap {
            table: Some(table),
            what: what.to_string(),
        };
        assert_eq!(
            rls_gaps_status(&[
                on("session_credentials", "a policy"),
                on("sessions", "a trigger"),
                on("sessions", "a policy"),
            ]),
            RlsSetupStatus::Incomplete {
                tables: vec!["sessions".to_string(), "session_credentials".to_string()],
                functions: false,
            }
        );
        assert_eq!(
            rls_gaps_status(&[RlsGap {
                table: None,
                what: "a function".to_string(),
            }]),
            RlsSetupStatus::Incomplete {
                tables: vec![],
                functions: true,
            }
        );

        // The check reads the catalog, so it does not wait for a lock that
        // someone holds on a table, as the setup's steps do: with all of
        // `folders` locked, it still says what is missing there, and soon.
        pg_exec(&server, "DROP POLICY folders_shared_read ON folders")
            .await
            .unwrap();
        let mut held = server.begin().await.unwrap();
        sqlx::query("LOCK TABLE folders IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *held)
            .await
            .unwrap();
        let started = std::time::Instant::now();
        assert_eq!(missing(&user).await, ["policy folders_shared_read"]);
        assert_eq!(
            rls_setup_status(&user).await,
            RlsSetupStatus::Incomplete {
                tables: vec!["folders".to_string()],
                functions: false,
            }
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        held.rollback().await.unwrap();
        setup_postgres_rls(&owner).await.unwrap();
        assert_eq!(rls_setup_status(&user).await, RlsSetupStatus::InPlace);

        // A check that fails is not taken for a setup in place.
        user.close().await;
        assert_eq!(rls_setup_status(&user).await, RlsSetupStatus::Unchecked);

        owner.close().await;
        server.close().await;
    }
}
