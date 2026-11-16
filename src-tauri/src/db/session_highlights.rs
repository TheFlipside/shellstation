//! Which of the user's own highlight profiles a session is shown with, in
//! `PostgreSQL` mode.
//!
//! A highlight profile is a list of patterns that the window runs over
//! everything a terminal shows. In a shared database every user could change
//! every profile, and so decide what ran in every other user's window. In
//! `PostgreSQL` mode the profiles are therefore each user's own, kept in the
//! local database like credential profiles and login sequences, and so is
//! the choice of a profile for a session: nothing of either is in the shared
//! database, where another role could reach it.
//!
//! The choice is kept here, by session. The two tables are made here and not
//! by a migration: the migrations are shared with `PostgreSQL`, where a
//! migration would put the tables into the shared database as well, unused,
//! and take a start of the role that owns it.
//!
//! In `SQLite` mode none of this is used: a session row names its profile.

use std::collections::{HashMap, HashSet};

use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::models::{Folder, Session};
use super::DbResult;
use crate::highlight::{HighlightRule, MAX_RULES_PER_PROFILE};

/// The flag that says the shared database's profiles have been taken over.
const TAKEN_OVER: &str = "central-highlight-profiles-taken-over";

/// The most profiles that are taken over, as many as an import takes.
const MAX_TAKEN_OVER_PROFILES: i64 = 1_000;

/// The longest rules of a profile that is taken over, in bytes: the size the
/// app lets a profile be saved with. It may have as many rules as the app
/// lets one be saved with, [`MAX_RULES_PER_PROFILE`].
const MAX_TAKEN_OVER_RULES_LEN: i32 = 1_048_576;

/// The longest name of a profile that is taken over, in bytes.
const MAX_TAKEN_OVER_NAME_LEN: i32 = 255;

/// The most that the rules of all profiles taken over come to, in bytes. The
/// shared table is every role's to write, and a start reads it before there
/// is a window: without this a role could have every other user's start
/// read, and keep, a thousand profiles of the longest rules.
const MAX_TAKEN_OVER_BYTES: i64 = 16 * 1_048_576;

/// The longest a statement of the take-over's may take, in milliseconds.
/// The app's own limit for a statement is a minute, and a start waits for
/// the take-over before there is a window: a role that fills the shared
/// table with rows must not get to hold every other user's start up that
/// long. A shared table that takes longer than this to go through is not
/// taken over until an admin has seen to it.
const TAKE_OVER_STATEMENT_TIMEOUT_MS: i64 = 10_000;

/// Tauri managed state: the user's own choices in `PostgreSQL` mode, `None`
/// in `SQLite` mode.
pub struct SessionHighlightState(pub Option<SessionHighlights>);

/// A highlight profile as the shared database holds it.
#[derive(Debug, Clone)]
pub struct CentralProfile {
    pub id: String,
    pub name: String,
    pub rules: String,
}

/// What a take-over brought into the local database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TakenOver {
    /// Profiles copied.
    pub copied: u32,
    /// Profiles that were already there, under their ID or with the same
    /// name and rules.
    pub already_there: u32,
    /// Profiles left out: no ID, no name, rules that are no rule list, or
    /// more rules than a profile saved in the app has.
    pub left_out: u32,
    /// Sessions that now have a profile chosen for them.
    pub sessions: u32,
    /// Profiles of the shared database that were not read: past one of the
    /// limits. They stay where they are.
    pub not_read: u32,
}

/// What a take-over reads from the shared database.
struct Central {
    profiles: Vec<CentralProfile>,
    /// Session ID and profile ID, for the role's own sessions.
    sessions: Vec<(String, String)>,
    not_read: u32,
}

/// The local database's table of the profile chosen for each session, with
/// the take-over of what a shared database held before; see the module.
#[derive(Clone)]
pub struct SessionHighlights {
    pool: SqlitePool,
}

