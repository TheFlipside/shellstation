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

/// When the triggers of [`setup_postgres_rls`] fire, as `pg_trigger.tgtype`
/// has it: a row trigger (1) that fires before (2) an update (16), and for
/// the ones that look at what a row is put into or names as its jump host,
/// an insert (4) as well.
const ON_CHANGE: i16 = 1 | 2 | 16;
const ON_PLACING: i16 = ON_CHANGE | 4;

/// The tables under row-level security; see [`setup_postgres_rls`].
const RLS_TABLES: [&str; 4] = ["folders", "sessions", MAPPING_TABLES[0], MAPPING_TABLES[1]];

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
    //   this function does. That fix-up finds such rows at the first setup
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
    // no one else can have put one. They are replaced on every run so that
    // a fix to them reaches existing databases, which locks no table; only
    // the table owner gets this far, the ownership check above stops
    // everyone else. A failure to replace one does not stop the steps after
    // it, but its triggers are then left alone.
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
    let functions = [
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
    ];
    // The schema for them (`RLS_SCHEMA`) is made for the role that owns the
    // tables, also when a member of that role or a superuser runs this. One
    // that is there and is another role's is given to it, which only a
    // superuser can do, or the role it belongs to if it is a member of the
    // owning role. Where that fails, the schema is not to be trusted with
    // the functions: none is created, and the triggers are left alone.
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
                 LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $fn$ {body} $fn$; \
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
    let triggers = [
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
    ];

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
    //
    // A shared row has a policy for reading it and none for changing it:
    // the owner's `*_owner_all` is the only one that lets a row be updated.
    // `*_shared_delete` says nothing that `*_owner_all` does not, and is
    // left as it is.
    let policies = [
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
    ];

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
        for (name, _, function, definition, tgtype, column) in triggers
            .iter()
            .filter(|trigger| trigger.1 == table && replaced.contains(&trigger.2))
        {
            // Whether a trigger has an argument and a condition is read off
            // the definition it is created with, so the two cannot differ:
            // `shellstation_own_folder` has its column as its argument and
            // no condition, the other functions a condition and no argument.
            //
            // The argument is kept as its bytes with a zero byte after it,
            // which `encode` writes as a backslash and three zeros. The text
            // to compare it with is an `E''` string, in which a backslash is
            // written twice whatever `standard_conforming_strings` says.
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
            let argument_and_condition = format!("{argument} AND {condition}");
            let look = format!(
                "SELECT t.oid, t.tgfoid, \
                        t.tgfoid = '{RLS_SCHEMA}.{function}()'::pg_catalog.regprocedure::pg_catalog.oid \
                            AND t.tgenabled = 'O' AND t.tgtype = {tgtype} \
                            AND t.tgattr::pg_catalog.text = ( \
                                SELECT a.attnum::pg_catalog.text FROM pg_catalog.pg_attribute a \
                                WHERE a.attrelid = t.tgrelid AND a.attname = '{column}') \
                            AND {argument_and_condition} \
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
    let Some(AppLogFile {
        appender,
        dir: log_dir,
        dir_problem,
    }) = log_file
    else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(stdout_layer)
            .init();
        return None;
    };

    let (non_blocking, guard) = tracing_appender::non_blocking(appender);
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
    Some(guard)
}

/// The application log as [`open_app_log_file`] opens it.
struct AppLogFile {
    appender: tracing_appender::rolling::RollingFileAppender,
    /// The directory the log is in.
    dir: std::path::PathBuf,
    /// Why the directory could not be restricted to its owner, if it could
    /// not: the log is written there all the same.
    dir_problem: Option<String>,
}

/// Open the daily-rotating application log file for `init_tracing`.
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
    // on Unix and left as it is on Windows. The logging library creates the
    // log file itself, by opening whatever holds the day's name, and with
    // the mode the umask leaves it. On Unix the directory is therefore all
    // that keeps others from reading the log, and only once it has been
    // restricted.
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
    // directory that others may read.
    let restricted = crate::private_fs::restrict_dir_to_owner_nofollow(&log_dir);
    let dir_problem = restricted.err().map(|e| {
        format!(
            "Failed to restrict the application log directory to its owner: {e} — \
             others may be able to read the application log"
        )
    });
    if let Some(problem) = &dir_problem {
        eprintln!("{problem} ({})", log_dir.display());
    }
    // Creating the directory can succeed and opening the file still fail
    // (directory not writable, disk full); that falls back the same way.
    match tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("shellstation")
        .filename_suffix("log")
        .build(&log_dir)
    {
        Ok(appender) => Some(AppLogFile {
            appender,
            dir: log_dir,
            dir_problem,
        }),
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
    // After init_tracing, so that a keychain problem is logged.
    config::load_postgres_password(&mut app_config);

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
            import::inspect_securecrt,
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
        drop(opened);
        // A chosen directory that had to be created is its owner's alone,
        // as the session logger's is.
        private_fs::assert_private(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A chosen directory that is already there is restricted to its owner
    /// on Unix, as the session logger restricts its own, so that the log
    /// file, which the logging library creates with the umask's mode, cannot
    /// be read by others.
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
        pg_exec(
            pool,
            &format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public \
                 TO {alice}, {bob}"
            ),
        )
        .await
        .unwrap();
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
        // and a folder-wide change of his reaches the one session he can see.
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
        assert_eq!(
            bulk_set_user_credentials(&pool, &folder, "bob", None)
                .await
                .unwrap(),
            1
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

        pool.close().await;
    }
}
