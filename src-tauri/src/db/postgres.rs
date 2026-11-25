use async_trait::async_trait;
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::models::{
    Credential, CredentialProfile, DataFingerprint, Folder, HighlightProfile, LoginSequence,
    LoginSequenceStep, NewCredentialProfile, NewFolder, NewHighlightProfile, NewLoginSequence,
    NewSession, Session, UpdateCredentialProfile, UpdateHighlightProfile, UpdateLoginSequence,
    UpdateSession,
};
use super::update_builder::{BindVal, Dialect, SessionColumn, UpdateBuilder};
use super::{cmp_hostname, search_pattern, BulkSessionEdit, DatabaseProvider, DbResult};

/// The limits every connection of the app sets for itself, as `name` and
/// value pairs: how long a statement waits for a lock, how long a statement
/// may take, and how long a transaction may sit idle.
///
/// Row and table locks are held by whoever takes them for as long as their
/// transaction is open, and `PostgreSQL` lets a statement wait for one
/// without end. Any role with the app's privileges can take them: a row lock
/// on every shared row with `SELECT … FOR UPDATE`, a lock on a whole table
/// with `LOCK TABLE`. So can a transaction left open in another tool, or a
/// client that stopped in the middle of one. Without a limit, a statement of
/// this app that needs such a lock never returns: the edit hangs and keeps
/// its connection, and a start waits in the setup before there is a window.
///
/// - `lock_timeout`: a statement that cannot get a lock in this time fails,
///   and the edit can be tried again.
/// - `statement_timeout`: the same for a statement that takes this long for
///   any other reason.
/// - `idle_in_transaction_session_timeout`: the server ends a session of
///   this app that has left a transaction open for this long, with the locks
///   it holds. The app's transactions do not wait for the user, so only a
///   client that has stopped gets there.
///
/// These protect the other users from this client and this client from
/// waiting. They do not stop another role from holding a lock: a session
/// sets its own limits, so nothing set here or on the server binds a role
/// that sends its own statements (`docs/ADMIN_GUIDE.md` §7).
const SESSION_TIMEOUTS: [(&str, &str); 3] = [
    ("lock_timeout", "5s"),
    ("statement_timeout", "60s"),
    ("idle_in_transaction_session_timeout", "30s"),
];

/// Sets each of [`SESSION_TIMEOUTS`] that the server has not set. A value
/// from the server, such as one an admin gave the database or the role with
/// `ALTER … SET`, is kept, shorter or longer. A timeout that is off reads as
/// `0`, whether nobody set it or someone set it to `0`, so the server cannot
/// switch one off for the app: the app then sets its own.
fn session_timeouts_sql() -> String {
    let values = SESSION_TIMEOUTS
        .map(|(name, value)| format!("('{name}', '{value}')"))
        .join(", ");
    format!(
        "SELECT pg_catalog.set_config(name, value, false) \
         FROM (VALUES {values}) AS timeouts(name, value) \
         WHERE pg_catalog.current_setting(name) = '0'"
    )
}

/// The options of the pool the app's `PostgreSQL` connections come from.
/// Every connection sets [`SESSION_TIMEOUTS`] before it is used, the ones
/// that run the migrations and the row-level security setup at startup
/// included.
///
/// The timeouts are set by a statement, not as startup parameters of the
/// connection, which a connection pooler in front of the server may refuse.
/// They are settings of the session, so they hold only where the app's
/// connection is one session on the server: directly, or through a pooler
/// that pools sessions. A pooler that hands each transaction to another
/// session does not carry them along.
pub fn pool_options() -> sqlx::postgres::PgPoolOptions {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(session_timeouts_sql()))
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
}

pub struct PostgresProvider {
    pool: PgPool,
}

impl PostgresProvider {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// The longest a statement of a search may take, in milliseconds.
///
/// A search reads every folder and every session the role can see, since no
/// index serves its pattern, and the search for the next text waits for it
/// (`SearchGate` in `commands/session.rs`). What it finds is for a list in
/// the sidebar: an answer that takes longer than this is of no use, and a
/// statement that goes on for the minute the app's other statements have
/// ([`SESSION_TIMEOUTS`]) loads the shared server for that long and keeps
/// the next search waiting.
const SEARCH_STATEMENT_TIMEOUT_MS: i64 = 10_000;

/// Let no statement of the transaction on `conn` take longer than `limit_ms`
/// milliseconds, which is more than none. A shorter limit that is in
/// effect, the server's or the app's, stays; the limit set here ends with
/// the transaction, so `conn` has to be in one. Nothing here sees to that:
/// outside of a transaction the limit is gone as soon as it is set, without
/// an error, and nothing is limited.
///
/// What is in effect is read as the number of milliseconds that
/// `pg_settings` gives as text, and compared here, so that the statement
/// compares nothing but two `text`s. The two statements of this function
/// run before the limit is set, with the limit that is in effect.
pub(crate) async fn limit_statement_time(
    conn: &mut sqlx::PgConnection,
    limit_ms: i64,
) -> DbResult<()> {
    debug_assert!(limit_ms > 0, "a limit of 0 is none");
    let current: String = sqlx::query_scalar(
        "SELECT setting FROM pg_catalog.pg_settings WHERE name = 'statement_timeout'",
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| format!("Failed to read the statement timeout: {e}"))?;
    // `0` is no limit; so is anything that is no number.
    let current = current.parse::<i64>().unwrap_or(0);
    if current > 0 && current <= limit_ms {
        return Ok(());
    }
    sqlx::query("SELECT pg_catalog.set_config('statement_timeout', $1, true)")
        .bind(limit_ms.to_string())
        .execute(&mut *conn)
        .await
        .map_err(|e| format!("Failed to set the statement timeout: {e}"))?;
    Ok(())
}

fn parse_uuid(s: &str) -> DbResult<Uuid> {
    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

fn parse_optional_uuid(s: Option<&str>) -> DbResult<Option<Uuid>> {
    match s {
        Some(v) => Ok(Some(parse_uuid(v)?)),
        None => Ok(None),
    }
}

/// What a command says when it is given a jump host that is not one of the
/// user's own sessions; see [`check_own_jump_host`]. The data import goes by
/// it to leave such a jump host out and go on.
pub(crate) const NOT_OWN_JUMP_HOST: &str = "A jump host has to be one of your own sessions";

/// Refuses `jump_host` as a session's jump host unless it is a session of
/// the user's own.
///
/// A session's jump host is a session of the same owner: whoever owns a
/// shared session would otherwise decide through which of another user's
/// sessions the people who open it connect, and the owner of a jump host
/// could take it from under another user's session by deleting it. The
/// database refuses the same (`shellstation_own_jump_host`, set up by
/// `setup_postgres_rls`). This is the app's own check, as its statements
/// check a folder: it has a message of its own, and it holds on a database
/// whose owning role has not started the version with that trigger yet.
///
/// `keeps` is the session being changed, if it is one that exists and is the
/// user's own: a jump host it has already is no change, and is let through
/// as the trigger lets it through. Such a session names another user's from
/// before this was refused, and can still be edited; it is not connected
/// through that jump host (`commands::session::jump_host_is_its_owners`).
/// A bulk edit keeps none: it sets one jump host for many sessions, and is
/// refused one that is not the user's own even if a session among them has
/// it already, where the trigger would let that one row through.
///
/// Only creating a session has a transaction for this to be part of. In the
/// other two the statement that writes comes after it, by itself: a row's
/// owner does not change in between (`shellstation_keep_owner`), and a jump
/// host deleted in between fails the write at the foreign key.
async fn check_own_jump_host<'e, E>(
    executor: E,
    jump_host: Uuid,
    keeps: Option<Uuid>,
) -> DbResult<()>
where
    E: sqlx::PgExecutor<'e>,
{
    let allowed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM sessions WHERE id = $1 AND owner = current_user) \
             OR EXISTS (SELECT 1 FROM sessions \
                        WHERE id = $2 AND jump_host_id = $1 AND owner = current_user)",
    )
    .bind(jump_host.to_string())
    .bind(keeps.map(|id| id.to_string()))
    .fetch_one(executor)
    .await
    .map_err(|e| format!("Failed to check jump host ownership: {e}"))?;
    if allowed {
        Ok(())
    } else {
        Err(NOT_OWN_JUMP_HOST.to_string())
    }
}