impl SessionHighlights {
    /// Use `pool`, the local database, and make the tables if they are not
    /// there yet.
    pub async fn open(pool: SqlitePool) -> DbResult<Self> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS local_session_highlights ( \
                 session_id TEXT PRIMARY KEY NOT NULL, \
                 highlight_profile_id TEXT NOT NULL \
             )",
        )
        .execute(&pool)
        .await
        .map_err(|e| format!("Failed to create the table of session highlights: {e}"))?;
        sqlx::query("CREATE TABLE IF NOT EXISTS local_flags (name TEXT PRIMARY KEY NOT NULL)")
            .execute(&pool)
            .await
            .map_err(|e| format!("Failed to create the table of local flags: {e}"))?;
        Ok(Self { pool })
    }

    /// Every session with a profile chosen for it. A row that holds no ID is
    /// left out.
    pub async fn all(&self) -> DbResult<HashMap<Uuid, Uuid>> {
        let rows =
            sqlx::query("SELECT session_id, highlight_profile_id FROM local_session_highlights")
                .fetch_all(&self.pool)
                .await
                .map_err(|e| format!("Failed to read session highlights: {e}"))?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let session = Uuid::parse_str(row.get("session_id")).ok()?;
                let profile = Uuid::parse_str(row.get("highlight_profile_id")).ok()?;
                Some((session, profile))
            })
            .collect())
    }

    /// The profile chosen for `session`, if any.
    pub async fn get(&self, session: Uuid) -> DbResult<Option<Uuid>> {
        let profile: Option<String> = sqlx::query_scalar(
            "SELECT highlight_profile_id FROM local_session_highlights WHERE session_id = ?",
        )
        .bind(session.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("Failed to read a session's highlight profile: {e}"))?;
        Ok(profile.and_then(|id| Uuid::parse_str(&id).ok()))
    }

    /// Choose `profile` for each of `sessions`, or none. All of them or, on
    /// an error, none. Returns how many sessions that is.
    pub async fn set(&self, sessions: &[Uuid], profile: Option<Uuid>) -> DbResult<u32> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;
        let profile = profile.map(|id| id.to_string());
        for session in sessions {
            let query = match &profile {
                Some(profile) => sqlx::query(
                    "INSERT INTO local_session_highlights (session_id, highlight_profile_id) \
                     VALUES (?, ?) \
                     ON CONFLICT (session_id) \
                     DO UPDATE SET highlight_profile_id = excluded.highlight_profile_id",
                )
                .bind(session.to_string())
                .bind(profile),
                None => sqlx::query("DELETE FROM local_session_highlights WHERE session_id = ?")
                    .bind(session.to_string()),
            };
            query
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("Failed to save a session's highlight profile: {e}"))?;
        }
        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
        Ok(u32::try_from(sessions.len()).unwrap_or(u32::MAX))
    }

    /// Forget the choice of `profile`, wherever it was made: the profile is
    /// gone. In `SQLite` mode the foreign key of a session row does this.
    pub async fn forget_profile(&self, profile: Uuid) -> DbResult<()> {
        sqlx::query("DELETE FROM local_session_highlights WHERE highlight_profile_id = ?")
            .bind(profile.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to forget a highlight profile's sessions: {e}"))?;
        Ok(())
    }

    /// Give each of `sessions` the profile chosen for it here, in place of
    /// what its row names, which in `PostgreSQL` mode is not the user's
    /// choice. If the choices cannot be read, no session has a profile: what
    /// the rows name is never shown.
    pub async fn overlay(&self, sessions: &mut [Session]) {
        let chosen = match self.all().await {
            Ok(chosen) => chosen,
            Err(e) => {
                tracing::warn!("Sessions are shown without their highlight profiles: {e:?}");
                HashMap::new()
            }
        };
        for session in sessions {
            session.highlight_profile_id = chosen.get(&session.id).copied();
        }
    }

    /// As [`Self::overlay`], for one session.
    pub async fn overlay_one(&self, session: &mut Session) {
        session.highlight_profile_id = match self.get(session.id).await {
            Ok(profile) => profile,
            Err(e) => {
                tracing::warn!("A session is shown without its highlight profile: {e:?}");
                None
            }
        };
    }

    /// Whether the shared database's profiles have been taken over.
    pub async fn taken_over(&self) -> DbResult<bool> {
        let flag: Option<String> =
            sqlx::query_scalar("SELECT name FROM local_flags WHERE name = ?")
                .bind(TAKEN_OVER)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| format!("Failed to read the local flags: {e}"))?;
        Ok(flag.is_some())
    }

    /// Take over the highlight profiles of the shared database, once: read
    /// them and what the role's own sessions name, and bring both into the
    /// local database with [`Self::take_over`]. `None` if that has been done.
    ///
    /// Versions before profiles were each user's own kept them in the shared
    /// database, and a session row named its profile there. Without this an
    /// update would leave every user without their profiles.
    ///
    /// Once is once for this local database, whichever shared database it
    /// was: one that the app is pointed at later is not taken over.
    pub async fn take_over_central(&self, pg: &sqlx::PgPool) -> DbResult<Option<TakenOver>> {
        if self.taken_over().await? {
            return Ok(None);
        }
        let central = read_central(pg).await?;
        let taken = self.take_over(&central.profiles, &central.sessions).await?;
        Ok(taken.map(|taken| TakenOver {
            not_read: central.not_read,
            ..taken
        }))
    }

    /// Bring `profiles` and the choices of `sessions` (session ID, profile
    /// ID) into the local database, unless that has been done before, and
    /// remember that it has. One transaction: all of it, or nothing and no
    /// flag.
    ///
    /// What is there already stays as it is. A profile whose ID is there, or
    /// whose name and rules are (as after a move from `SQLite` to
    /// `PostgreSQL`, which leaves the local profiles behind), is not copied
    /// again, and a session with a choice keeps it.
    ///
    /// The shared table was every user's to write, so what comes from it is
    /// checked: a profile needs an ID, a name and rules that are a rule
    /// list of at most [`MAX_RULES_PER_PROFILE`] rules, as one saved in the
    /// app has. A `SecureCRT` import could store one with more, which is
    /// then left out, for its user to import again: the window runs every
    /// rule over all a terminal shows, and whose rules these are cannot be
    /// told. A profile's name is kept as written, and its patterns are
    /// compiled, and then checked, only where a session uses them; both as
    /// for an imported profile.
    pub async fn take_over(
        &self,
        profiles: &[CentralProfile],
        sessions: &[(String, String)],
    ) -> DbResult<Option<TakenOver>> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        // The flag first: a write, so that of two instances of the app that
        // start together one waits here and then finds it set.
        let first = sqlx::query("INSERT OR IGNORE INTO local_flags (name) VALUES (?)")
            .bind(TAKEN_OVER)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("Failed to set a local flag: {e}"))?
            .rows_affected()
            == 1;
        if !first {
            return Ok(None);
        }

        let mut taken = TakenOver {
            copied: 0,
            already_there: 0,
            left_out: 0,
            sessions: 0,
            not_read: 0,
        };
        let local_of = copy_profiles(&mut tx, profiles, &mut taken).await?;
        taken.sessions = copy_choices(&mut tx, sessions, &local_of).await?;

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
        Ok(Some(taken))
    }
}

/// Copy the `profiles` of a shared database that are not in the local one
/// yet, counting in `taken`. Returns, for each profile of the shared database
/// that now has one, the local profile that stands for it.
async fn copy_profiles(
    conn: &mut sqlx::SqliteConnection,
    profiles: &[CentralProfile],
    taken: &mut TakenOver,
) -> DbResult<HashMap<Uuid, Uuid>> {
    let local = sqlx::query("SELECT id, name, rules, sort_order FROM highlight_profiles")
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| format!("Failed to list highlight profiles: {e}"))?;
    let mut ids: HashSet<Uuid> = HashSet::new();
    let mut by_content: HashMap<(String, String), Uuid> = HashMap::new();
    let mut next_sort_order: i32 = 0;
    for row in &local {
        let order: i32 = row.get("sort_order");
        next_sort_order = next_sort_order.max(order.saturating_add(1));
        let Ok(id) = Uuid::parse_str(row.get("id")) else {
            continue;
        };
        ids.insert(id);
        by_content
            .entry((row.get("name"), row.get("rules")))
            .or_insert(id);
    }

    let mut local_of: HashMap<Uuid, Uuid> = HashMap::new();
    for profile in profiles {
        let Ok(id) = Uuid::parse_str(&profile.id) else {
            taken.left_out += 1;
            continue;
        };
        if local_of.contains_key(&id) {
            continue;
        }
        if ids.contains(&id) {
            local_of.insert(id, id);
            taken.already_there += 1;
            continue;
        }
        let content = (profile.name.clone(), profile.rules.clone());
        if let Some(&same) = by_content.get(&content) {
            local_of.insert(id, same);
            taken.already_there += 1;
            continue;
        }
        let rules_fit = serde_json::from_str::<Vec<HighlightRule>>(&profile.rules)
            .is_ok_and(|rules| rules.len() <= MAX_RULES_PER_PROFILE);
        if profile.name.is_empty() || !rules_fit {
            taken.left_out += 1;
            continue;
        }
        sqlx::query(
            "INSERT INTO highlight_profiles (id, name, rules, sort_order) VALUES (?, ?, ?, ?)",
        )
        .bind(id.to_string())
        .bind(&profile.name)
        .bind(&profile.rules)
        .bind(next_sort_order)
        .execute(&mut *conn)
        .await
        .map_err(|e| format!("Failed to take over a highlight profile: {e}"))?;
        next_sort_order = next_sort_order.saturating_add(1);
        ids.insert(id);
        by_content.insert(content, id);
        local_of.insert(id, id);
        taken.copied += 1;
    }
    Ok(local_of)
}