fn row_to_folder(row: &PgRow) -> DbResult<Folder> {
    Ok(Folder {
        id: parse_uuid(row.get("id"))?,
        name: row.get("name"),
        parent_id: parse_optional_uuid(row.get("parent_id"))?,
        sort_order: row.get("sort_order"),
        owner: row.get("owner"),
        visibility: row.get("visibility"),
    })
}

fn row_to_session(row: &PgRow) -> DbResult<Session> {
    Ok(Session {
        id: parse_uuid(row.get("id"))?,
        folder_id: parse_uuid(row.get("folder_id"))?,
        name: row.get("name"),
        hostname: row.get("hostname"),
        port: row.get("port"),
        protocol: row.get("protocol"),
        username: row.get("username"),
        auth_method: row.get("auth_method"),
        jump_host_id: parse_optional_uuid(row.get("jump_host_id"))?,
        tags: row.get("tags"),
        icon: row.get("icon"),
        sort_order: row.get("sort_order"),
        highlight_profile_id: parse_optional_uuid(row.get("highlight_profile_id"))?,
        credential_profile_id: parse_optional_uuid(row.get("credential_profile_id"))?,
        legacy_algorithms: row.get::<i32, _>("legacy_algorithms") != 0,
        login_sequence_id: parse_optional_uuid(row.get("login_sequence_id"))?,
        owner: row.get("owner"),
        visibility: row.get("visibility"),
    })
}

fn row_to_credential_profile(row: &PgRow) -> DbResult<CredentialProfile> {
    Ok(CredentialProfile {
        id: parse_uuid(row.get("id"))?,
        name: row.get("name"),
        auth_type: row.get("auth_type"),
        username: row.get("username"),
        keychain_ref: row.get("keychain_ref"),
        key_path: row.get("key_path"),
        sort_order: row.get("sort_order"),
    })
}

fn row_to_highlight_profile(row: &PgRow) -> DbResult<HighlightProfile> {
    Ok(HighlightProfile {
        id: parse_uuid(row.get("id"))?,
        name: row.get("name"),
        rules: row.get("rules"),
        sort_order: row.get("sort_order"),
    })
}

fn row_to_login_sequence(row: &PgRow) -> DbResult<LoginSequence> {
    let steps_json: String = row.get("steps");
    let steps: Vec<LoginSequenceStep> = serde_json::from_str(&steps_json)
        .map_err(|e| format!("Failed to parse login sequence steps: {e}"))?;
    Ok(LoginSequence {
        id: parse_uuid(row.get("id"))?,
        name: row.get("name"),
        send_initial_cr: row.get::<i32, _>("send_initial_cr") != 0,
        steps,
        sort_order: row.get("sort_order"),
    })
}

fn row_to_credential(row: &PgRow) -> DbResult<Credential> {
    Ok(Credential {
        id: parse_uuid(row.get("id"))?,
        session_id: parse_uuid(row.get("session_id"))?,
        username: row.get("username"),
        auth_type: row.get("auth_type"),
        keychain_ref: row.get("keychain_ref"),
    })
}

/// The folders whose name matches `pattern`, which is one of
/// [`search_pattern`]'s. RLS limits this to the folders the role can see:
/// its own and the shared ones, as `list_folders`.
async fn folders_matching<'e, E>(executor: E, pattern: &str) -> DbResult<Vec<Folder>>
where
    E: sqlx::PgExecutor<'e>,
{
    let rows = sqlx::query(
        "SELECT id, name, parent_id, sort_order, owner, visibility \
         FROM folders \
         WHERE name ILIKE $1 ESCAPE '\\' \
         ORDER BY sort_order ASC, name ASC",
    )
    .bind(pattern)
    .fetch_all(executor)
    .await
    .map_err(|e| format!("Failed to search folders: {e}"))?;

    rows.iter().map(row_to_folder).collect()
}

/// The sessions that match `pattern`, which is one of [`search_pattern`]'s,
/// in their name, host, user name or tags. RLS limits this to the sessions
/// the role can see.
async fn sessions_matching<'e, E>(executor: E, pattern: &str) -> DbResult<Vec<Session>>
where
    E: sqlx::PgExecutor<'e>,
{
    let rows = sqlx::query(
        "SELECT id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags, icon, sort_order, highlight_profile_id, credential_profile_id, legacy_algorithms, owner, visibility, login_sequence_id \
         FROM sessions \
         WHERE name ILIKE $1 ESCAPE '\\' \
            OR hostname ILIKE $1 ESCAPE '\\' \
            OR username ILIKE $1 ESCAPE '\\' \
            OR tags ILIKE $1 ESCAPE '\\' \
         ORDER BY sort_order ASC, name ASC",
    )
    .bind(pattern)
    .fetch_all(executor)
    .await
    .map_err(|e| format!("Failed to search sessions: {e}"))?;

    rows.iter().map(row_to_session).collect()
}

/// The two statements of a search, in the transaction on `conn` and with
/// the time limit of a search's statements.
async fn search_within(
    conn: &mut sqlx::PgConnection,
    pattern: &str,
) -> DbResult<(Vec<Folder>, Vec<Session>)> {
    limit_statement_time(&mut *conn, SEARCH_STATEMENT_TIMEOUT_MS).await?;
    let folders = folders_matching(&mut *conn, pattern).await?;
    let sessions = sessions_matching(&mut *conn, pattern).await?;
    Ok((folders, sessions))
}

/// `ids`, the ones that read as IDs.
fn parse_ids(ids: &[String]) -> Vec<Uuid> {
    ids.iter()
        .filter_map(|id| Uuid::parse_str(id).ok())
        .collect()
}