/// Choose for each of `sessions` (session ID, profile ID) that has no choice
/// yet the local profile that stands for its profile, going by `local_of`.
/// Returns how many sessions got one.
async fn copy_choices(
    conn: &mut sqlx::SqliteConnection,
    sessions: &[(String, String)],
    local_of: &HashMap<Uuid, Uuid>,
) -> DbResult<u32> {
    let mut chosen = 0;
    for (session, profile) in sessions {
        let (Ok(session), Ok(profile)) = (Uuid::parse_str(session), Uuid::parse_str(profile))
        else {
            continue;
        };
        let Some(local) = local_of.get(&profile) else {
            continue;
        };
        let added = sqlx::query(
            "INSERT OR IGNORE INTO local_session_highlights \
                 (session_id, highlight_profile_id) VALUES (?, ?)",
        )
        .bind(session.to_string())
        .bind(local.to_string())
        .execute(&mut *conn)
        .await
        .map_err(|e| format!("Failed to take over a session's highlight profile: {e}"))?
        .rows_affected();
        if added == 1 {
            chosen += 1;
        }
    }
    Ok(chosen)
}

/// What a take-over reads from the shared database: its highlight profiles,
/// and for each of the role's own sessions that names one, the session's ID
/// and the profile's.
///
/// Every role can write the table of profiles, and a start reads it before
/// there is a window, so no more is read than these limits allow. A profile
/// whose name or rules are longer than the app saves is left where it is.
/// Of the others, those come first that a session of the role's own names,
/// which no other role can write, and then the rest in the order the app
/// showed them; they are read up to [`MAX_TAKEN_OVER_PROFILES`] of them and
/// up to [`MAX_TAKEN_OVER_BYTES`] of rules, whichever is reached first. So
/// no one can fill the limits with profiles of their own before those the
/// user's sessions use. What is not read is counted.
///
/// The profiles the role's sessions name are joined in as a list of their
/// own, each once, so that the statement goes through the sessions once and
/// not once for each profile. It compares the lengths of the rules, which
/// the server has without reading them, and reads only the rules it returns.
///
/// Only the role's own sessions give a choice. In another user's shared
/// session the profile was that user's choice, and a profile's patterns run
/// in the window of whoever it is chosen for: the user chooses one for such
/// a session themselves.
///
/// Every comparison is one `pg_catalog` has an operator for with exactly its
/// two types: `integer` with `integer`, `bigint` with `bigint`, `text` with
/// `text`, and `text` with the `name` that `current_user` is.
///
/// The three statements are one transaction, for the time limit that
/// [`limit_statement_time`] sets for it.
async fn read_central(pg: &sqlx::PgPool) -> DbResult<Central> {
    let mut tx = pg
        .begin()
        .await
        .map_err(|e| format!("Failed to begin transaction: {e}"))?;
    limit_statement_time(&mut tx).await?;

    let rows = sqlx::query(
        "SELECT id, name, rules, sort_order FROM ( \
             SELECT p.id, p.name, p.rules, p.sort_order, \
                    pg_catalog.row_number() OVER w AS position, \
                    pg_catalog.sum(pg_catalog.octet_length(p.rules)) OVER w AS bytes \
             FROM highlight_profiles p \
             LEFT JOIN ( \
                 SELECT DISTINCT s.highlight_profile_id AS id FROM sessions s \
                 WHERE s.highlight_profile_id IS NOT NULL AND s.owner = current_user \
             ) AS mine ON mine.id = p.id \
             WHERE pg_catalog.octet_length(p.rules) <= $1 \
                 AND pg_catalog.octet_length(p.name) <= $2 \
             WINDOW w AS (ORDER BY (mine.id IS NULL) ASC, \
                 p.sort_order ASC, p.name ASC, p.id ASC) \
         ) AS ranked \
         WHERE position <= $3 AND bytes <= $4 \
         ORDER BY sort_order ASC, name ASC, id ASC",
    )
    .bind(MAX_TAKEN_OVER_RULES_LEN)
    .bind(MAX_TAKEN_OVER_NAME_LEN)
    .bind(MAX_TAKEN_OVER_PROFILES)
    .bind(MAX_TAKEN_OVER_BYTES)
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| format!("Failed to read the shared highlight profiles: {e}"))?;
    let profiles: Vec<CentralProfile> = rows
        .iter()
        .map(|row| CentralProfile {
            id: row.get("id"),
            name: row.get("name"),
            rules: row.get("rules"),
        })
        .collect();

    let all: i64 = sqlx::query_scalar("SELECT pg_catalog.count(*) FROM highlight_profiles")
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("Failed to count the shared highlight profiles: {e}"))?;
    let read = i64::try_from(profiles.len()).unwrap_or(i64::MAX);
    let not_read = u32::try_from(all.saturating_sub(read).max(0)).unwrap_or(u32::MAX);

    let sessions = sqlx::query(
        "SELECT id, highlight_profile_id FROM sessions \
         WHERE highlight_profile_id IS NOT NULL AND owner = current_user",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| format!("Failed to read the sessions' highlight profiles: {e}"))?
    .iter()
    .map(|row| (row.get("id"), row.get("highlight_profile_id")))
    .collect();

    // Nothing was written; this ends the transaction and its time limit.
    tx.commit()
        .await
        .map_err(|e| format!("Failed to commit: {e}"))?;

    Ok(Central {
        profiles,
        sessions,
        not_read,
    })
}