/// Lock the role's own sessions in `folder`, so that two reorders or sorts
/// of it do not interleave. Only the role's own: those are the rows it
/// writes, and waiting for a lock on another user's would let whoever holds
/// it keep this user from ordering a folder of their own.
///
/// `FOR NO KEY UPDATE`, which is what the `UPDATE` of `sort_order` takes
/// anyway: `FOR UPDATE` would also wait for the key share lock of a foreign
/// key, which another user holds on a session of this user's while their
/// transaction that wrote a mapping for it is open (§4.5.3). By `id`, so
/// that two of these lock the rows in the same order.
async fn lock_own_sessions(conn: &mut sqlx::PgConnection, folder: &str) -> DbResult<()> {
    sqlx::query(
        "SELECT id FROM sessions \
         WHERE folder_id = $1 AND owner = current_user \
         ORDER BY id FOR NO KEY UPDATE",
    )
    .bind(folder)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| format!("Failed to lock sessions for reorder: {e}"))?;
    Ok(())
}

/// Lock the role's own folders in `parent` (`None`: the top level), as
/// [`lock_own_sessions`] does sessions.
async fn lock_own_folders(conn: &mut sqlx::PgConnection, parent: Option<&str>) -> DbResult<()> {
    sqlx::query(
        "SELECT id FROM folders \
         WHERE parent_id IS NOT DISTINCT FROM $1 AND owner = current_user \
         ORDER BY id FOR NO KEY UPDATE",
    )
    .bind(parent)
    .fetch_all(&mut *conn)
    .await
    .map_err(|e| format!("Failed to lock folders for reorder: {e}"))?;
    Ok(())
}

/// The statement of [`number_own_folders`]: one statement for the whole
/// list, whose rows of other users' match no row it may update. A place
/// lists whatever other users share there, and a statement for each ID
/// would let them make the user's sort as many round trips long, with the
/// user's own rows locked.
const NUMBER_OWN_FOLDERS: &str = "UPDATE folders SET sort_order = (o.place - 1)::int \
     FROM unnest($1::text[]) WITH ORDINALITY AS o(id, place) \
     WHERE folders.id = o.id \
     AND folders.parent_id IS NOT DISTINCT FROM $2 \
     AND folders.owner = current_user";

/// As [`NUMBER_OWN_FOLDERS`], for sessions.
const NUMBER_OWN_SESSIONS: &str = "UPDATE sessions SET sort_order = (o.place - 1)::int \
     FROM unnest($1::text[]) WITH ORDINALITY AS o(id, place) \
     WHERE sessions.id = o.id \
     AND sessions.folder_id = $2 \
     AND sessions.owner = current_user";

/// Number the role's own folders in `parent` by their place in `ids`, as
/// [`number_own_sessions`] does sessions.
async fn number_own_folders(
    conn: &mut sqlx::PgConnection,
    parent: Option<&str>,
    ids: &[String],
) -> DbResult<()> {
    sqlx::query(NUMBER_OWN_FOLDERS)
        .bind(first_of_each(ids))
        .bind(parent)
        .execute(&mut *conn)
        .await
        .map_err(|e| format!("Failed to reorder folders: {e}"))?;
    Ok(())
}

/// Number the role's own sessions in `folder` by their place in `ids`, the
/// sessions there in their new order. Another user's are left as they are:
/// a row is its owner's alone to change.
async fn number_own_sessions(
    conn: &mut sqlx::PgConnection,
    folder: &str,
    ids: &[String],
) -> DbResult<()> {
    sqlx::query(NUMBER_OWN_SESSIONS)
        .bind(first_of_each(ids))
        .bind(folder)
        .execute(&mut *conn)
        .await
        .map_err(|e| format!("Failed to reorder sessions: {e}"))?;
    Ok(())
}

/// `ids` with each one where it comes first. A row is numbered by its place
/// in the list, so the list gives each row one place.
fn first_of_each(ids: &[String]) -> Vec<&str> {
    let mut seen = std::collections::HashSet::new();
    ids.iter()
        .map(String::as_str)
        .filter(|id| seen.insert(*id))
        .collect()
}

#[async_trait]
impl DatabaseProvider for PostgresProvider {
    // ── Folders ──────────────────────────────────────────────────────────

    async fn create_folder(&self, folder: NewFolder) -> DbResult<Folder> {
        let id = Uuid::new_v4();
        let id_str = id.to_string();
        let parent_str = folder.parent_id.map(|u| u.to_string());

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        if let Some(ref target) = parent_str {
            if sqlx::query(
                "SELECT 1 FROM folders WHERE id = $1 AND owner = current_user FOR NO KEY UPDATE",
            )
            .bind(target)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| format!("Failed to check parent folder ownership: {e}"))?
            .is_none()
            {
                return Err("You can only create sub-folders in your own folders".to_string());
            }
        }

        // Lock sibling rows to prevent concurrent sort_order races.
        // A row lock cannot be combined with aggregates, so we lock first
        // and then compute the max in a separate query.
        //
        // Only the user's own siblings: at the top level the siblings
        // include every other user's shared folders, and waiting for a lock
        // on one of those would let whoever holds it keep this user from
        // creating a folder of their own. Two users who get the same
        // sort_order there are told apart by name.
        sqlx::query(
            "SELECT id FROM folders \
             WHERE parent_id IS NOT DISTINCT FROM $1 AND owner = current_user ORDER BY id FOR NO KEY UPDATE",
        )
        .bind(&parent_str)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("Failed to lock sibling folders: {e}"))?;

        let sort_order: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM folders \
             WHERE parent_id IS NOT DISTINCT FROM $1",
        )
        .bind(&parent_str)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        // owner defaults to current_user; visibility defaults to 'personal'.
        let row = sqlx::query(
            "INSERT INTO folders (id, name, parent_id, sort_order, owner, visibility) \
             VALUES ($1, $2, $3, $4, current_user, 'personal') \
             RETURNING owner, visibility",
        )
        .bind(&id_str)
        .bind(&folder.name)
        .bind(&parent_str)
        .bind(sort_order)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("Failed to create folder: {e}"))?;

        let owner: String = row.get("owner");
        let visibility: String = row.get("visibility");

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;

        Ok(Folder {
            id,
            name: folder.name,
            parent_id: folder.parent_id,
            sort_order,
            owner,
            visibility,
        })
    }

    async fn list_folders(&self) -> DbResult<Vec<Folder>> {
        let rows = sqlx::query(
            "SELECT id, name, parent_id, sort_order, owner, visibility \
             FROM folders ORDER BY sort_order ASC, name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list folders: {e}"))?;

        rows.iter().map(row_to_folder).collect()
    }

    async fn folder_owners_in(&self, parent: Option<Uuid>) -> DbResult<Vec<(Uuid, String)>> {
        let rows =
            sqlx::query("SELECT id, owner FROM folders WHERE parent_id IS NOT DISTINCT FROM $1")
                .bind(parent.map(|id| id.to_string()))
                .fetch_all(&self.pool)
                .await
                .map_err(|e| format!("Failed to list folders: {e}"))?;
        rows.iter()
            .map(|row| Ok((parse_uuid(row.get("id"))?, row.get("owner"))))
            .collect()
    }

    async fn session_owners_in(&self, folder: Uuid) -> DbResult<Vec<(Uuid, String)>> {
        let rows = sqlx::query("SELECT id, owner FROM sessions WHERE folder_id = $1")
            .bind(folder.to_string())
            .fetch_all(&self.pool)
            .await
            .map_err(|e| format!("Failed to list sessions: {e}"))?;
        rows.iter()
            .map(|row| Ok((parse_uuid(row.get("id"))?, row.get("owner"))))
            .collect()
    }

    async fn search_folders(&self, query: &str) -> DbResult<Vec<Folder>> {
        let Some(pattern) = search_pattern(query) else {
            return Ok(Vec::new());
        };
        folders_matching(&self.pool, &pattern).await
    }

    /// One transaction for the two statements, for the time limit a
    /// search's statements have ([`SEARCH_STATEMENT_TIMEOUT_MS`]), which
    /// ends with it. They are still two reads, each of the database as it is
    /// when it runs, and they hold one of the pool's connections between
    /// them. The transaction also keeps the lock that the read of `folders`
    /// takes until its end, so through the read of `sessions`, where each
    /// statement by itself lets its lock go when it is done.
    ///
    /// A statement that waits for a lock another session holds gives up
    /// before the limit as a rule, at the session's `lock_timeout`, which is
    /// shorter unless the server sets a longer one ([`SESSION_TIMEOUTS`]).
    /// The transaction's begin and end and the two statements that read and
    /// set the limit have the limit the app's other statements have.
    async fn search(&self, query: &str) -> DbResult<(Vec<Folder>, Vec<Session>)> {
        let Some(pattern) = search_pattern(query) else {
            return Ok((Vec::new(), Vec::new()));
        };
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin the search: {e}"))?;
        let found = search_within(&mut tx, &pattern).await;
        // Ended now, and not when the connection is next used: a statement
        // that ran into its limit leaves the transaction open on the
        // server. The transaction wrote nothing, so an end that fails takes
        // nothing from a search that found: what it read is whole. sqlx
        // then rolls the transaction back as the connection goes back to
        // the pool, which drops a connection that is gone.
        let ended = if found.is_ok() {
            tx.commit().await
        } else {
            tx.rollback().await
        };
        if let Err(e) = ended {
            tracing::warn!("The search's transaction was not ended: {e}");
        }
        found
    }

    async fn rename_folder(&self, id: Uuid, name: &str) -> DbResult<()> {
        let result =
            sqlx::query("UPDATE folders SET name = $1 WHERE id = $2 AND owner = current_user")
                .bind(name)
                .bind(id.to_string())
                .execute(&self.pool)
                .await
                .map_err(|e| format!("Failed to rename folder: {e}"))?;

        if result.rows_affected() == 0 {
            return Err("You can only rename folders you own".to_string());
        }
        Ok(())
    }

    async fn move_folder(&self, id: Uuid, new_parent_id: Option<Uuid>) -> DbResult<()> {
        let id_str = id.to_string();
        let parent_str = new_parent_id.map(|u| u.to_string());

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        if sqlx::query(
            "SELECT 1 FROM folders WHERE id = $1 AND owner = current_user FOR NO KEY UPDATE",
        )
        .bind(&id_str)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| format!("Failed to check folder ownership: {e}"))?
        .is_none()
        {
            return Err("You can only move folders you own".to_string());
        }

        if let Some(ref target) = parent_str {
            if sqlx::query(
                "SELECT 1 FROM folders WHERE id = $1 AND owner = current_user FOR NO KEY UPDATE",
            )
            .bind(target)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| format!("Failed to check target folder ownership: {e}"))?
            .is_none()
            {
                return Err("You can only move items into your own folders".to_string());
            }
        }

        // Lock the user's own siblings in the target parent to prevent
        // sort_order races; see `create_folder` for why only those.
        sqlx::query(
            "SELECT id FROM folders \
             WHERE parent_id IS NOT DISTINCT FROM $1 AND owner = current_user ORDER BY id FOR NO KEY UPDATE",
        )
        .bind(&parent_str)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("Failed to lock sibling folders: {e}"))?;

        let sort_order: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM folders \
             WHERE parent_id IS NOT DISTINCT FROM $1",
        )
        .bind(&parent_str)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        let result =
            sqlx::query("UPDATE folders SET parent_id = $1, sort_order = $2 WHERE id = $3")
                .bind(&parent_str)
                .bind(sort_order)
                .bind(&id_str)
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("Failed to move folder: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Folder {id} not found"));
        }

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
        Ok(())
    }

    // RLS enforces access: owner_all + shared_delete (owner only).
    async fn delete_folder(&self, id: Uuid) -> DbResult<()> {
        let result = sqlx::query("DELETE FROM folders WHERE id = $1")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete folder: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Folder {id} not found"));
        }
        Ok(())
    }

    // ── Sessions ─────────────────────────────────────────────────────────

    async fn create_session(&self, session: NewSession) -> DbResult<Session> {
        let id = Uuid::new_v4();
        let id_str = id.to_string();
        let folder_str = session.folder_id.to_string();
        let jump_str = session.jump_host_id.map(|u| u.to_string());

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        if sqlx::query(
            "SELECT 1 FROM folders WHERE id = $1 AND owner = current_user FOR NO KEY UPDATE",
        )
        .bind(&folder_str)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| format!("Failed to check target folder ownership: {e}"))?
        .is_none()
        {
            return Err("You can only create sessions in your own folders".to_string());
        }

        if let Some(jump_host) = session.jump_host_id {
            check_own_jump_host(&mut *tx, jump_host, None).await?;
        }

        // Lock the user's own sibling sessions to prevent concurrent
        // sort_order races; see `create_folder` for why only those.
        sqlx::query(
            "SELECT id FROM sessions WHERE folder_id = $1 AND owner = current_user ORDER BY id FOR NO KEY UPDATE",
        )
        .bind(&folder_str)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("Failed to lock sibling sessions: {e}"))?;

        let sort_order: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM sessions WHERE folder_id = $1",
        )
        .bind(&folder_str)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        let row = sqlx::query(
            "INSERT INTO sessions \
                (id, folder_id, name, hostname, port, protocol, username, \
                 auth_method, jump_host_id, tags, icon, sort_order, \
                 highlight_profile_id, credential_profile_id, legacy_algorithms, \
                 login_sequence_id, owner, visibility) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16, \
                     current_user, 'personal') \
             RETURNING owner, visibility",
        )
        .bind(&id_str)
        .bind(&folder_str)
        .bind(&session.name)
        .bind(&session.hostname)
        .bind(session.port)
        .bind(&session.protocol)
        .bind(&session.username)
        .bind(&session.auth_method)
        .bind(&jump_str)
        .bind(&session.tags)
        .bind(&session.icon)
        .bind(sort_order)
        .bind(session.highlight_profile_id.map(|u| u.to_string()))
        .bind(session.credential_profile_id.map(|u| u.to_string()))
        .bind(i32::from(session.legacy_algorithms))
        .bind(session.login_sequence_id.map(|u| u.to_string()))
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("Failed to create session: {e}"))?;

        let owner: String = row.get("owner");
        let visibility: String = row.get("visibility");

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;

        Ok(Session {
            id,
            folder_id: session.folder_id,
            name: session.name,
            hostname: session.hostname,
            port: session.port,
            protocol: session.protocol,
            username: session.username,
            auth_method: session.auth_method,
            jump_host_id: session.jump_host_id,
            tags: session.tags,
            icon: session.icon,
            sort_order,
            highlight_profile_id: session.highlight_profile_id,
            credential_profile_id: session.credential_profile_id,
            legacy_algorithms: session.legacy_algorithms,
            login_sequence_id: session.login_sequence_id,
            owner,
            visibility,
        })
    }

    async fn get_session(&self, id: Uuid) -> DbResult<Option<Session>> {
        let row = sqlx::query(
            "SELECT id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags, icon, sort_order, highlight_profile_id, credential_profile_id, legacy_algorithms, owner, visibility, login_sequence_id \
             FROM sessions WHERE id = $1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("Failed to get session: {e}"))?;

        match row {
            Some(r) => Ok(Some(row_to_session(&r)?)),
            None => Ok(None),
        }
    }

    async fn list_all_sessions(&self) -> DbResult<Vec<Session>> {
        let rows = sqlx::query(
            "SELECT id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags, icon, sort_order, highlight_profile_id, credential_profile_id, legacy_algorithms, owner, visibility, login_sequence_id \
             FROM sessions ORDER BY sort_order ASC, name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list sessions: {e}"))?;

        rows.iter().map(row_to_session).collect()
    }

    // RLS enforces access: only its owner updates a row (docs/DESIGN.md §4.5).
    async fn update_session(&self, id: Uuid, update: UpdateSession) -> DbResult<()> {
        let mut b = UpdateBuilder::new(Dialect::Postgres);
        b.set_text(SessionColumn::Name, update.name.as_ref());
        b.set_text(SessionColumn::Hostname, update.hostname.as_ref());
        b.set_int(SessionColumn::Port, update.port);
        b.set_text(SessionColumn::Protocol, update.protocol.as_ref());
        b.set_text(SessionColumn::Username, update.username.as_ref());
        b.set_text(SessionColumn::AuthMethod, update.auth_method.as_ref());
        b.set_optional_uuid(SessionColumn::JumpHostId, update.jump_host_id.as_ref());
        b.set_text(SessionColumn::Tags, update.tags.as_ref());
        b.set_text(SessionColumn::Icon, update.icon.as_ref());
        b.set_optional_uuid(
            SessionColumn::HighlightProfileId,
            update.highlight_profile_id.as_ref(),
        );
        b.set_optional_uuid(
            SessionColumn::CredentialProfileId,
            update.credential_profile_id.as_ref(),
        );
        b.set_bool_as_int(SessionColumn::LegacyAlgorithms, update.legacy_algorithms);
        b.set_optional_uuid(
            SessionColumn::LoginSequenceId,
            update.login_sequence_id.as_ref(),
        );

        if b.is_empty() {
            return Ok(());
        }

        if let Some(Some(jump_host)) = update.jump_host_id {
            check_own_jump_host(&self.pool, jump_host, Some(id)).await?;
        }

        // RLS: a non-owner attempting to UPDATE will match zero rows, which
        // the caller surfaces as "Session {id} not found" — same shape as a
        // genuinely missing row, no enumeration channel.
        let sql = format!(
            "UPDATE sessions SET {} WHERE id = {} AND owner = current_user",
            b.set_clause(),
            b.where_placeholder(),
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
        for val in b.values() {
            query = match val {
                BindVal::Text(s) => query.bind(s.as_str()),
                BindVal::Int(n) => query.bind(*n),
                BindVal::Null => query.bind(None::<String>),
            };
        }
        query = query.bind(id.to_string());

        let result = query
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to update session: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Session {id} not found"));
        }
        Ok(())
    }

    async fn move_session(&self, id: Uuid, new_folder_id: Uuid) -> DbResult<()> {
        let id_str = id.to_string();
        let folder_str = new_folder_id.to_string();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        if sqlx::query(
            "SELECT 1 FROM sessions WHERE id = $1 AND owner = current_user FOR NO KEY UPDATE",
        )
        .bind(&id_str)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| format!("Failed to check session ownership: {e}"))?
        .is_none()
        {
            return Err("You can only move sessions you own".to_string());
        }

        if sqlx::query(
            "SELECT 1 FROM folders WHERE id = $1 AND owner = current_user FOR NO KEY UPDATE",
        )
        .bind(&folder_str)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| format!("Failed to check target folder ownership: {e}"))?
        .is_none()
        {
            return Err("You can only move items into your own folders".to_string());
        }

        // Lock the user's own sibling sessions in the target folder to
        // prevent sort_order races; see `create_folder` for why only those.
        sqlx::query(
            "SELECT id FROM sessions WHERE folder_id = $1 AND owner = current_user ORDER BY id FOR NO KEY UPDATE",
        )
        .bind(&folder_str)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("Failed to lock sibling sessions: {e}"))?;

        let sort_order: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM sessions WHERE folder_id = $1",
        )
        .bind(&folder_str)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        let result =
            sqlx::query("UPDATE sessions SET folder_id = $1, sort_order = $2 WHERE id = $3")
                .bind(&folder_str)
                .bind(sort_order)
                .bind(&id_str)
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("Failed to move session: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Session {id} not found"));
        }

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
        Ok(())
    }

    // RLS enforces access: owner_all + shared_delete (owner only).
    async fn delete_session(&self, id: Uuid) -> DbResult<()> {
        let result = sqlx::query("DELETE FROM sessions WHERE id = $1")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete session: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Session {id} not found"));
        }
        Ok(())
    }

    async fn search_sessions(&self, query: &str) -> DbResult<Vec<Session>> {
        let Some(pattern) = search_pattern(query) else {
            return Ok(Vec::new());
        };
        sessions_matching(&self.pool, &pattern).await
    }

    // ── Reordering ─────────────────────────────────────────────────────

    // RLS enforces access: only its owner updates a row (docs/DESIGN.md §4.5).
    async fn reorder_folders(
        &self,
        parent_id: Option<Uuid>,
        ordered_ids: Vec<Uuid>,
    ) -> DbResult<()> {
        let parent_str = parent_id.map(|u| u.to_string());

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        lock_own_folders(&mut tx, parent_str.as_deref()).await?;
        let ids: Vec<String> = ordered_ids.iter().map(Uuid::to_string).collect();
        number_own_folders(&mut tx, parent_str.as_deref(), &ids).await?;

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
        Ok(())
    }

    async fn reorder_sessions(&self, folder_id: Uuid, ordered_ids: Vec<Uuid>) -> DbResult<()> {
        let folder_str = folder_id.to_string();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        lock_own_sessions(&mut tx, &folder_str).await?;
        let ids: Vec<String> = ordered_ids.iter().map(Uuid::to_string).collect();
        number_own_sessions(&mut tx, &folder_str, &ids).await?;

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
        Ok(())
    }

    // A sort is a reorder by the sorted list of everything the place lists
    // that the role can see: its own rows are numbered by their place in it,
    // as `reorder_folders` numbers them, and the list goes back so that
    // where other users' rows come among them is kept as well (`tree_order`).
    async fn sort_folders_alphabetically(&self, parent_id: Option<Uuid>) -> DbResult<Vec<Uuid>> {
        let parent_str = parent_id.map(|u| u.to_string());

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        lock_own_folders(&mut tx, parent_str.as_deref()).await?;
        let rows = sqlx::query(
            "SELECT id FROM folders \
             WHERE parent_id IS NOT DISTINCT FROM $1 \
             ORDER BY name ASC, id ASC",
        )
        .bind(&parent_str)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("Failed to sort folders: {e}"))?;
        let ids: Vec<String> = rows.iter().map(|row| row.get("id")).collect();

        number_own_folders(&mut tx, parent_str.as_deref(), &ids).await?;
        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
        Ok(parse_ids(&ids))
    }

    async fn sort_sessions_alphabetically(&self, folder_id: Uuid) -> DbResult<Vec<Uuid>> {
        let folder_str = folder_id.to_string();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        lock_own_sessions(&mut tx, &folder_str).await?;
        let rows =
            sqlx::query("SELECT id FROM sessions WHERE folder_id = $1 ORDER BY name ASC, id ASC")
                .bind(&folder_str)
                .fetch_all(&mut *tx)
                .await
                .map_err(|e| format!("Failed to sort sessions: {e}"))?;
        let ids: Vec<String> = rows.iter().map(|row| row.get("id")).collect();

        number_own_sessions(&mut tx, &folder_str, &ids).await?;
        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
        Ok(parse_ids(&ids))
    }

    async fn sort_sessions_by_hostname(&self, folder_id: Uuid) -> DbResult<Vec<Uuid>> {
        let folder_str = folder_id.to_string();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        lock_own_sessions(&mut tx, &folder_str).await?;
        // By name first, so that sessions with the same host come in the
        // same order every time.
        let rows = sqlx::query(
            "SELECT id, hostname FROM sessions WHERE folder_id = $1 ORDER BY name ASC, id ASC",
        )
        .bind(&folder_str)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("Failed to sort sessions: {e}"))?;

        let mut pairs: Vec<(String, String)> = rows
            .iter()
            .map(|r| (r.get("id"), r.get("hostname")))
            .collect();
        pairs.sort_by(|a, b| cmp_hostname(&a.1, &b.1));
        let ids: Vec<String> = pairs.into_iter().map(|(id, _)| id).collect();

        number_own_sessions(&mut tx, &folder_str, &ids).await?;
        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
        Ok(parse_ids(&ids))
    }

    // ── Credentials ──────────────────────────────────────────────────────

    async fn upsert_credential(&self, cred: Credential) -> DbResult<()> {
        sqlx::query(
            "INSERT INTO credentials (id, session_id, username, auth_type, keychain_ref) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT(session_id) DO UPDATE SET \
               username = EXCLUDED.username, \
               auth_type = EXCLUDED.auth_type, \
               keychain_ref = EXCLUDED.keychain_ref",
        )
        .bind(cred.id.to_string())
        .bind(cred.session_id.to_string())
        .bind(&cred.username)
        .bind(&cred.auth_type)
        .bind(&cred.keychain_ref)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to upsert credential: {e}"))?;

        Ok(())
    }

    async fn get_credential(&self, session_id: Uuid) -> DbResult<Option<Credential>> {
        let row = sqlx::query(
            "SELECT id, session_id, username, auth_type, keychain_ref FROM credentials WHERE session_id = $1",
        )
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("Failed to get credential: {e}"))?;

        match row {
            Some(r) => Ok(Some(row_to_credential(&r)?)),
            None => Ok(None),
        }
    }

    async fn delete_credential(&self, session_id: Uuid) -> DbResult<()> {
        sqlx::query("DELETE FROM credentials WHERE session_id = $1")
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete credential: {e}"))?;

        Ok(())
    }

    async fn list_all_credentials(&self) -> DbResult<Vec<Credential>> {
        let rows = sqlx::query(
            "SELECT id, session_id, username, auth_type, keychain_ref FROM credentials ORDER BY session_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list credentials: {e}"))?;

        rows.iter().map(row_to_credential).collect()
    }

    // ── Credential Profiles ──────────────────────────────────────────────

    async fn create_credential_profile(
        &self,
        profile: NewCredentialProfile,
    ) -> DbResult<CredentialProfile> {
        let id = Uuid::new_v4();
        let id_str = id.to_string();
        let keychain_ref = format!("credprofile-{id}");

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        sqlx::query("SELECT id FROM credential_profiles FOR UPDATE")
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| format!("Failed to lock credential profiles: {e}"))?;

        let sort_order: i32 =
            sqlx::query_scalar("SELECT COALESCE(MAX(sort_order), -1) + 1 FROM credential_profiles")
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        sqlx::query(
            "INSERT INTO credential_profiles (id, name, auth_type, username, keychain_ref, key_path, sort_order) \
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&id_str)
        .bind(&profile.name)
        .bind(&profile.auth_type)
        .bind(&profile.username)
        .bind(&keychain_ref)
        .bind(&profile.key_path)
        .bind(sort_order)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to create credential profile: {e}"))?;

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;

        Ok(CredentialProfile {
            id,
            name: profile.name,
            auth_type: profile.auth_type,
            username: profile.username,
            keychain_ref,
            key_path: profile.key_path,
            sort_order,
        })
    }

    async fn list_credential_profiles(&self) -> DbResult<Vec<CredentialProfile>> {
        let rows = sqlx::query(
            "SELECT id, name, auth_type, username, keychain_ref, key_path, sort_order \
             FROM credential_profiles ORDER BY sort_order ASC, name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list credential profiles: {e}"))?;

        rows.iter().map(row_to_credential_profile).collect()
    }

    async fn get_credential_profile(&self, id: Uuid) -> DbResult<Option<CredentialProfile>> {
        let row = sqlx::query(
            "SELECT id, name, auth_type, username, keychain_ref, key_path, sort_order \
             FROM credential_profiles WHERE id = $1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("Failed to get credential profile: {e}"))?;

        match row {
            Some(r) => Ok(Some(row_to_credential_profile(&r)?)),
            None => Ok(None),
        }
    }

    async fn update_credential_profile(
        &self,
        id: Uuid,
        update: UpdateCredentialProfile,
    ) -> DbResult<()> {
        let mut sets = Vec::new();
        let mut values: Vec<String> = Vec::new();
        let mut idx: usize = 1;

        if let Some(ref name) = update.name {
            sets.push(format!("name = ${idx}"));
            values.push(name.clone());
            idx += 1;
        }
        if let Some(ref auth_type) = update.auth_type {
            sets.push(format!("auth_type = ${idx}"));
            values.push(auth_type.clone());
            idx += 1;
        }
        if let Some(ref username) = update.username {
            sets.push(format!("username = ${idx}"));
            values.push(username.clone());
            idx += 1;
        }
        if let Some(ref key_path) = update.key_path {
            sets.push(format!("key_path = ${idx}"));
            values.push(key_path.clone());
            idx += 1;
        }

        if sets.is_empty() {
            return Ok(());
        }

        let sql = format!(
            "UPDATE credential_profiles SET {} WHERE id = ${idx}",
            sets.join(", ")
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
        for val in &values {
            query = query.bind(val.as_str());
        }
        query = query.bind(id.to_string());

        let result = query
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to update credential profile: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Credential profile {id} not found"));
        }
        Ok(())
    }

    async fn delete_credential_profile(&self, id: Uuid) -> DbResult<()> {
        let result = sqlx::query("DELETE FROM credential_profiles WHERE id = $1")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete credential profile: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Credential profile {id} not found"));
        }
        Ok(())
    }

    async fn bulk_set_session_credential_profile(
        &self,
        folder_id: Uuid,
        profile_id: Option<Uuid>,
    ) -> DbResult<u32> {
        // The walk down the folders is a `UNION`, not a `UNION ALL`, here
        // and in the other statements that go down a folder: a folder the
        // walk has been through is not gone through again, so it ends on
        // folders that name each other as their parent.
        let result = sqlx::query(
            "WITH RECURSIVE subtree(id) AS ( \
                 SELECT id FROM folders WHERE id = $2 AND owner = current_user \
                 UNION \
                 SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id \
                     WHERE f.owner = current_user \
             ) \
             UPDATE sessions SET credential_profile_id = $1 \
             WHERE folder_id IN (SELECT id FROM subtree) \
                 AND owner = current_user AND protocol != 'telnet'",
        )
        .bind(profile_id.map(|u| u.to_string()))
        .bind(folder_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to bulk set credential profile: {e}"))?;

        Ok(result.rows_affected() as u32)
    }

    async fn bulk_edit_sessions(&self, folder_id: Uuid, edit: BulkSessionEdit) -> DbResult<u32> {
        if edit.jump_host_id.is_none()
            && edit.highlight_profile_id.is_none()
            && edit.icon.is_none()
            && edit.login_sequence_id.is_none()
        {
            return Ok(0);
        }

        // Before either pass, so that a refused jump host changes nothing.
        if let Some(Some(jump_host)) = edit.jump_host_id {
            check_own_jump_host(&self.pool, jump_host, None).await?;
        }

        let subtree_cte = "WITH RECURSIVE subtree(id) AS ( \
            SELECT id FROM folders WHERE id = $1 AND owner = current_user \
            UNION \
            SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id \
                WHERE f.owner = current_user \
        )";

        let mut total: u64 = 0;

        // First pass: highlight_profile_id / icon applied to all rows
        // (SSH + telnet) in the subtree. Positional params start at $2
        // because $1 is the folder_id bound into the recursive CTE.
        let mut setters: Vec<String> = Vec::new();
        if edit.highlight_profile_id.is_some() {
            setters.push(format!("highlight_profile_id = ${}", setters.len() + 2));
        }
        if edit.icon.is_some() {
            setters.push(format!("icon = ${}", setters.len() + 2));
        }
        if edit.login_sequence_id.is_some() {
            setters.push(format!("login_sequence_id = ${}", setters.len() + 2));
        }

        if !setters.is_empty() {
            let sql = format!(
                "{subtree_cte} \
                 UPDATE sessions SET {} \
                 WHERE folder_id IN (SELECT id FROM subtree) AND owner = current_user",
                setters.join(", "),
            );
            let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(folder_id.to_string());
            if let Some(hp) = &edit.highlight_profile_id {
                q = q.bind(hp.map(|u| u.to_string()));
            }
            if let Some(icon) = &edit.icon {
                q = q.bind(icon);
            }
            if let Some(ls) = &edit.login_sequence_id {
                q = q.bind(ls.map(|u| u.to_string()));
            }
            let result = q
                .execute(&self.pool)
                .await
                .map_err(|e| format!("Failed to bulk edit sessions: {e}"))?;
            total = result.rows_affected();
        }

        // Second pass: jump_host_id — SSH only.
        if let Some(jh) = &edit.jump_host_id {
            let sql = format!(
                "{subtree_cte} \
                 UPDATE sessions SET jump_host_id = $2 \
                 WHERE folder_id IN (SELECT id FROM subtree) \
                     AND owner = current_user AND protocol != 'telnet'",
            );
            let result = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()))
                .bind(folder_id.to_string())
                .bind(jh.map(|u| u.to_string()))
                .execute(&self.pool)
                .await
                .map_err(|e| format!("Failed to bulk set jump host: {e}"))?;
            // See sqlite.rs: report distinct sessions touched, not the
            // sum of both passes (they overlap on SSH rows).
            if setters.is_empty() {
                total = result.rows_affected();
            }
        }

        Ok(total as u32)
    }

    // ── Highlight Profiles ────────────────────────────────────────────────

    async fn create_highlight_profile(
        &self,
        profile: NewHighlightProfile,
    ) -> DbResult<HighlightProfile> {
        let id = Uuid::new_v4();
        let id_str = id.to_string();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        sqlx::query("SELECT id FROM highlight_profiles FOR UPDATE")
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| format!("Failed to lock highlight profiles: {e}"))?;

        let sort_order: i32 =
            sqlx::query_scalar("SELECT COALESCE(MAX(sort_order), -1) + 1 FROM highlight_profiles")
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        sqlx::query(
            "INSERT INTO highlight_profiles (id, name, rules, sort_order) VALUES ($1, $2, $3, $4)",
        )
        .bind(&id_str)
        .bind(&profile.name)
        .bind(&profile.rules)
        .bind(sort_order)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to create highlight profile: {e}"))?;

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;

        Ok(HighlightProfile {
            id,
            name: profile.name,
            rules: profile.rules,
            sort_order,
        })
    }

    async fn list_highlight_profiles(&self) -> DbResult<Vec<HighlightProfile>> {
        let rows = sqlx::query(
            "SELECT id, name, rules, sort_order FROM highlight_profiles ORDER BY sort_order ASC, name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list highlight profiles: {e}"))?;

        rows.iter().map(row_to_highlight_profile).collect()
    }

    async fn get_highlight_profile(&self, id: Uuid) -> DbResult<Option<HighlightProfile>> {
        let row =
            sqlx::query("SELECT id, name, rules, sort_order FROM highlight_profiles WHERE id = $1")
                .bind(id.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| format!("Failed to get highlight profile: {e}"))?;

        match row {
            Some(r) => Ok(Some(row_to_highlight_profile(&r)?)),
            None => Ok(None),
        }
    }

    async fn update_highlight_profile(
        &self,
        id: Uuid,
        update: UpdateHighlightProfile,
    ) -> DbResult<()> {
        let mut sets = Vec::new();
        let mut values: Vec<String> = Vec::new();
        let mut idx: usize = 1;

        if let Some(ref name) = update.name {
            sets.push(format!("name = ${idx}"));
            values.push(name.clone());
            idx += 1;
        }
        if let Some(ref rules) = update.rules {
            sets.push(format!("rules = ${idx}"));
            values.push(rules.clone());
            idx += 1;
        }

        if sets.is_empty() {
            return Ok(());
        }

        let sql = format!(
            "UPDATE highlight_profiles SET {} WHERE id = ${idx}",
            sets.join(", ")
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
        for val in &values {
            query = query.bind(val.as_str());
        }
        query = query.bind(id.to_string());

        let result = query
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to update highlight profile: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Highlight profile {id} not found"));
        }
        Ok(())
    }

    async fn delete_highlight_profile(&self, id: Uuid) -> DbResult<()> {
        let result = sqlx::query("DELETE FROM highlight_profiles WHERE id = $1")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete highlight profile: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Highlight profile {id} not found"));
        }
        Ok(())
    }

    // ── Login Sequences ──────────────────────────────────────────────────

    async fn create_login_sequence(&self, sequence: NewLoginSequence) -> DbResult<LoginSequence> {
        let id = Uuid::new_v4();
        let id_str = id.to_string();
        let steps_json = serde_json::to_string(&sequence.steps)
            .map_err(|e| format!("Failed to serialise login sequence steps: {e}"))?;

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        sqlx::query("SELECT id FROM login_sequences FOR UPDATE")
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| format!("Failed to lock login sequences: {e}"))?;

        let sort_order: i32 =
            sqlx::query_scalar("SELECT COALESCE(MAX(sort_order), -1) + 1 FROM login_sequences")
                .fetch_one(&mut *tx)
                .await
                .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        sqlx::query(
            "INSERT INTO login_sequences (id, name, send_initial_cr, steps, sort_order) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(&id_str)
        .bind(&sequence.name)
        .bind(i32::from(sequence.send_initial_cr))
        .bind(&steps_json)
        .bind(sort_order)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to create login sequence: {e}"))?;

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;

        Ok(LoginSequence {
            id,
            name: sequence.name,
            send_initial_cr: sequence.send_initial_cr,
            steps: sequence.steps,
            sort_order,
        })
    }

    async fn list_login_sequences(&self) -> DbResult<Vec<LoginSequence>> {
        let rows = sqlx::query(
            "SELECT id, name, send_initial_cr, steps, sort_order \
             FROM login_sequences ORDER BY sort_order ASC, name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list login sequences: {e}"))?;

        rows.iter().map(row_to_login_sequence).collect()
    }

    async fn get_login_sequence(&self, id: Uuid) -> DbResult<Option<LoginSequence>> {
        let row = sqlx::query(
            "SELECT id, name, send_initial_cr, steps, sort_order \
             FROM login_sequences WHERE id = $1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("Failed to get login sequence: {e}"))?;

        match row {
            Some(r) => Ok(Some(row_to_login_sequence(&r)?)),
            None => Ok(None),
        }
    }

    async fn update_login_sequence(&self, id: Uuid, update: UpdateLoginSequence) -> DbResult<()> {
        enum BindVal {
            Text(String),
            Int(i32),
        }
        let mut sets = Vec::new();
        let mut values: Vec<BindVal> = Vec::new();
        let mut idx: usize = 1;

        if let Some(ref name) = update.name {
            sets.push(format!("name = ${idx}"));
            values.push(BindVal::Text(name.clone()));
            idx += 1;
        }
        if let Some(send_initial_cr) = update.send_initial_cr {
            sets.push(format!("send_initial_cr = ${idx}"));
            values.push(BindVal::Int(i32::from(send_initial_cr)));
            idx += 1;
        }
        if let Some(ref steps) = update.steps {
            let json = serde_json::to_string(steps)
                .map_err(|e| format!("Failed to serialise login sequence steps: {e}"))?;
            sets.push(format!("steps = ${idx}"));
            values.push(BindVal::Text(json));
            idx += 1;
        }

        if sets.is_empty() {
            return Ok(());
        }

        let sql = format!(
            "UPDATE login_sequences SET {} WHERE id = ${idx}",
            sets.join(", ")
        );
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.as_str()));
        for val in &values {
            match val {
                BindVal::Text(s) => query = query.bind(s.as_str()),
                BindVal::Int(n) => query = query.bind(*n),
            }
        }
        query = query.bind(id.to_string());

        let result = query
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to update login sequence: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Login sequence {id} not found"));
        }
        Ok(())
    }

    async fn delete_login_sequence(&self, id: Uuid) -> DbResult<()> {
        let result = sqlx::query("DELETE FROM login_sequences WHERE id = $1")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete login sequence: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Login sequence {id} not found"));
        }
        Ok(())
    }

    async fn bulk_set_session_login_sequence(
        &self,
        folder_id: Uuid,
        sequence_id: Option<Uuid>,
    ) -> DbResult<u32> {
        let result = sqlx::query(
            "WITH RECURSIVE subtree(id) AS ( \
                 SELECT id FROM folders WHERE id = $2 AND owner = current_user \
                 UNION \
                 SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id \
                     WHERE f.owner = current_user \
             ) \
             UPDATE sessions SET login_sequence_id = $1 \
             WHERE folder_id IN (SELECT id FROM subtree) AND owner = current_user",
        )
        .bind(sequence_id.map(|u| u.to_string()))
        .bind(folder_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to bulk set login sequence: {e}"))?;

        Ok(result.rows_affected() as u32)
    }

    async fn data_fingerprint(&self) -> DbResult<DataFingerprint> {
        let folder_rows =
            sqlx::query("SELECT id, name, sort_order, visibility FROM folders ORDER BY id")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| format!("Failed to fingerprint folders: {e}"))?;
        let session_rows = sqlx::query(
            "SELECT id, name, hostname, port, folder_id, sort_order, visibility \
             FROM sessions ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to fingerprint sessions: {e}"))?;

        let mut hasher = DefaultHasher::new();
        folder_rows.len().hash(&mut hasher);
        for row in &folder_rows {
            let id: String = row.get("id");
            let name: String = row.get("name");
            let sort_order: i32 = row.get("sort_order");
            let visibility: String = row.get("visibility");
            id.hash(&mut hasher);
            name.hash(&mut hasher);
            sort_order.hash(&mut hasher);
            visibility.hash(&mut hasher);
        }
        session_rows.len().hash(&mut hasher);
        for row in &session_rows {
            let id: String = row.get("id");
            let name: String = row.get("name");
            let hostname: String = row.get("hostname");
            let port: i32 = row.get("port");
            let folder_id: String = row.get("folder_id");
            let sort_order: i32 = row.get("sort_order");
            let visibility: String = row.get("visibility");
            id.hash(&mut hasher);
            name.hash(&mut hasher);
            hostname.hash(&mut hasher);
            port.hash(&mut hasher);
            folder_id.hash(&mut hasher);
            sort_order.hash(&mut hasher);
            visibility.hash(&mut hasher);
        }
        Ok(DataFingerprint {
            hash: format!("{:x}", hasher.finish()),
        })
    }
}