/// Let no statement of the transaction on `conn` take longer than
/// [`TAKE_OVER_STATEMENT_TIMEOUT_MS`]. A shorter limit that is in effect, the
/// server's or the app's, stays; the limit set here ends with the
/// transaction.
///
/// What is in effect is read as the number of milliseconds that
/// `pg_settings` gives as text, and compared here, so that the statement
/// compares nothing but two `text`s.
async fn limit_statement_time(conn: &mut sqlx::PgConnection) -> DbResult<()> {
    let current: String = sqlx::query_scalar(
        "SELECT setting FROM pg_catalog.pg_settings WHERE name = 'statement_timeout'",
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| format!("Failed to read the statement timeout: {e}"))?;
    // `0` is no limit; so is anything that is no number.
    let current = current.parse::<i64>().unwrap_or(0);
    if current > 0 && current <= TAKE_OVER_STATEMENT_TIMEOUT_MS {
        return Ok(());
    }
    sqlx::query("SELECT pg_catalog.set_config('statement_timeout', $1, true)")
        .bind(TAKE_OVER_STATEMENT_TIMEOUT_MS.to_string())
        .execute(&mut *conn)
        .await
        .map_err(|e| format!("Failed to set the statement timeout: {e}"))?;
    Ok(())
}

/// Of `sessions`, the IDs of those in `folder` and in the folders below it,
/// going by `folders`. A folder that is not among `folders` is not gone
/// into, and no folder twice, so this ends on folders that name each other
/// as their parent.
pub fn sessions_below(folder: Uuid, folders: &[Folder], sessions: &[Session]) -> Vec<Uuid> {
    if !folders.iter().any(|f| f.id == folder) {
        return Vec::new();
    }
    let mut children: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for f in folders {
        if let Some(parent) = f.parent_id {
            children.entry(parent).or_default().push(f.id);
        }
    }
    let mut below = HashSet::from([folder]);
    let mut pending = vec![folder];
    while let Some(current) = pending.pop() {
        for &child in children.get(&current).into_iter().flatten() {
            if below.insert(child) {
                pending.push(child);
            }
        }
    }
    sessions
        .iter()
        .filter(|s| below.contains(&s.folder_id))
        .map(|s| s.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;
    use crate::db::models::NewHighlightProfile;
    use crate::db::sqlite::SqliteProvider;
    use crate::db::DatabaseProvider;

    const RULES: &str =
        r##"[{"pattern":"error","color":"#ff0000","case_sensitive":false,"bold":true}]"##;

    /// A local database as `PostgreSQL` mode has it, with the provider that
    /// keeps the profiles in it.
    async fn local() -> (SessionHighlights, SqliteProvider) {
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

    fn central(id: Uuid, name: &str, rules: &str) -> CentralProfile {
        CentralProfile {
            id: id.to_string(),
            name: name.to_string(),
            rules: rules.to_string(),
        }
    }

    fn session(id: Uuid, folder_id: Uuid, highlight_profile_id: Option<Uuid>) -> Session {
        Session {
            id,
            folder_id,
            name: "session".to_string(),
            hostname: "host.example".to_string(),
            port: 22,
            protocol: "ssh".to_string(),
            username: String::new(),
            auth_method: "profile".to_string(),
            jump_host_id: None,
            tags: "[]".to_string(),
            icon: "desktop".to_string(),
            sort_order: 0,
            highlight_profile_id,
            credential_profile_id: None,
            legacy_algorithms: false,
            login_sequence_id: None,
            owner: "alice".to_string(),
            visibility: "shared".to_string(),
        }
    }

    fn folder(id: Uuid, parent_id: Option<Uuid>) -> Folder {
        Folder {
            id,
            name: "folder".to_string(),
            parent_id,
            sort_order: 0,
            owner: "alice".to_string(),
            visibility: "personal".to_string(),
        }
    }

    #[tokio::test]
    async fn opening_twice_keeps_what_is_there() {
        let (highlights, _) = local().await;
        let (session, profile) = (Uuid::new_v4(), Uuid::new_v4());
        highlights.set(&[session], Some(profile)).await.unwrap();

        let again = SessionHighlights::open(highlights.pool.clone())
            .await
            .unwrap();
        assert_eq!(again.get(session).await.unwrap(), Some(profile));
    }

    #[tokio::test]
    async fn a_choice_is_set_replaced_and_cleared() {
        let (highlights, _) = local().await;
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let (first, second) = (Uuid::new_v4(), Uuid::new_v4());

        assert_eq!(highlights.set(&[a, b], Some(first)).await.unwrap(), 2);
        assert_eq!(highlights.set(&[a], Some(second)).await.unwrap(), 1);
        assert_eq!(
            highlights.all().await.unwrap(),
            HashMap::from([(a, second), (b, first)])
        );

        highlights.set(&[a], None).await.unwrap();
        assert_eq!(highlights.get(a).await.unwrap(), None);
        assert_eq!(highlights.get(b).await.unwrap(), Some(first));
        // Clearing what is not set is no error.
        highlights.set(&[a], None).await.unwrap();
    }

    #[tokio::test]
    async fn a_deleted_profile_is_forgotten_for_every_session() {
        let (highlights, _) = local().await;
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (gone, kept) = (Uuid::new_v4(), Uuid::new_v4());
        highlights.set(&[a, b], Some(gone)).await.unwrap();
        highlights.set(&[c], Some(kept)).await.unwrap();

        highlights.forget_profile(gone).await.unwrap();
        assert_eq!(highlights.all().await.unwrap(), HashMap::from([(c, kept)]));
    }

    #[tokio::test]
    async fn the_overlay_replaces_what_a_row_names() {
        let (highlights, _) = local().await;
        let folder_id = Uuid::new_v4();
        let (mine, theirs) = (Uuid::new_v4(), Uuid::new_v4());
        // Both rows name a profile of the shared database; the user has
        // chosen one of their own for the first and none for the second.
        let mut sessions = vec![
            session(Uuid::new_v4(), folder_id, Some(theirs)),
            session(Uuid::new_v4(), folder_id, Some(theirs)),
        ];
        highlights.set(&[sessions[0].id], Some(mine)).await.unwrap();

        highlights.overlay(&mut sessions).await;
        assert_eq!(sessions[0].highlight_profile_id, Some(mine));
        assert_eq!(sessions[1].highlight_profile_id, None);

        let mut one = session(sessions[0].id, folder_id, Some(theirs));
        highlights.overlay_one(&mut one).await;
        assert_eq!(one.highlight_profile_id, Some(mine));
        let mut other = session(sessions[1].id, folder_id, Some(theirs));
        highlights.overlay_one(&mut other).await;
        assert_eq!(other.highlight_profile_id, None);
    }

    #[tokio::test]
    async fn a_take_over_copies_profiles_and_choices_once() {
        let (highlights, db) = local().await;
        let (cisco, linux) = (Uuid::new_v4(), Uuid::new_v4());
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let profiles = [
            central(cisco, "Cisco", RULES),
            central(linux, "Linux", "[]"),
        ];
        let sessions = [
            (a.to_string(), cisco.to_string()),
            (b.to_string(), linux.to_string()),
        ];

        assert!(!highlights.taken_over().await.unwrap());
        let taken = highlights.take_over(&profiles, &sessions).await.unwrap();
        assert_eq!(
            taken,
            Some(TakenOver {
                copied: 2,
                already_there: 0,
                left_out: 0,
                sessions: 2,
                not_read: 0,
            })
        );
        assert!(highlights.taken_over().await.unwrap());

        let local = db.list_highlight_profiles().await.unwrap();
        assert_eq!(
            local
                .iter()
                .map(|p| (p.id, p.name.as_str(), p.rules.as_str()))
                .collect::<Vec<_>>(),
            vec![(cisco, "Cisco", RULES), (linux, "Linux", "[]")]
        );
        assert_eq!(
            highlights.all().await.unwrap(),
            HashMap::from([(a, cisco), (b, linux)])
        );

        // The shared table stays every user's to write. Nothing that gets
        // there later is taken, and a profile deleted here does not come back.
        db.delete_highlight_profile(linux).await.unwrap();
        highlights.forget_profile(linux).await.unwrap();
        let later = [central(Uuid::new_v4(), "Planted", RULES)];
        let planted = [(a.to_string(), later[0].id.clone())];
        assert_eq!(highlights.take_over(&later, &planted).await.unwrap(), None);
        assert_eq!(
            highlights.take_over(&profiles, &sessions).await.unwrap(),
            None
        );
        assert_eq!(db.list_highlight_profiles().await.unwrap().len(), 1);
        assert_eq!(highlights.all().await.unwrap(), HashMap::from([(a, cisco)]));
    }

    #[tokio::test]
    async fn a_take_over_keeps_what_is_there() {
        let (highlights, db) = local().await;
        // A profile from the user's time in SQLite mode, which a move to
        // PostgreSQL copied into the shared database under a new ID.
        let own = db
            .create_highlight_profile(NewHighlightProfile {
                name: "Cisco".to_string(),
                rules: RULES.to_string(),
            })
            .await
            .unwrap();
        let (copy, other) = (Uuid::new_v4(), Uuid::new_v4());
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        // The user has already chosen for `c`.
        highlights.set(&[c], Some(own.id)).await.unwrap();

        let taken = highlights
            .take_over(
                &[
                    central(copy, "Cisco", RULES),
                    // The same name with other rules is another profile.
                    central(other, "Cisco", "[]"),
                    // The ID that is there already, whatever it holds now.
                    central(own.id, "Changed", "[]"),
                ],
                &[
                    (a.to_string(), copy.to_string()),
                    (b.to_string(), other.to_string()),
                    (c.to_string(), other.to_string()),
                ],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            taken,
            TakenOver {
                copied: 1,
                already_there: 2,
                left_out: 0,
                sessions: 2,
                not_read: 0,
            }
        );

        let local = db.list_highlight_profiles().await.unwrap();
        assert_eq!(
            local
                .iter()
                .map(|p| (p.id, p.name.as_str(), p.rules.as_str(), p.sort_order))
                .collect::<Vec<_>>(),
            vec![(own.id, "Cisco", RULES, 0), (other, "Cisco", "[]", 1)]
        );
        assert_eq!(
            highlights.all().await.unwrap(),
            HashMap::from([(a, own.id), (b, other), (c, own.id)])
        );
    }

    #[tokio::test]
    async fn a_take_over_leaves_out_what_is_no_profile() {
        let (highlights, db) = local().await;
        let (good, no_rules, no_list, no_name) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        // One rule more than a profile saved in the app has, and as many as
        // it has, which is taken.
        let rule = &RULES[1..RULES.len() - 1];
        let rules_of = |count: usize| format!("[{}]", vec![rule; count].join(","));
        let (too_many, most) = (Uuid::new_v4(), Uuid::new_v4());
        let too_many_rules = rules_of(MAX_RULES_PER_PROFILE + 1);
        let most_rules = rules_of(MAX_RULES_PER_PROFILE);

        let taken = highlights
            .take_over(
                &[
                    central(good, "Good", RULES),
                    central(most, "Most", &most_rules),
                    central(no_rules, "Not JSON", "not json"),
                    central(no_list, "No list", r#"{"pattern":"x"}"#),
                    central(no_name, "", RULES),
                    central(too_many, "Too many", &too_many_rules),
                    CentralProfile {
                        id: "not an id".to_string(),
                        name: "No ID".to_string(),
                        rules: RULES.to_string(),
                    },
                    // Listed twice, as no primary key lets it be, counts once.
                    central(good, "Good", RULES),
                ],
                &[
                    (a.to_string(), good.to_string()),
                    // A profile that was left out, one that is not there, and
                    // a row without IDs: no choice comes of any of them.
                    (b.to_string(), no_rules.to_string()),
                    (c.to_string(), Uuid::new_v4().to_string()),
                    ("not an id".to_string(), good.to_string()),
                    (c.to_string(), "not an id".to_string()),
                ],
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            taken,
            TakenOver {
                copied: 2,
                already_there: 0,
                left_out: 5,
                sessions: 1,
                not_read: 0,
            }
        );
        let local = db.list_highlight_profiles().await.unwrap();
        assert_eq!(
            local.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![good, most]
        );
        assert_eq!(highlights.all().await.unwrap(), HashMap::from([(a, good)]));
    }

    #[test]
    fn sessions_below_a_folder() {
        let (top, child, grandchild, elsewhere) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let folders = [
            folder(top, None),
            folder(child, Some(top)),
            folder(grandchild, Some(child)),
            folder(elsewhere, None),
        ];
        let sessions = [
            session(Uuid::new_v4(), top, None),
            session(Uuid::new_v4(), grandchild, None),
            session(Uuid::new_v4(), elsewhere, None),
        ];

        assert_eq!(
            sessions_below(top, &folders, &sessions),
            vec![sessions[0].id, sessions[1].id]
        );
        assert_eq!(
            sessions_below(child, &folders, &sessions),
            vec![sessions[1].id]
        );
        // A folder that is not among those given has nothing below it, also
        // where sessions name it.
        assert_eq!(
            sessions_below(top, &folders[1..], &sessions),
            Vec::<Uuid>::new()
        );
    }

    #[test]
    fn sessions_below_ends_on_folders_that_name_each_other() {
        let (a, b, inside) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let folders = [
            folder(a, Some(b)),
            folder(b, Some(a)),
            folder(inside, Some(b)),
        ];
        let sessions = [
            session(Uuid::new_v4(), a, None),
            session(Uuid::new_v4(), inside, None),
        ];

        let mut below = sessions_below(a, &folders, &sessions);
        below.sort();
        let mut all: Vec<Uuid> = sessions.iter().map(|s| s.id).collect();
        all.sort();
        assert_eq!(below, all);
    }
}
