use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use tauri::{AppHandle, State};
use uuid::Uuid;

use zeroize::Zeroizing;

use tracing::debug;

use crate::db::models::{
    CredentialProfile, DataFingerprint, Folder, NewFolder, NewSession, Session, UpdateSession,
};
use crate::db::session_highlights::{sessions_below, SessionHighlightState, SessionHighlights};
use crate::db::{CredentialDbState, DatabaseProvider, DbState, HighlightDbState, PgUserState};
use crate::login_pins::{needs_confirmation, LoginPinState, UsedAs};
use crate::session_logger::SessionLogState;
use crate::ssh::{establish_ssh_connection, SshConnectParams, SshState};
use crate::telnet::{establish_telnet_connection, TelnetConnectParams, TelnetState};

use super::highlight::require_highlight_profile;
use super::session_credentials::CredentialMappings;
use super::{
    connection, validate_dimensions, validate_port, validate_session_fields, ConnectSetup,
    TerminalReadyState, MAX_JUMP_HOPS,
};

// ── Folder commands ──────────────────────────────────────────────────

#[tauri::command]
pub async fn folder_create(
    state: State<'_, DbState>,
    name: String,
    parent_id: Option<String>,
) -> Result<Folder, String> {
    if name.is_empty() || name.len() > super::MAX_NAME_LEN {
        return Err(format!(
            "Folder name must be 1–{} characters",
            super::MAX_NAME_LEN
        ));
    }
    let parent = parent_id.map(|s| parse_uuid(&s)).transpose()?;
    state
        .0
        .create_folder(NewFolder {
            name,
            parent_id: parent,
        })
        .await
}

#[tauri::command]
pub async fn folder_list(state: State<'_, DbState>) -> Result<Vec<Folder>, String> {
    state.0.list_folders().await
}

#[tauri::command]
pub async fn folder_rename(
    state: State<'_, DbState>,
    id: String,
    name: String,
) -> Result<(), String> {
    if name.is_empty() || name.len() > super::MAX_NAME_LEN {
        return Err(format!(
            "Folder name must be 1–{} characters",
            super::MAX_NAME_LEN
        ));
    }
    state.0.rename_folder(parse_uuid(&id)?, &name).await
}

#[tauri::command]
pub async fn folder_move(
    state: State<'_, DbState>,
    id: String,
    new_parent_id: Option<String>,
) -> Result<(), String> {
    let folder_uuid = parse_uuid(&id)?;
    let parent = new_parent_id.map(|s| parse_uuid(&s)).transpose()?;

    // Reject cycles: a folder cannot be moved into itself or any of its
    // descendants. Without this, a recursive sort would stack-overflow.
    if let Some(target_parent) = parent {
        if target_parent == folder_uuid {
            return Err("A folder cannot be moved into itself.".to_string());
        }
        let all_folders = state.0.list_folders().await?;
        let mut current = Some(target_parent);
        // A ring above the target that this folder is no part of, as a
        // database may have from statements of someone's own, must not keep
        // this from coming to an end. The database then decides about the
        // move.
        let mut seen = HashSet::new();
        while let Some(cur) = current {
            if cur == folder_uuid {
                return Err("A folder cannot be moved into one of its descendants.".to_string());
            }
            if !seen.insert(cur) {
                break;
            }
            current = all_folders
                .iter()
                .find(|f| f.id == cur)
                .and_then(|f| f.parent_id);
        }
    }

    state.0.move_folder(folder_uuid, parent).await
}

#[tauri::command]
pub async fn folder_delete(
    state: State<'_, DbState>,
    highlights: State<'_, SessionHighlightState>,
    id: String,
) -> Result<(), String> {
    let folder = parse_uuid(&id)?;
    // The sessions that go with the folder, for the highlight profiles
    // chosen for them (PostgreSQL mode): looked up before they are gone.
    let gone = match highlights.0.as_ref() {
        Some(_) => visible_sessions_below(&*state.0, folder).await,
        None => Vec::new(),
    };
    state.0.delete_folder(folder).await?;
    if let Some(local) = highlights.0.as_ref() {
        forget_highlights(local, &gone).await;
    }
    Ok(())
}

/// The sessions in `folder` and below it that this user can see, whoever
/// owns them. Empty if they cannot be listed, which is logged: what was
/// chosen for them then stays behind.
async fn visible_sessions_below(db: &dyn DatabaseProvider, folder: Uuid) -> Vec<Uuid> {
    match (db.list_folders().await, db.list_all_sessions().await) {
        (Ok(folders), Ok(sessions)) => sessions_below(folder, &folders, &sessions),
        (Err(e), _) | (_, Err(e)) => {
            tracing::warn!(
                "Could not list the sessions of a folder that is being deleted; highlight profiles chosen for them are not dropped: {e:?}"
            );
            Vec::new()
        }
    }
}

/// Drop the highlight profiles chosen for sessions that have been deleted.
/// The sessions are gone either way, so a failure is only logged: what stays
/// is a row for a session that no longer exists.
async fn forget_highlights(local: &SessionHighlights, sessions: &[Uuid]) {
    if sessions.is_empty() {
        return;
    }
    if let Err(e) = local.set(sessions, None).await {
        tracing::warn!("Could not drop the highlight profiles of deleted sessions: {e:?}");
    }
}

/// Of the sessions in `folder` and below it, those a Bulk Edit covers in
/// `PostgreSQL` mode: the role's own, in a folder of the role's own and in
/// the role's own folders below it, as the statements of
/// `bulk_edit_sessions` have it. None while the role is not known.
async fn own_sessions_below(
    db: &dyn DatabaseProvider,
    folder: Uuid,
    role: Option<&str>,
) -> Result<Vec<Uuid>, String> {
    let Some(role) = role else {
        return Ok(Vec::new());
    };
    let mut folders = db.list_folders().await?;
    folders.retain(|f| f.owner == role);
    let mut sessions = db.list_all_sessions().await?;
    sessions.retain(|s| s.owner == role);
    Ok(sessions_below(folder, &folders, &sessions))
}

/// Bulk-edit sessions in a folder (and its descendants). Each `set_*` flag
/// controls whether its sibling field is applied; when `true`, a `null`
/// value clears the column. This avoids the `Option<Option<T>>` JSON
/// ambiguity.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn folder_bulk_edit_sessions(
    state: State<'_, DbState>,
    profiles: State<'_, HighlightDbState>,
    highlights: State<'_, SessionHighlightState>,
    pg_user: State<'_, PgUserState>,
    folder_id: String,
    set_jump_host: bool,
    jump_host_id: Option<String>,
    set_highlight_profile: bool,
    highlight_profile_id: Option<String>,
    set_login_sequence: bool,
    login_sequence_id: Option<String>,
    icon: Option<String>,
) -> Result<u32, String> {
    let folder_uuid = parse_uuid(&folder_id)?;
    let jump_host = if set_jump_host {
        Some(match jump_host_id {
            Some(s) => Some(parse_uuid(&s)?),
            None => None,
        })
    } else {
        None
    };
    let highlight = if set_highlight_profile {
        Some(match highlight_profile_id {
            Some(s) => Some(parse_uuid(&s)?),
            None => None,
        })
    } else {
        None
    };
    let login_seq = if set_login_sequence {
        Some(match login_sequence_id {
            Some(s) => Some(parse_uuid(&s)?),
            None => None,
        })
    } else {
        None
    };
    if let Some(ref i) = icon {
        if i.len() > super::MAX_ICON_LEN {
            return Err(format!(
                "Icon too long (max {} characters)",
                super::MAX_ICON_LEN
            ));
        }
    }
    bulk_edit_sessions(
        &*state.0,
        &profiles,
        highlights.0.as_ref(),
        pg_user.0.as_deref(),
        folder_uuid,
        crate::db::BulkSessionEdit {
            jump_host_id: jump_host,
            highlight_profile_id: highlight,
            login_sequence_id: login_seq,
            icon,
        },
    )
    .await
}

/// A Bulk Edit of the sessions in `folder` and below it. `local` is `None`
/// in `SQLite` mode, where all of `edit` goes to the session rows.
///
/// In `PostgreSQL` mode the highlight profile is one of the user's own, and
/// the choice of it is kept locally, so it is taken out of what goes to the
/// shared database, and chosen for the sessions that the rest of the edit
/// covers: the role's own. It is checked first, so that a profile that is
/// not there changes nothing.
async fn bulk_edit_sessions(
    db: &dyn DatabaseProvider,
    profiles: &HighlightDbState,
    local: Option<&SessionHighlights>,
    role: Option<&str>,
    folder: Uuid,
    mut edit: crate::db::BulkSessionEdit,
) -> Result<u32, String> {
    let Some(local) = local else {
        return db.bulk_edit_sessions(folder, edit).await;
    };
    let Some(profile) = edit.highlight_profile_id.take() else {
        return db.bulk_edit_sessions(folder, edit).await;
    };
    if let Some(profile) = profile {
        require_highlight_profile(profiles, profile).await?;
    }
    // Before anything is changed, so that a failure to list them changes
    // nothing.
    let own = own_sessions_below(db, folder, role).await?;
    let edited = db.bulk_edit_sessions(folder, edit).await?;
    let chosen = local
        .set(&own, profile)
        .await
        .map_err(|e| format!("The sessions were updated, but not their highlight profile: {e}"))?;
    // The sessions touched, not the sum: both cover the same sessions, the
    // jump host only those of them that are SSH.
    Ok(edited.max(chosen))
}

// ── Session commands ─────────────────────────────────────────────────

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn session_create(
    state: State<'_, DbState>,
    profiles: State<'_, HighlightDbState>,
    highlights: State<'_, SessionHighlightState>,
    folder_id: String,
    name: String,
    hostname: String,
    port: i32,
    protocol: Option<String>,
    username: Option<String>,
    tags: String,
    icon: String,
    jump_host_id: Option<String>,
    highlight_profile_id: Option<String>,
    credential_profile_id: Option<String>,
    login_sequence_id: Option<String>,
    legacy_algorithms: Option<bool>,
) -> Result<Session, String> {
    validate_port(port)?;
    validate_session_fields(Some(&name), Some(&hostname), Some(&tags))?;
    if icon.len() > super::MAX_ICON_LEN {
        return Err(format!(
            "Icon too long (max {} characters)",
            super::MAX_ICON_LEN
        ));
    }
    let folder = parse_uuid(&folder_id)?;
    let jump = jump_host_id.map(|s| parse_uuid(&s)).transpose()?;

    let effective_protocol = protocol.unwrap_or_else(|| "ssh".to_string());
    if effective_protocol != "ssh" && effective_protocol != "telnet" {
        return Err(format!("Unsupported protocol: {effective_protocol}"));
    }

    let highlight = highlight_profile_id.map(|s| parse_uuid(&s)).transpose()?;
    let credential = credential_profile_id
        .as_deref()
        .map(parse_uuid)
        .transpose()?;
    let login_seq = login_sequence_id.as_deref().map(parse_uuid).transpose()?;

    create_session(
        &*state.0,
        &profiles,
        highlights.0.as_ref(),
        NewSession {
            folder_id: folder,
            name,
            hostname,
            port,
            protocol: effective_protocol,
            username: username.unwrap_or_default(),
            // auth_method is determined by the credential profile at connect
            // time; keep the column populated for backwards compatibility
            // with existing rows but don't expose it in the dialog.
            auth_method: "profile".to_string(),
            jump_host_id: jump,
            tags,
            icon,
            highlight_profile_id: highlight,
            credential_profile_id: credential,
            login_sequence_id: login_seq,
            legacy_algorithms: legacy_algorithms.unwrap_or(false),
        },
    )
    .await
}

/// Create a session. `local` is `None` in `SQLite` mode, where the row names
/// its highlight profile.
///
/// In `PostgreSQL` mode the highlight profile is the user's own choice: it
/// is kept in `local`, and the row, which every user of a shared session
/// reads, names none. The session that is returned has the user's choice.
async fn create_session(
    db: &dyn DatabaseProvider,
    profiles: &HighlightDbState,
    local: Option<&SessionHighlights>,
    mut session: NewSession,
) -> Result<Session, String> {
    let Some(local) = local else {
        return db.create_session(session).await;
    };
    let highlight = session.highlight_profile_id.take();
    if let Some(profile) = highlight {
        require_highlight_profile(profiles, profile).await?;
    }
    let mut created = db.create_session(session).await?;
    if let Some(profile) = highlight {
        local
            .set(&[created.id], Some(profile))
            .await
            .map_err(|e| format!("The session was saved, but not its highlight profile: {e}"))?;
    }
    created.highlight_profile_id = highlight;
    Ok(created)
}

#[tauri::command]
pub async fn session_get(
    state: State<'_, DbState>,
    highlights: State<'_, SessionHighlightState>,
    id: String,
) -> Result<Option<Session>, String> {
    let mut session = state.0.get_session(parse_uuid(&id)?).await?;
    if let (Some(local), Some(session)) = (highlights.0.as_ref(), session.as_mut()) {
        local.overlay_one(session).await;
    }
    Ok(session)
}

#[tauri::command]
pub async fn session_list_all(
    state: State<'_, DbState>,
    highlights: State<'_, SessionHighlightState>,
) -> Result<Vec<Session>, String> {
    let mut sessions = state.0.list_all_sessions().await?;
    if let Some(local) = highlights.0.as_ref() {
        local.overlay(&mut sessions).await;
    }
    Ok(sessions)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn session_update(
    state: State<'_, DbState>,
    profiles: State<'_, HighlightDbState>,
    highlights: State<'_, SessionHighlightState>,
    id: String,
    name: Option<String>,
    hostname: Option<String>,
    port: Option<i32>,
    protocol: Option<String>,
    username: Option<String>,
    tags: Option<String>,
    icon: Option<String>,
    jump_host_id: Option<String>,
    highlight_profile_id: Option<String>,
    credential_profile_id: Option<String>,
    login_sequence_id: Option<String>,
    legacy_algorithms: Option<bool>,
) -> Result<(), String> {
    if let Some(p) = port {
        validate_port(p)?;
    }
    if let Some(ref proto) = protocol {
        if proto != "ssh" && proto != "telnet" {
            return Err(format!("Unsupported protocol: {proto}"));
        }
    }
    validate_session_fields(name.as_deref(), hostname.as_deref(), tags.as_deref())?;
    if let Some(ref i) = icon {
        if i.len() > super::MAX_ICON_LEN {
            return Err(format!(
                "Icon too long (max {} characters)",
                super::MAX_ICON_LEN
            ));
        }
    }
    let session_id = parse_uuid(&id)?;
    // The frontend always sends these fields explicitly on update, so None
    // from the wire is the user's "clear to none" intent — translate it to
    // Some(None) to distinguish from "don't touch" in the DB layer.
    let jump = Some(match jump_host_id {
        Some(s) => Some(parse_uuid(&s)?),
        None => None,
    });
    let highlight = Some(match highlight_profile_id {
        Some(s) => Some(parse_uuid(&s)?),
        None => None,
    });
    let credential = Some(match credential_profile_id {
        Some(s) => Some(parse_uuid(&s)?),
        None => None,
    });
    let login_seq = Some(match login_sequence_id {
        Some(s) => Some(parse_uuid(&s)?),
        None => None,
    });

    update_session(
        &*state.0,
        &profiles,
        highlights.0.as_ref(),
        session_id,
        UpdateSession {
            name,
            hostname,
            port,
            protocol,
            username,
            auth_method: None,
            jump_host_id: jump,
            tags,
            icon,
            highlight_profile_id: highlight,
            credential_profile_id: credential,
            login_sequence_id: login_seq,
            legacy_algorithms,
        },
    )
    .await
}

/// Update a session. `local` is `None` in `SQLite` mode, where the row names
/// its highlight profile.
///
/// In `PostgreSQL` mode the highlight profile is the user's own choice: it
/// is kept in `local`, once the row has been updated, and the row's own
/// column is left as it is. Nothing reads that column any more; a version
/// from before profiles were each user's own still does.
async fn update_session(
    db: &dyn DatabaseProvider,
    profiles: &HighlightDbState,
    local: Option<&SessionHighlights>,
    id: Uuid,
    mut update: UpdateSession,
) -> Result<(), String> {
    let Some(local) = local else {
        return db.update_session(id, update).await;
    };
    let highlight = update.highlight_profile_id.take();
    if let Some(profile) = highlight {
        // An update of nothing else is no statement, which a session that is
        // not there would fail. So that no choice is kept for an ID that is
        // no session's, the session is looked for first.
        if db.get_session(id).await?.is_none() {
            return Err(format!("Session {id} not found"));
        }
        if let Some(profile) = profile {
            require_highlight_profile(profiles, profile).await?;
        }
    }
    db.update_session(id, update).await?;
    if let Some(profile) = highlight {
        local
            .set(&[id], profile)
            .await
            .map_err(|e| format!("The session was saved, but not its highlight profile: {e}"))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn session_move(
    state: State<'_, DbState>,
    id: String,
    new_folder_id: String,
) -> Result<(), String> {
    state
        .0
        .move_session(parse_uuid(&id)?, parse_uuid(&new_folder_id)?)
        .await
}

#[tauri::command]
pub async fn session_delete(
    state: State<'_, DbState>,
    highlights: State<'_, SessionHighlightState>,
    id: String,
) -> Result<(), String> {
    let session = parse_uuid(&id)?;
    state.0.delete_session(session).await?;
    if let Some(local) = highlights.0.as_ref() {
        forget_highlights(local, &[session]).await;
    }
    Ok(())
}

/// Cap the search term length to bound LIKE-query cost and prevent a
/// pathologically long needle from forcing expensive full-table scans.
const MAX_SEARCH_QUERY_LEN: usize = 200;

/// What a search found: the folders whose name has the query in it, and the
/// sessions that have it in their name, host, user name or tags. A session
/// is not found by the name of its folder; the window lists a folder that
/// was found with what is in it.
#[derive(Debug, serde::Serialize)]
pub struct SearchResults {
    pub folders: Vec<Folder>,
    pub sessions: Vec<Session>,
}

/// The query as it is searched for: its first [`MAX_SEARCH_QUERY_LEN`]
/// bytes, and as far beyond as the character at that place goes.
fn capped_query(query: &str) -> &str {
    if query.len() <= MAX_SEARCH_QUERY_LEN {
        return query;
    }
    &query[..query
        .char_indices()
        .take_while(|(i, _)| *i < MAX_SEARCH_QUERY_LEN)
        .last()
        .map_or(0, |(i, c)| i + c.len_utf8())]
}

/// Folders and sessions for one query. The two statements run one after the
/// other, so a search holds one of the pool's connections at a time.
async fn search(db: &dyn DatabaseProvider, query: &str) -> Result<SearchResults, String> {
    let query = capped_query(query);
    let folders = db.search_folders(query).await?;
    let sessions = db.search_sessions(query).await?;
    Ok(SearchResults { folders, sessions })
}

/// What a search is answered with that was not run, because a later one
/// asked while it waited for its turn ([`SearchGate`]).
const SEARCH_REPLACED: &str = "Search not run: a later search has replaced it";

/// What a search is answered with that was not run, because as many
/// searches as [`SEARCHES_UNENDED_AT_MOST`] were given up and have not ended.
const SEARCH_REFUSED: &str = "Search not run: earlier searches have not ended";

/// What a search is answered with whose task ended without an answer, as by
/// a panic. What it ended with is in the application log.
const SEARCH_FAILED: &str = "Search failed";

/// How long a search keeps its turn at the most.
///
/// With the limits the app sets for its `PostgreSQL` sessions
/// (`db::postgres::pool_options`), each of a search's two statements waits
/// up to 5 s for a connection and runs up to 60 s, which is 130 s. The rest
/// is for the read of the local database that follows them in `PostgreSQL`
/// mode, which has no limit of its own and takes a moment as a rule. A
/// search is therefore done before this as a rule where those limits are in
/// effect and the server answers. It is not where the server sets a longer
/// `statement_timeout`, which the app keeps, where a connection pooler does
/// not carry the limits along, or where a connection has gone silent: a
/// search can then outlast its turn ([`SearchGate::run`]).
const SEARCH_TURN_LIMIT: Duration = Duration::from_secs(150);

/// How many searches there are at the most that have not ended: the one
/// that has its turn, and those that were given up at
/// [`SEARCH_TURN_LIMIT`] and are still waiting for the database. Each holds
/// one database connection at the most, of the pool's ten.
const SEARCHES_UNENDED_AT_MOST: usize = 3;

/// Lets one search run at a time, and of the searches that wait for it, the
/// one that asked last.
///
/// A search reads every folder and every session the user can see: its
/// pattern begins with a wildcard, which no index serves. In `PostgreSQL`
/// mode those are statements against the shared database, each on one of the
/// pool's ten connections and for as long as `statement_timeout` lets it
/// run. Without the gate, searches that are asked for faster than the
/// database answers them run side by side: they load the server by their
/// number, and can take every connection of the pool, so that the user's
/// other commands fail for want of one.
///
/// A search that is running is not called off. sqlx cannot cancel a
/// statement, and a query whose future is dropped leaves the statement
/// running on the server and its connection waiting for the answer. So a
/// later search waits, without a connection. A search that waits when
/// another one asks is answered with [`SEARCH_REPLACED`] then, and not when
/// its turn would have come: what it would find is for a query that is no
/// longer asked, and so no more than one search waits, however many ask. A
/// caller that asks without end therefore keeps every search but its last
/// from running: the gate limits what searching costs, and not who searches.
///
/// A search counts as asked for when its command begins. Tauri runs a
/// command to its end, so one that asked is one that takes its turn: were
/// the last one to ask given up while it waits, the ones it replaced would
/// stay replaced, and the next search would be the next one asked for.
///
/// A search keeps its turn until it has its answer, and no longer than
/// [`SEARCH_TURN_LIMIT`]: one without an answer by then is given up, and the
/// next may run beside it ([`Self::run`]). The turn is the command's, so
/// this too rests on a command being run to its end; one that were dropped
/// would leave its search running without the turn, as one that is given
/// up, and counted like it. What searching costs is thus one
/// database connection at a time, and with searches that were given up, up
/// to [`SEARCHES_UNENDED_AT_MOST`].
///
/// The window paces its own searches (`src/stores/sessionStore.ts`). The gate
/// is for what the window does not keep to, such as the searches of a page
/// that was loaded again while the old page's search is still running.
pub struct SearchGate {
    /// Held by the search that has its turn.
    running: tokio::sync::Mutex<()>,
    /// The number of the search that asked last.
    asked: tokio::sync::watch::Sender<u64>,
    /// A permit for each search that has not ended, given up or not.
    unended: Arc<tokio::sync::Semaphore>,
}

impl Default for SearchGate {
    fn default() -> Self {
        Self {
            running: tokio::sync::Mutex::new(()),
            asked: tokio::sync::watch::channel(0).0,
            unended: Arc::new(tokio::sync::Semaphore::new(SEARCHES_UNENDED_AT_MOST)),
        }
    }
}

impl SearchGate {
    /// Waits until no search has its turn. `None` for a search that another
    /// one has asked after, as soon as that one asks: it is not to run, and
    /// gets no turn from then on. The search has its turn while it keeps
    /// what is returned.
    async fn turn(&self) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        let mut asked = self.asked.subscribe();
        let mut number = 0;
        self.asked.send_modify(|last| {
            *last += 1;
            number = *last;
        });
        tokio::select! {
            // A later search first: one that is replaced does not take a
            // turn that has come free in the same moment.
            biased;
            // Not an error: the numbers are sent by the gate, which is there.
            _ = asked.wait_for(|last| *last > number) => None,
            turn = self.running.lock() => Some(turn),
        }
    }

    /// Runs `search` when its turn has come, as a task of its own, and
    /// answers with what it found, or with an error once `limit` is up.
    ///
    /// The limit is for a database that does not answer. A connection that
    /// has gone silent without being closed brings none of the server's time
    /// limits to the app, and a search that waited on it would keep its
    /// turn, and every later search waiting, for as long as the operating
    /// system keeps the connection. After `limit` the search is given up:
    /// its turn ends, and its task is left to end by itself, with the
    /// connection it holds. Dropping it would put that connection back into
    /// the pool with a statement still running on it, for the next command
    /// to wait on.
    ///
    /// A search that was given up no longer has the turn, so the next one
    /// runs beside it. So that these do not take the pool's connections one
    /// by one while the database stays silent, a search is refused with
    /// [`SEARCH_REFUSED`] while [`SEARCHES_UNENDED_AT_MOST`] have not ended.
    async fn run<T, S>(&self, limit: Duration, search: S) -> Result<T, String>
    where
        T: Send + 'static,
        S: Future<Output = Result<T, String>> + Send + 'static,
    {
        let Some(_turn) = self.turn().await else {
            return Err(SEARCH_REPLACED.to_string());
        };
        let Ok(unended) = self.unended.clone().try_acquire_owned() else {
            return Err(SEARCH_REFUSED.to_string());
        };
        let task = tokio::spawn(async move {
            // Until the search has ended, whenever that is.
            let _unended = unended;
            search.await
        });
        match tokio::time::timeout(limit, task).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(e)) => {
                tracing::error!("The search's task ended without an answer: {e}");
                Err(SEARCH_FAILED.to_string())
            }
            Err(_) => Err(format!(
                "Search given up: the database has not answered within {limit:?}"
            )),
        }
    }
}

#[tauri::command]
pub async fn session_search(
    state: State<'_, DbState>,
    highlights: State<'_, SessionHighlightState>,
    gate: State<'_, SearchGate>,
    query: String,
) -> Result<SearchResults, String> {
    // A search that waits holds its query as it is searched for, and no
    // more of what it was sent.
    let mut query = query;
    query.truncate(capped_query(&query).len());
    query.shrink_to_fit();

    let db = state.0.clone();
    let local = highlights.0.clone();
    gate.run(SEARCH_TURN_LIMIT, async move {
        let mut results = search(db.as_ref(), &query).await?;
        if let Some(local) = local.as_ref() {
            local.overlay(&mut results.sessions).await;
        }
        Ok(results)
    })
    .await
}

#[tauri::command]
pub async fn session_data_fingerprint(
    state: State<'_, DbState>,
) -> Result<DataFingerprint, String> {
    state.0.data_fingerprint().await
}

// ── Visibility (multi-user) ──────────────────────────────────────────

/// Toggle an item's visibility between "personal" and "shared".
/// Only meaningful in `PostgreSQL` mode where `RLS` enforces ownership.
#[tauri::command]
pub async fn set_visibility(
    pg: State<'_, crate::db::PgPoolState>,
    id: String,
    item_type: String,
    visibility: String,
) -> Result<(), String> {
    let pool =
        pg.0.as_ref()
            .ok_or("Visibility control is only available in PostgreSQL mode")?;
    set_item_visibility(pool, parse_uuid(&id)?, &item_type, &visibility).await
}

/// Set the visibility of a folder or a session of the current role's own,
/// and with a folder that of the role's own folders and sessions below it.
///
/// Apart from the command, so that it can be run against a server without
/// the app. The walk down the folders is a `UNION`, not a `UNION ALL`: a
/// folder it has been through is not gone through again, so it ends on
/// folders that name each other as their parent.
pub(crate) async fn set_item_visibility(
    pool: &sqlx::PgPool,
    id: uuid::Uuid,
    item_type: &str,
    visibility: &str,
) -> Result<(), String> {
    if visibility != "personal" && visibility != "shared" {
        return Err(format!(
            "Invalid visibility \"{visibility}\": must be \"personal\" or \"shared\""
        ));
    }
    let id_str = id.to_string();
    match item_type {
        "folder" => {
            let result = sqlx::query(
                "UPDATE folders SET visibility = $1 WHERE id = $2 AND owner = current_user",
            )
            .bind(visibility)
            .bind(&id_str)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to set visibility: {e}"))?;
            if result.rows_affected() == 0 {
                return Err("Item not found or you are not the owner".to_string());
            }
            // Cascade to all descendant folders owned by the current user
            sqlx::query(
                "WITH RECURSIVE subtree(id) AS ( \
                     SELECT id FROM folders WHERE parent_id = $2 AND owner = current_user \
                     UNION \
                     SELECT f.id FROM folders f JOIN subtree s \
                         ON f.parent_id = s.id AND f.owner = current_user \
                 ) \
                 UPDATE folders SET visibility = $1 WHERE id IN (SELECT id FROM subtree)",
            )
            .bind(visibility)
            .bind(&id_str)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to cascade folder visibility: {e}"))?;
            // Cascade to all sessions in the target folder and descendants
            sqlx::query(
                "WITH RECURSIVE subtree(id) AS ( \
                     SELECT id FROM folders WHERE id = $2 AND owner = current_user \
                     UNION \
                     SELECT f.id FROM folders f JOIN subtree s \
                         ON f.parent_id = s.id AND f.owner = current_user \
                 ) \
                 UPDATE sessions SET visibility = $1 \
                 WHERE folder_id IN (SELECT id FROM subtree) AND owner = current_user",
            )
            .bind(visibility)
            .bind(&id_str)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to cascade session visibility: {e}"))?;
        }
        "session" => {
            let result = sqlx::query(
                "UPDATE sessions SET visibility = $1 WHERE id = $2 AND owner = current_user",
            )
            .bind(visibility)
            .bind(&id_str)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to set visibility: {e}"))?;
            if result.rows_affected() == 0 {
                return Err("Item not found or you are not the owner".to_string());
            }
        }
        _ => {
            return Err(format!(
                "Invalid item_type \"{item_type}\": must be \"folder\" or \"session\""
            ));
        }
    }
    Ok(())
}

// ── Reordering commands ──────────────────────────────────────────────

#[tauri::command]
pub async fn folder_reorder(
    state: State<'_, DbState>,
    parent_id: Option<String>,
    ordered_ids: Vec<String>,
) -> Result<(), String> {
    let parent_uuid = parent_id
        .as_deref()
        .map(|s| Uuid::parse_str(s).map_err(|e| format!("Invalid parent_id: {e}")))
        .transpose()?;
    let uuids: Vec<Uuid> = ordered_ids
        .iter()
        .map(|s| Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}")))
        .collect::<Result<_, _>>()?;
    state.0.reorder_folders(parent_uuid, uuids).await
}

#[tauri::command]
pub async fn session_reorder(
    state: State<'_, DbState>,
    folder_id: String,
    ordered_ids: Vec<String>,
) -> Result<(), String> {
    let folder_uuid = Uuid::parse_str(&folder_id).map_err(|e| format!("Invalid folder_id: {e}"))?;
    let uuids: Vec<Uuid> = ordered_ids
        .iter()
        .map(|s| Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}")))
        .collect::<Result<_, _>>()?;
    state.0.reorder_sessions(folder_uuid, uuids).await
}

#[tauri::command]
pub async fn folder_sort_alphabetically(
    state: State<'_, DbState>,
    parent_id: Option<String>,
    recursive: Option<bool>,
) -> Result<(), String> {
    let parent_uuid = parent_id
        .as_deref()
        .map(|s| Uuid::parse_str(s).map_err(|e| format!("Invalid parent_id: {e}")))
        .transpose()?;
    sort_children_alphabetically(&state.0, parent_uuid, recursive.unwrap_or(false)).await
}

#[tauri::command]
pub async fn session_sort_alphabetically(
    state: State<'_, DbState>,
    folder_id: String,
) -> Result<(), String> {
    let folder_uuid = Uuid::parse_str(&folder_id).map_err(|e| format!("Invalid folder_id: {e}"))?;
    state.0.sort_sessions_alphabetically(folder_uuid).await
}

#[tauri::command]
pub async fn session_sort_by_hostname(
    state: State<'_, DbState>,
    folder_id: String,
) -> Result<(), String> {
    let folder_uuid = Uuid::parse_str(&folder_id).map_err(|e| format!("Invalid folder_id: {e}"))?;
    state.0.sort_sessions_by_hostname(folder_uuid).await
}

/// Sort the immediate children (folders + sessions) of a parent folder
/// alphabetically.  When `recursive` is true, descend into every subfolder
/// and do the same.
///
/// A folder is gone into once: folders that name each other as their
/// parent, as a database may have from statements of someone's own, would
/// otherwise keep this going down without end. The folders are read once
/// for the same walk, after the first sort as before; sorting changes the
/// order of folders, not what they are in.
async fn sort_children_alphabetically(
    db: &std::sync::Arc<dyn super::super::db::DatabaseProvider>,
    parent_id: Option<Uuid>,
    recursive: bool,
) -> Result<(), String> {
    let mut all_folders = None;
    let mut seen = HashSet::new();
    let mut to_sort = vec![parent_id];
    while let Some(parent) = to_sort.pop() {
        if let Some(pid) = parent {
            if !seen.insert(pid) {
                continue;
            }
        }
        db.sort_folders_alphabetically(parent).await?;
        if let Some(pid) = parent {
            db.sort_sessions_alphabetically(pid).await?;
        }
        if recursive {
            let folders = match &all_folders {
                Some(folders) => folders,
                None => all_folders.insert(db.list_folders().await?),
            };
            to_sort.extend(
                folders
                    .iter()
                    .filter(|f| f.parent_id == parent)
                    .map(|f| Some(f.id)),
            );
        }
    }
    Ok(())
}

// ── Connect a saved session ──────────────────────────────────────────

/// Load a saved session from the database, resolve its credential profile,
/// walk any jump host chain, and open an SSH (or Telnet) connection.
/// Returns the live connection `ID` for use with `ssh_write` / `ssh_resize`.
///
/// ## Why this function is large
///
/// `session_connect` is intentionally an orchestration boundary: it ties
/// together state from eight Tauri-managed services (db, `cred_db`,
/// `login_seq_db`, ssh, telnet, terminal-ready, session-log, pg-pool,
/// config) plus per-session settings to produce one side-effecting
/// connection. The branchiness measured by cyclomatic complexity comes
/// from coordinating those services — every `?` is a state lookup that
/// must be released cleanly before the next, and every `if` selects
/// between code paths the user explicitly chose (SSH vs Telnet, jump
/// chain vs direct, password vs key vs keyboard-interactive, logging
/// on vs off, login-sequence present vs absent).
///
/// Splitting this into smaller helpers was tried and abandoned: the
/// helpers ended up taking 6-8 of the same `State<'_, …>` handles each,
/// duplicated the audit-trail logging at every boundary, and made
/// lifetime errors harder to read without reducing total complexity.
/// Unit-testing this in isolation would require mocking all eight
/// services and a real network endpoint; the right testing point is
/// the integration layer (Tauri's `mock_builder`), tracked separately.
///
/// If you are tempted to add a new branch here, prefer adding it to a
/// freestanding helper in `ssh::` or `telnet::` and calling that helper
/// from one place in this function. The shape of `session_connect` —
/// "load → validate → branch on protocol → emit" — should stay flat.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn session_connect(
    app_handle: AppHandle,
    db: State<'_, DbState>,
    cred_db: State<'_, CredentialDbState>,
    login_seq_db: State<'_, crate::db::LoginSequenceDbState>,
    ssh: State<'_, SshState>,
    telnet: State<'_, TelnetState>,
    ready_state: State<'_, TerminalReadyState>,
    logger_state: State<'_, SessionLogState>,
    pg: State<'_, crate::db::PgPoolState>,
    pg_user: State<'_, crate::db::PgUserState>,
    config: State<'_, crate::config::ConfigState>,
    pins: State<'_, LoginPinState>,
    page: String,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    ready_state.check_page(&page)?;
    validate_dimensions(cols, rows)?;

    let session_id = parse_uuid(&id)?;
    let session =
        db.0.get_session(session_id)
            .await?
            .ok_or_else(|| format!("Session {id} not found"))?;
    // The commands that save a session take no other port, but a row can be
    // written past them, and a port outside the range would be cut down to
    // another one than the row shows.
    validate_port(session.port)?;

    let settings = connection::current(&config)?;

    // What the session's own row names, where that is this user's to go by;
    // see `row_defaults_apply`.
    let row_defaults = row_defaults_apply(pg.0.is_some(), pg_user.0.as_deref(), &session);
    let row_login_sequence = session.login_sequence_id.filter(|_| row_defaults);
    let row_credential_profile = session.credential_profile_id.filter(|_| row_defaults);
    if !row_defaults
        && (session.login_sequence_id.is_some() || session.credential_profile_id.is_some())
    {
        debug!(session = %id, "Not this user's session — the profile and sequence its row names are left out");
    }

    // Resolve login sequence (applies to both SSH and Telnet).
    let user_ident_for_ls = pg.0.as_ref().and_then(|_| {
        config
            .config
            .lock()
            .ok()
            .and_then(|c| c.user_ident.clone())
            .filter(|s| !s.is_empty())
    });
    let mapped_ls_id = if let (Some(pool), Some(ref ident)) = (&pg.0, &user_ident_for_ls) {
        super::session_login_sequences::query_user_login_sequence(
            pool,
            &session_id.to_string(),
            ident,
        )
        .await?
    } else {
        None
    };
    // A login sequence the user assigned to a session that is not theirs is
    // run only for what they confirmed the session names; see
    // `login_pins.rs`. This comes before the sequence is read and before
    // anything is connected to, for Telnet as for SSH: `session` is the row
    // as read above, and it is what the rest of this function goes by.
    if needs_confirmation(pg.0.is_some(), row_defaults, mapped_ls_id.is_some()) {
        pins.check(&session, UsedAs::Target, pg_user.0.is_some())?;
    }
    let effective_ls_id = mapped_ls_id.or(row_login_sequence);

    let login_sequence = if let Some(ls_id) = effective_ls_id {
        login_seq_db.0.get_login_sequence(ls_id).await?
    } else {
        None
    };

    // Dispatch by protocol.
    if session.protocol == "telnet" {
        let conn_id = Uuid::new_v4().to_string();

        {
            let manager = telnet.0.lock().await;
            manager.check_capacity()?;
        }

        let logger = Some(std::sync::Arc::clone(&logger_state.0));
        let setup = ConnectSetup::begin(&conn_id, &session.name, page, &ready_state, &logger_state);

        // Create ready signal so the reader task waits for the frontend listener.
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
        setup.add_ready_signal(move || {
            let _ = ready_tx.send(());
        });

        let telnet_login_seq = if let Some(ref seq) = login_sequence {
            Some(crate::login_sequence::resolve_login_sequence(
                seq,
                &session.username,
                "",
            )?)
        } else {
            None
        };

        let telnet_session = establish_telnet_connection(TelnetConnectParams {
            id: conn_id.clone(),
            host: session.hostname,
            port: session.port as u16,
            cols,
            rows,
            app_handle,
            restrict_private_ips: settings.restrict_private_ips,
            connect_timeout_secs: settings.connect_timeout_secs,
            logger,
            ready_rx,
            login_sequence: telnet_login_seq,
        })
        .await?;

        {
            let mut manager = telnet.0.lock().await;
            setup.check_page_unchanged()?;
            manager.register_session(conn_id.clone(), telnet_session)?;
        }

        setup.disarm();
        return Ok(conn_id);
    }

    // SSH path (default). In PG mode, resolve per-user credential mapping first;
    // fall back to session's own credential_profile_id, then keyboard-interactive.
    let user_ident = pg.0.as_ref().and_then(|_| {
        config
            .config
            .lock()
            .ok()
            .and_then(|c| c.user_ident.clone())
            .filter(|s| !s.is_empty())
    });
    let mappings = match (&pg.0, &user_ident) {
        (Some(pool), Some(ident)) => CredentialMappings::Postgres {
            pool,
            user_ident: ident,
        },
        _ => CredentialMappings::None,
    };
    let mapped_credential_id = mappings.of(session_id).await?;
    // The same for a credential profile the user assigned, before the
    // profile is read.
    if needs_confirmation(pg.0.is_some(), row_defaults, mapped_credential_id.is_some()) {
        pins.check(&session, UsedAs::Target, pg_user.0.is_some())?;
    }
    let effective_credential_id = mapped_credential_id.or(row_credential_profile);

    let (username, auth_method, auth_credential) = if let Some(profile_id) = effective_credential_id
    {
        debug!(session = %id, profile_id = %profile_id, "Resolving credential profile for session");
        let profile = cred_db
            .0
            .get_credential_profile(profile_id)
            .await?
            .ok_or_else(|| {
                format!(
                    "Credential profile for session \"{}\" no longer exists. {}",
                    session.name,
                    other_profile_advice(row_defaults)
                )
            })?;
        debug!(session = %id, auth_type = %profile.auth_type, user = %profile.username, "Credential profile resolved");
        profile_to_auth(&profile)?
    } else {
        debug!(session = %id, user = %session.username, "No credential profile — falling back to keyboard-interactive");
        if session.username.is_empty() {
            return Err(format!(
                "No credential profile assigned and no username configured. {}",
                no_profile_advice(
                    row_defaults,
                    "Edit the session to set a username or assign a credential profile."
                )
            ));
        }
        (
            session.username.clone(),
            "keyboard-interactive".to_string(),
            Zeroizing::new(String::new()),
        )
    };

    // Resolve jump host chain.
    let jump_hops = resolve_jump_chain(
        db.0.as_ref(),
        cred_db.0.as_ref(),
        &session,
        pg.0.as_ref(),
        &mappings,
        pg_user.0.as_deref(),
        &pins,
    )
    .await?;
    if jump_hops.len() > MAX_JUMP_HOPS {
        return Err(format!("Too many jump hops (max {MAX_JUMP_HOPS})"));
    }

    let conn_id = Uuid::new_v4().to_string();

    {
        let manager = ssh.manager.lock().await;
        manager.check_capacity()?;
    }

    let logger = Some(std::sync::Arc::clone(&logger_state.0));
    let setup = ConnectSetup::begin(&conn_id, &session.name, page, &ready_state, &logger_state);

    // Create ready signal so the reader task waits for the frontend listener.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    setup.add_ready_signal(move || {
        let _ = ready_tx.send(());
    });

    let ssh_login_seq = if let Some(ref seq) = login_sequence {
        Some(crate::login_sequence::resolve_login_sequence(
            seq,
            &username,
            sequence_password(&auth_method, &auth_credential),
        )?)
    } else {
        None
    };

    let ssh_session = establish_ssh_connection(
        SshConnectParams {
            id: conn_id.clone(),
            host: session.hostname,
            port: session.port as u16,
            username,
            auth_method,
            auth_credential,
            cols,
            rows,
            app_handle,
            jump_hops,
            legacy_algorithms: session.legacy_algorithms,
            restrict_private_ips: settings.restrict_private_ips,
            connect_timeout_secs: settings.connect_timeout_secs,
            keepalive_interval_secs: settings.keepalive_interval_secs,
            keepalive_max: settings.keepalive_max as usize,
            logger,
            kbd_interactive_senders: std::sync::Arc::clone(&ssh.kbd_interactive_senders),
            ready_rx,
            login_sequence: ssh_login_seq,
        },
        &ssh.host_verify_senders,
    )
    .await?;

    {
        let mut manager = ssh.manager.lock().await;
        setup.check_page_unchanged()?;
        manager.register_session(conn_id.clone(), ssh_session)?;
    }

    setup.disarm();
    Ok(conn_id)
}

// ── Helpers ──────────────────────────────────────────────────────────

fn parse_uuid(s: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

/// Translate a profile into the (username, ssh `auth_method`, secret) triple
/// expected by the SSH connection layer.
fn profile_to_auth(
    profile: &CredentialProfile,
) -> Result<(String, String, Zeroizing<String>), String> {
    match profile.auth_type.as_str() {
        "password" => {
            let secret = crate::credentials::retrieve(&profile.keychain_ref)
                .map_err(|e| format!("Failed to retrieve profile secret: {e}"))?;
            Ok((profile.username.clone(), "password".to_string(), secret))
        }
        "key" => {
            // Private-key path is stored on the profile row. A key profile
            // has no secret in the keychain: a passphrase for the key is not
            // kept for a profile, and none is passed to russh.
            Ok((
                profile.username.clone(),
                "publickey".to_string(),
                Zeroizing::new(profile.key_path.clone()),
            ))
        }
        "keyboard-interactive" => Ok((
            profile.username.clone(),
            "keyboard-interactive".to_string(),
            Zeroizing::new(String::new()),
        )),
        other => Err(format!(
            "Auth type \"{other}\" is not yet supported for connect \
             (profile \"{}\").",
            profile.name
        )),
    }
}

/// Whether the credential profile and the login sequence that a session's
/// own row names are this user's to connect with.
///
/// Both are IDs in the connecting user's local database, and decide which of
/// that user's secrets the connection sends to the session's host. In
/// `PostgreSQL` mode a session can be another user's, shared: then that user
/// wrote the row, the host and these IDs alike, and an ID in it that happens
/// to be, or was made to be, one of this user's own profiles would send that
/// profile's secret to a host of the other user's choosing. So there the
/// row's IDs count only for the session's owner, and anyone else connects by
/// their own mapping of the session (`session_credentials`,
/// `session_login_sequences`) or without a profile.
///
/// The app itself leaves both columns empty in `PostgreSQL` mode and keeps
/// each user's choice in the mapping tables, so what a row names there was
/// imported or written by a statement of someone's own. In `SQLite` mode
/// every session is the user's own.
///
/// `pg_user` is the role the app connected as. Where that is not known, no
/// row is taken for this user's, nor is one that still has the placeholder
/// owner.
fn row_defaults_apply(pg_mode: bool, pg_user: Option<&str>, session: &Session) -> bool {
    // `'local'` is the placeholder owner of rows from before rows had one,
    // and no role's own, also not that of a role with this name.
    !pg_mode || (pg_user == Some(session.owner.as_str()) && session.owner != "local")
}

/// What a connect that finds no credential profile to go by tells the user to
/// do about it. `own` is the advice for a session that is the user's to edit.
/// For any other it is that what its row names does not count, and where the
/// user assigns their own: the session's context menu, which has **My Login**
/// for another user's session in place of **Edit**.
///
/// "Any other" is as [`row_defaults_apply`] has it: another user's shared
/// session, but also a row that still has the placeholder owner, and every
/// session while the role the app connected as is not known. The text
/// therefore does not say whose the session is. It does say who chooses the
/// host, since a profile assigned to a session is used for whatever host its
/// owner has it name, and that the User Identity has to be set, without
/// which no mapping is saved or looked up. While the role is not known the
/// menu has no **My Login**; that state is the database's status not having
/// been read, and passes with it.
fn no_profile_advice(row_defaults: bool, own: &'static str) -> &'static str {
    if row_defaults {
        own
    } else {
        "The credential profile and login sequence this session names are \
         used only for its owner, and you are not on record as that. Assign \
         a credential profile of your own to it from its context menu (My \
         Login), with your User Identity set in Settings. Its owner chooses \
         the host your profile is then used for."
    }
}

/// What a connect tells the user to do when the credential profile assigned
/// to a session or jump host is gone: where another one is picked, which is
/// the session dialog for a session that is the user's to edit and **My
/// Login** for any other; see [`no_profile_advice`].
fn other_profile_advice(row_defaults: bool) -> &'static str {
    if row_defaults {
        "Edit that session and pick a different profile."
    } else {
        "Pick a different profile for it from its context menu (My Login)."
    }
}

/// What `\w` in a login sequence stands for on an SSH connection: the
/// password of a password profile, and nothing with any other way of
/// authenticating. With a key profile the credential is the path of the key
/// file, which is no password, and nothing to type at a remote host's prompt:
/// it would tell that host where the key is kept and under which user name.
fn sequence_password<'a>(auth_method: &str, auth_credential: &'a str) -> &'a str {
    if auth_method == "password" {
        auth_credential
    } else {
        ""
    }
}

/// Whether a session may be the jump host of the session whose row names it,
/// given the owners of the two.
///
/// In `PostgreSQL` mode a jump host has to be a session of the same owner,
/// link by link, so a chain has one owner. The owner of a shared session
/// chooses its jump host, and the database took any session's ID for it,
/// another user's included. A user who opened such a session went through a
/// jump host they had a profile for, which is first of all a session of
/// their own: their own bastion then opened a connection to the host and
/// port the other user's session names. And a jump host that its owner
/// deleted was dropped from the session of someone else that named it, which
/// then connected directly.
///
/// The database refuses to name such a jump host now
/// (`shellstation_own_jump_host` in `setup_postgres_rls`). This is for what
/// was named before, for a database whose owning role has not started that
/// version yet, and for one where a trigger does not hold a user (Option B).
/// It goes by the owners of the two rows, not by who connects, and so holds
/// as far as a row's owner does: a user who can write another owner into a
/// row of their own, which the policies and `shellstation_keep_owner` keep a
/// regular user from, gets past it. Rows that still have the placeholder
/// `'local'` for an owner count as one owner's among themselves, and as
/// another owner's to everyone else's.
///
/// In `SQLite` mode every session is the user's own, whatever its owner says.
fn jump_host_is_its_owners(pg_mode: bool, named_by_owner: &str, jump_host_owner: &str) -> bool {
    !pg_mode || named_by_owner == jump_host_owner
}

/// Walk the `jump_host_id` chain in the database to build a list of `JumpHop`
/// entries for the SSH connect call. Each hop pulls its own profile. In
/// `PostgreSQL` mode the walk fails at a hop that is not a session of the
/// same owner as the session that names it, before that hop's profile is
/// looked up, and no hop is returned; see [`jump_host_is_its_owners`]. It
/// fails likewise at a hop that is not the user's own session and no longer
/// names what the user confirmed of it (`pins`, see `login_pins.rs`). Where
/// a profile the user assigned is used for such a hop, the session connected
/// to is held to what the user confirmed of it as well, also where nothing
/// is assigned to that session itself: its switch for legacy algorithms is
/// what the connection to every hop is made with, so a session of the same
/// owner that the user never confirmed, or one whose switch has been turned
/// since, would carry the hop's profile over weaker algorithms than the
/// user agreed to. The caller has resolved the target's own profile by
/// then, and has connected to nothing.
async fn resolve_jump_chain(
    db: &dyn DatabaseProvider,
    cred_db: &dyn DatabaseProvider,
    session: &Session,
    pg_pool: Option<&sqlx::PgPool>,
    mappings: &CredentialMappings<'_>,
    pg_user: Option<&str>,
    pins: &LoginPinState,
) -> Result<Vec<crate::ssh::JumpHop>, String> {
    let mut hops = Vec::new();
    let mut visited = HashSet::new();
    let mut current_jump_id = session.jump_host_id;
    // The session whose row names the hop being looked at: the target first,
    // then each hop for the one before it.
    let mut named_by = (session.name.clone(), session.owner.clone());
    // Whether the session connected to has been held to what the user
    // confirmed of it, which the first hop with a profile of the user's
    // asks for.
    let mut target_confirmed = false;

    while let Some(jump_id) = current_jump_id {
        if !visited.insert(jump_id) {
            return Err(format!(
                "Circular jump host chain detected at session {jump_id}"
            ));
        }
        if hops.len() >= MAX_JUMP_HOPS {
            return Err("Jump host chain too deep".to_string());
        }

        let jump_session = db.get_session(jump_id).await?.ok_or_else(|| {
            "Jump host session not found. It may be a personal session belonging \
             to another user — ask the owner to share it."
                .to_string()
        })?;

        // As for the session connected to: no port that would be cut down
        // to another one.
        validate_port(jump_session.port).map_err(|e| {
            format!(
                "Jump host \"{}\": {e}",
                crate::login_pins::name_for_message(&jump_session.name)
            )
        })?;

        if !jump_host_is_its_owners(pg_pool.is_some(), &named_by.1, &jump_session.owner) {
            debug!(hop = hops.len(), jump_id = %jump_id, "Jump hop is not a session of the owner of the session that names it");
            return Err(format!(
                "\"{}\" names \"{}\" as its jump host, which is not a session \
                 of the same owner. A jump host has to be one, so this \
                 connection is not opened. The owner of \"{}\" can pick one of \
                 their own sessions as its jump host.",
                named_by.0, jump_session.name, named_by.0
            ));
        }

        debug!(
            hop = hops.len(),
            jump_id = %jump_id,
            host = %jump_session.hostname,
            port = jump_session.port,
            "Resolving jump hop"
        );

        // In PG mode, resolve per-user credential mapping for the hop. The
        // profile the hop's own row names counts only where the hop is this
        // user's own session; see `row_defaults_apply`.
        let row_defaults = row_defaults_apply(pg_pool.is_some(), pg_user, &jump_session);
        let row_credential_profile = jump_session.credential_profile_id.filter(|_| row_defaults);
        let mapped_profile_id = mappings.of(jump_id).await?;
        // A profile the user assigned to a jump host that is not their own
        // session is used only for what they confirmed that session names,
        // before the profile is read; see `login_pins.rs`.
        if needs_confirmation(pg_pool.is_some(), row_defaults, mapped_profile_id.is_some()) {
            if !target_confirmed {
                pins.check(session, UsedAs::Target, pg_user.is_some())?;
                target_confirmed = true;
            }
            pins.check(&jump_session, UsedAs::JumpHost, pg_user.is_some())?;
        }
        let effective_profile_id = mapped_profile_id.or(row_credential_profile);

        let profile_id = effective_profile_id.ok_or_else(|| {
            format!(
                "No credential profile assigned to jump host \"{}\". {}",
                jump_session.name,
                no_profile_advice(
                    row_defaults,
                    "Open the Credentials Manager to create one, then edit the \
                     session to link it."
                )
            )
        })?;
        let jump_profile = cred_db
            .get_credential_profile(profile_id)
            .await?
            .ok_or_else(|| {
                format!(
                    "Credential profile for jump host \"{}\" no longer exists. {}",
                    jump_session.name,
                    other_profile_advice(row_defaults)
                )
            })?;
        let (username, auth_method, auth_credential) = profile_to_auth(&jump_profile)?;

        hops.push(crate::ssh::JumpHop {
            host: jump_session.hostname.clone(),
            port: jump_session.port as u16,
            username,
            auth_method,
            auth_credential,
        });

        current_jump_id = jump_session.jump_host_id;
        named_by = (jump_session.name, jump_session.owner);
    }

    // The chain is built from target→bastion order, but SSH needs bastion→target.
    hops.reverse();
    debug!(total_hops = hops.len(), "Jump chain resolved");
    Ok(hops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::NewCredentialProfile;
    use crate::db::sqlite::SqliteProvider;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    fn session_of(owner: &str) -> Session {
        Session {
            id: Uuid::new_v4(),
            folder_id: Uuid::new_v4(),
            name: "session".to_string(),
            hostname: "host.example".to_string(),
            port: 22,
            protocol: "ssh".to_string(),
            username: String::new(),
            auth_method: "password".to_string(),
            jump_host_id: None,
            tags: "[]".to_string(),
            icon: "desktop".to_string(),
            sort_order: 0,
            highlight_profile_id: None,
            credential_profile_id: Some(Uuid::new_v4()),
            legacy_algorithms: false,
            login_sequence_id: Some(Uuid::new_v4()),
            owner: owner.to_string(),
            visibility: "shared".to_string(),
        }
    }

    #[test]
    fn a_row_names_a_profile_for_its_owner_only() {
        // `SQLite`: every session is the user's own, whatever its owner says.
        assert!(row_defaults_apply(false, None, &session_of("local")));
        assert!(row_defaults_apply(false, Some("alice"), &session_of("bob")));
        // `PostgreSQL`: the user's own session, but not another user's shared
        // one, and no session at all while the role is not known.
        assert!(row_defaults_apply(
            true,
            Some("alice"),
            &session_of("alice")
        ));
        assert!(!row_defaults_apply(true, Some("alice"), &session_of("bob")));
        assert!(!row_defaults_apply(
            true,
            Some("alice"),
            &session_of("Alice")
        ));
        assert!(!row_defaults_apply(true, None, &session_of("alice")));
        assert!(!row_defaults_apply(true, None, &session_of("")));
        // The placeholder owner is no role's, also not that of a role named
        // like it.
        assert!(!row_defaults_apply(
            true,
            Some("local"),
            &session_of("local")
        ));
        // The advice to edit a session is for its owner; anyone else is
        // told where their own profile is assigned.
        assert_eq!(
            no_profile_advice(true, "Edit the session."),
            "Edit the session."
        );
        let advice = no_profile_advice(false, "Edit the session.");
        assert!(advice.contains("only for its owner"));
        assert!(advice.contains("My Login"));
        assert!(advice.contains("owner chooses the host"));
        assert!(!advice.contains("Edit the session"));
        // It is also what a row with the placeholder owner gets, so it does
        // not call the session another user's.
        assert!(!advice.contains("another user"));
        // The same goes for a profile that is gone.
        assert!(other_profile_advice(true).contains("Edit that session"));
        assert!(other_profile_advice(false).contains("My Login"));
        assert!(!other_profile_advice(false).contains("Edit"));
    }

    #[test]
    fn a_login_sequence_types_a_password_and_no_key_path() {
        assert_eq!(sequence_password("password", "hunter2"), "hunter2");
        assert_eq!(
            sequence_password("publickey", "/home/a/.ssh/id_ed25519"),
            ""
        );
        assert_eq!(sequence_password("keyboard-interactive", ""), "");
        assert_eq!(sequence_password("", "x"), "");
    }

    #[test]
    fn a_jump_host_is_a_session_of_the_same_owner() {
        // `SQLite`: every session is the user's own, whatever its owner says.
        assert!(jump_host_is_its_owners(false, "local", "local"));
        assert!(jump_host_is_its_owners(false, "alice", "bob"));
        // `PostgreSQL`: the jump host of a session is its owner's, whoever
        // connects. Another user's is none, and role names are not folded.
        assert!(jump_host_is_its_owners(true, "alice", "alice"));
        assert!(!jump_host_is_its_owners(true, "alice", "bob"));
        assert!(!jump_host_is_its_owners(true, "bob", "alice"));
        assert!(!jump_host_is_its_owners(true, "alice", "Alice"));
        // A database from before rows had an owner, where every row still
        // has the placeholder: there is one owner, and no one to keep apart.
        assert!(jump_host_is_its_owners(true, "local", "local"));
        assert!(!jump_host_is_its_owners(true, "local", "alice"));
    }

    /// Nothing confirmed, and a file that is never written: these chains
    /// are of the user's own sessions, which need no confirming.
    fn no_pins() -> LoginPinState {
        LoginPinState::new(std::env::temp_dir().join(format!("ss-no-pins-{}", Uuid::new_v4())))
    }

    /// The error of a chain that does not resolve.
    async fn chain_error(
        db: &SqliteProvider,
        target: Uuid,
        pg_pool: Option<&sqlx::PgPool>,
    ) -> String {
        let target = db.get_session(target).await.unwrap().unwrap();
        resolve_jump_chain(
            db,
            db,
            &target,
            pg_pool,
            &CredentialMappings::None,
            Some("alice"),
            &no_pins(),
        )
        .await
        .err()
        .expect("the chain resolved")
    }

    #[tokio::test]
    async fn a_recursive_sort_ends_on_folders_that_name_each_other() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(":memory:"))
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        let db: std::sync::Arc<dyn DatabaseProvider> =
            std::sync::Arc::new(SqliteProvider::new(pool.clone()));
        let folder = |name: &str, parent_id: Option<Uuid>| {
            let db = db.clone();
            let name = name.to_string();
            async move {
                db.create_folder(NewFolder { name, parent_id })
                    .await
                    .unwrap()
                    .id
            }
        };
        // Two folders that name each other as their parent, as only a
        // statement past the move command leaves them, and in the second
        // two more, out of order.
        let ring_a = folder("ring a", None).await;
        let ring_b = folder("ring b", Some(ring_a)).await;
        let zeta = folder("zeta", Some(ring_b)).await;
        folder("in zeta, second", Some(zeta)).await;
        folder("in zeta, first", Some(zeta)).await;
        folder("alpha", Some(ring_b)).await;
        db.move_folder(ring_a, Some(ring_b)).await.unwrap();

        let names_in = |parent: Uuid| {
            let db = db.clone();
            async move {
                let mut folders = db.list_folders().await.unwrap();
                folders.retain(|f| f.parent_id == Some(parent));
                folders.sort_by_key(|f| f.sort_order);
                folders.into_iter().map(|f| f.name).collect::<Vec<_>>()
            }
        };
        assert_eq!(names_in(ring_b).await, ["zeta", "alpha", "ring a"]);
        assert_eq!(names_in(zeta).await, ["in zeta, second", "in zeta, first"]);

        // From either folder of the ring the sort ends, and has been through
        // what is below the ring as well. One that does not end fails here.
        for start in [ring_a, ring_b] {
            sqlx::query("UPDATE folders SET sort_order = -sort_order")
                .execute(&pool)
                .await
                .unwrap();
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                sort_children_alphabetically(&db, Some(start), true),
            )
            .await
            .expect("the sort did not end")
            .unwrap();
            assert_eq!(names_in(ring_b).await, ["alpha", "ring a", "zeta"]);
            assert_eq!(names_in(zeta).await, ["in zeta, first", "in zeta, second"]);
        }
        // Without going down, only the folder asked for is sorted.
        sqlx::query("UPDATE folders SET sort_order = -sort_order")
            .execute(&pool)
            .await
            .unwrap();
        sort_children_alphabetically(&db, Some(ring_b), false)
            .await
            .unwrap();
        assert_eq!(names_in(ring_b).await, ["alpha", "ring a", "zeta"]);
        assert_eq!(names_in(zeta).await, ["in zeta, second", "in zeta, first"]);

        // A folder that names itself as its parent is gone into once as
        // well.
        let own = folder("its own", None).await;
        db.move_folder(own, Some(own)).await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            sort_children_alphabetically(&db, Some(own), true),
        )
        .await
        .expect("the sort did not end")
        .unwrap();
    }

    #[tokio::test]
    async fn a_chain_fails_at_the_first_jump_host_of_another_owner() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(":memory:"))
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        let db = SqliteProvider::new(pool.clone());
        // What says that this is `PostgreSQL` mode. No server is contacted:
        // the pool connects at its first statement, and without a user
        // identity no mapping is looked up.
        let pg = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/none")
            .unwrap();

        let folder = db
            .create_folder(NewFolder {
                name: "folder".to_string(),
                parent_id: None,
            })
            .await
            .unwrap()
            .id;
        let profile = db
            .create_credential_profile(NewCredentialProfile {
                name: "key".to_string(),
                auth_type: "key".to_string(),
                username: "admin".to_string(),
                key_path: "id_ed25519".to_string(),
            })
            .await
            .unwrap()
            .id;
        // A session of `owner`, which a `PostgreSQL` row has and the `SQLite`
        // provider writes as the placeholder.
        let session = |name: &'static str,
                       owner: &'static str,
                       jump_host_id: Option<Uuid>,
                       credential_profile_id: Option<Uuid>| {
            let (db, pool) = (&db, &pool);
            async move {
                let id = db
                    .create_session(NewSession {
                        folder_id: folder,
                        name: name.to_string(),
                        hostname: format!("{name}.example"),
                        port: 22,
                        protocol: "ssh".to_string(),
                        username: String::new(),
                        auth_method: "password".to_string(),
                        jump_host_id,
                        tags: "[]".to_string(),
                        icon: "desktop".to_string(),
                        highlight_profile_id: None,
                        credential_profile_id,
                        login_sequence_id: None,
                        legacy_algorithms: false,
                    })
                    .await
                    .unwrap()
                    .id;
                sqlx::query("UPDATE sessions SET owner = ? WHERE id = ?")
                    .bind(owner)
                    .bind(id.to_string())
                    .execute(pool)
                    .await
                    .unwrap();
                id
            }
        };

        // target (alice) -> near (alice) -> far (bob), and `far` has no
        // profile.
        let far = session("far", "bob", None, None).await;
        let near = session("near", "alice", Some(far), Some(profile)).await;
        let target = session("target", "alice", Some(near), None).await;

        // The link that is refused is the one from `near` to `far`, whether
        // it is the first of the chain or a later one, and the error names
        // its two sessions. It comes before `far`'s profile is looked up, or
        // the missing profile would be the error.
        for from in [near, target] {
            let error = chain_error(&db, from, Some(&pg)).await;
            assert!(
                error.starts_with("\"near\" names \"far\" as its jump host"),
                "{error}"
            );
            assert!(error.contains("not a session of the same owner"), "{error}");
        }

        // `SQLite` mode has no such rule: the walk goes on to `far`.
        let error = chain_error(&db, target, None).await;
        assert!(
            error.starts_with("No credential profile assigned to jump host \"far\""),
            "{error}"
        );

        // A chain of one owner resolves, bastion first.
        let far = session("far2", "alice", None, Some(profile)).await;
        let near = session("near2", "alice", Some(far), Some(profile)).await;
        let target = session("target2", "alice", Some(near), None).await;
        let target = db.get_session(target).await.unwrap().unwrap();
        let hops = resolve_jump_chain(
            &db,
            &db,
            &target,
            Some(&pg),
            &CredentialMappings::None,
            Some("alice"),
            &no_pins(),
        )
        .await
        .expect("a chain of one owner did not resolve");
        let hosts: Vec<&str> = hops.iter().map(|hop| hop.host.as_str()).collect();
        assert_eq!(hosts, ["far2.example", "near2.example"]);

        // Another user's shared session and its jump host, which `alice`
        // connects to with a profile of her own assigned to the jump host.
        // The profile is used only for what she confirmed the jump host
        // names; see `login_pins.rs`.
        let bastion = session("bastion", "bob", None, None).await;
        let shared = session("shared", "bob", Some(bastion), None).await;
        let shared = db.get_session(shared).await.unwrap().unwrap();
        let assigned = CredentialMappings::Fixed([(bastion, profile)].into());
        let dir = crate::login_pins::tests::TestDir::new();
        let pins = LoginPinState::new(dir.file());
        let through_bastion = |pins| {
            let (db, shared, pg, assigned) = (&db, &shared, &pg, &assigned);
            async move {
                resolve_jump_chain(db, db, shared, Some(pg), assigned, Some("alice"), pins).await
            }
        };

        // Never confirmed: refused, before the profile is read. Nothing is
        // assigned to the session itself, but the profile for its jump host
        // would be used with the session's switch for legacy algorithms, so
        // the session has to be confirmed too.
        let error = through_bastion(&pins).await.err().expect("not refused");
        assert!(
            error.starts_with("Session \"shared\" is not your own, and you have not confirmed"),
            "{error}"
        );
        // The session confirmed and its jump host not: refused at that.
        pins.confirm(
            (shared.id, crate::login_pins::SessionPin::of(&shared)),
            Vec::new(),
        )
        .unwrap();
        let error = through_bastion(&pins).await.err().expect("not refused");
        assert!(
            error.starts_with("Jump host \"bastion\" is not your own, and you have not confirmed"),
            "{error}"
        );
        // Both confirmed, each as what it is: the chain resolves.
        let row = db.get_session(bastion).await.unwrap().unwrap();
        pins.confirm(
            (shared.id, crate::login_pins::SessionPin::of(&shared)),
            vec![(bastion, crate::login_pins::JumpHostPin::of(&row))],
        )
        .unwrap();
        let hops = through_bastion(&pins)
            .await
            .expect("refused though confirmed");
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0].host, "bastion.example");
        // Another session of the same owner through the same jump host, which
        // was never confirmed: the profile for the jump host is not used
        // through it, whatever its switch says.
        let other = session("other", "bob", Some(bastion), None).await;
        let other = db.get_session(other).await.unwrap().unwrap();
        let error =
            resolve_jump_chain(&db, &db, &other, Some(&pg), &assigned, Some("alice"), &pins)
                .await
                .err()
                .expect("not refused");
        assert!(
            error.starts_with("Session \"other\" is not your own, and you have not confirmed"),
            "{error}"
        );
        // And the confirmed session with its switch turned on since.
        let mut switched = shared.clone();
        switched.legacy_algorithms = true;
        let error = resolve_jump_chain(
            &db,
            &db,
            &switched,
            Some(&pg),
            &assigned,
            Some("alice"),
            &pins,
        )
        .await
        .err()
        .expect("not refused");
        assert!(error.contains("legacy algorithms off → on"), "{error}");
        // Its owner has it name another host: refused again, and told why.
        sqlx::query("UPDATE sessions SET hostname = 'elsewhere.example' WHERE id = ?")
            .bind(bastion.to_string())
            .execute(&pool)
            .await
            .unwrap();
        let error = through_bastion(&pins).await.err().expect("not refused");
        assert!(
            error.contains("host bastion.example → elsewhere.example"),
            "{error}"
        );
        // Without a known role nothing of hers is used.
        let error = resolve_jump_chain(&db, &db, &shared, Some(&pg), &assigned, None, &pins)
            .await
            .err()
            .expect("not refused");
        assert!(error.contains("Restart ShellStation"), "{error}");
        // And with nothing assigned there is nothing to confirm: the jump
        // host has no profile for her, which is the error.
        let error = resolve_jump_chain(
            &db,
            &db,
            &shared,
            Some(&pg),
            &CredentialMappings::None,
            Some("alice"),
            &pins,
        )
        .await
        .err()
        .expect("not refused");
        assert!(
            error.starts_with("No credential profile assigned to jump host \"bastion\""),
            "{error}"
        );
        // In `SQLite` mode every session is the user's own, whatever its
        // owner says, so there the row's own profile counts and nothing is
        // confirmed; this one's row names none.
        let error = resolve_jump_chain(
            &db,
            &db,
            &shared,
            None,
            &CredentialMappings::None,
            Some("alice"),
            &pins,
        )
        .await
        .err()
        .expect("not refused");
        assert!(
            error.starts_with("No credential profile assigned to jump host \"bastion\""),
            "{error}"
        );
    }

    // ── Highlight profiles in PostgreSQL mode ────────────────────────────

    use crate::commands::highlight::set_session_highlight_profile;
    use crate::db::models::NewHighlightProfile;

    /// What the commands work with in `PostgreSQL` mode: a database that
    /// stands for the shared one, and the user's local one with their
    /// highlight profiles and the profile chosen for each session.
    struct Highlighting {
        shared: SqliteProvider,
        profiles: HighlightDbState,
        local: SessionHighlights,
        folder: Uuid,
    }

    async fn memory_pool(foreign_keys: &'static str) -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(":memory:")
                    .pragma("foreign_keys", foreign_keys),
            )
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        pool
    }

    async fn highlighting() -> Highlighting {
        let shared = SqliteProvider::new(memory_pool("ON").await);
        let local_pool = memory_pool("OFF").await;
        let local = SessionHighlights::open(local_pool.clone()).await.unwrap();
        let profiles = HighlightDbState(std::sync::Arc::new(SqliteProvider::new(local_pool)));
        let folder = shared
            .create_folder(NewFolder {
                name: "folder".to_string(),
                parent_id: None,
            })
            .await
            .unwrap()
            .id;
        Highlighting {
            shared,
            profiles,
            local,
            folder,
        }
    }

    async fn profile_in(db: &dyn DatabaseProvider, name: &str) -> Uuid {
        db.create_highlight_profile(NewHighlightProfile {
            name: name.to_string(),
            rules: "[]".to_string(),
        })
        .await
        .unwrap()
        .id
    }

    fn new_session(folder_id: Uuid, name: &str, highlight_profile_id: Option<Uuid>) -> NewSession {
        NewSession {
            folder_id,
            name: name.to_string(),
            hostname: format!("{name}.example"),
            port: 22,
            protocol: "ssh".to_string(),
            username: String::new(),
            auth_method: "profile".to_string(),
            jump_host_id: None,
            tags: "[]".to_string(),
            icon: "desktop".to_string(),
            highlight_profile_id,
            credential_profile_id: None,
            login_sequence_id: None,
            legacy_algorithms: false,
        }
    }

    async fn row(db: &dyn DatabaseProvider, id: Uuid) -> Session {
        db.get_session(id).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn in_postgresql_mode_a_new_session_keeps_its_highlight_profile_locally() {
        let h = highlighting().await;
        let mine = profile_in(&*h.profiles.0, "mine").await;

        let created = create_session(
            &h.shared,
            &h.profiles,
            Some(&h.local),
            new_session(h.folder, "router", Some(mine)),
        )
        .await
        .unwrap();
        // The window gets the user's choice; the row, which every user of a
        // shared session reads, names no profile.
        assert_eq!(created.highlight_profile_id, Some(mine));
        assert_eq!(row(&h.shared, created.id).await.highlight_profile_id, None);
        assert_eq!(h.local.get(created.id).await.unwrap(), Some(mine));

        let plain = create_session(
            &h.shared,
            &h.profiles,
            Some(&h.local),
            new_session(h.folder, "switch", None),
        )
        .await
        .unwrap();
        assert_eq!(plain.highlight_profile_id, None);
        assert_eq!(h.local.get(plain.id).await.unwrap(), None);

        // A profile that is not one of the user's is refused before anything
        // is created. One of the shared database is no such profile.
        let central = profile_in(&h.shared, "central").await;
        let error = create_session(
            &h.shared,
            &h.profiles,
            Some(&h.local),
            new_session(h.folder, "firewall", Some(central)),
        )
        .await
        .unwrap_err();
        assert!(error.contains("not found"), "{error}");
        assert_eq!(h.shared.list_all_sessions().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn in_postgresql_mode_an_update_leaves_the_rows_highlight_profile_alone() {
        let h = highlighting().await;
        let mine = profile_in(&*h.profiles.0, "mine").await;
        // A row from before profiles were each user's own names one of the
        // shared database.
        let central = profile_in(&h.shared, "central").await;
        let id = h
            .shared
            .create_session(new_session(h.folder, "router", Some(central)))
            .await
            .unwrap()
            .id;

        let update = |name: &str, highlight: Option<Uuid>| UpdateSession {
            name: Some(name.to_string()),
            highlight_profile_id: Some(highlight),
            ..Default::default()
        };
        update_session(
            &h.shared,
            &h.profiles,
            Some(&h.local),
            id,
            update("core", Some(mine)),
        )
        .await
        .unwrap();
        let after = row(&h.shared, id).await;
        assert_eq!(after.name, "core");
        assert_eq!(after.highlight_profile_id, Some(central));
        assert_eq!(h.local.get(id).await.unwrap(), Some(mine));

        // "None" clears the user's choice, and still not the row.
        update_session(
            &h.shared,
            &h.profiles,
            Some(&h.local),
            id,
            update("core", None),
        )
        .await
        .unwrap();
        assert_eq!(row(&h.shared, id).await.highlight_profile_id, Some(central));
        assert_eq!(h.local.get(id).await.unwrap(), None);

        // An update that says nothing about the profile keeps the choice.
        h.local.set(&[id], Some(mine)).await.unwrap();
        update_session(
            &h.shared,
            &h.profiles,
            Some(&h.local),
            id,
            UpdateSession {
                name: Some("edge".to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(h.local.get(id).await.unwrap(), Some(mine));

        // A profile that is not the user's changes nothing, the name included.
        let error = update_session(
            &h.shared,
            &h.profiles,
            Some(&h.local),
            id,
            update("renamed", Some(central)),
        )
        .await
        .unwrap_err();
        assert!(error.contains("not found"), "{error}");
        assert_eq!(row(&h.shared, id).await.name, "edge");
        assert_eq!(h.local.get(id).await.unwrap(), Some(mine));

        // A session that is not there gets no choice either, also where the
        // profile is all the update names, which is no statement that could
        // fail.
        let missing = Uuid::new_v4();
        let error = update_session(
            &h.shared,
            &h.profiles,
            Some(&h.local),
            missing,
            UpdateSession {
                highlight_profile_id: Some(Some(mine)),
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("not found"), "{error}");
        assert_eq!(h.local.get(missing).await.unwrap(), None);
        update_session(
            &h.shared,
            &h.profiles,
            Some(&h.local),
            missing,
            update("x", Some(mine)),
        )
        .await
        .unwrap_err();
        assert_eq!(h.local.get(missing).await.unwrap(), None);
    }

    #[tokio::test]
    async fn in_sqlite_mode_the_row_names_the_highlight_profile() {
        let h = highlighting().await;
        let db: std::sync::Arc<dyn DatabaseProvider> = std::sync::Arc::new(h.shared);
        let profiles = HighlightDbState(db.clone());
        let (first, second) = (
            profile_in(&*db, "first").await,
            profile_in(&*db, "second").await,
        );

        let created = create_session(
            &*db,
            &profiles,
            None,
            new_session(h.folder, "router", Some(first)),
        )
        .await
        .unwrap();
        assert_eq!(
            row(&*db, created.id).await.highlight_profile_id,
            Some(first)
        );

        update_session(
            &*db,
            &profiles,
            None,
            created.id,
            UpdateSession {
                highlight_profile_id: Some(Some(second)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(
            row(&*db, created.id).await.highlight_profile_id,
            Some(second)
        );

        set_session_highlight_profile(&*db, &profiles, None, created.id, None)
            .await
            .unwrap();
        assert_eq!(row(&*db, created.id).await.highlight_profile_id, None);
        set_session_highlight_profile(&*db, &profiles, None, created.id, Some(first))
            .await
            .unwrap();
        assert_eq!(
            row(&*db, created.id).await.highlight_profile_id,
            Some(first)
        );
    }

    #[tokio::test]
    async fn in_postgresql_mode_a_highlight_profile_is_chosen_for_any_session_there_is() {
        let h = highlighting().await;
        let mine = profile_in(&*h.profiles.0, "mine").await;
        let central = profile_in(&h.shared, "central").await;
        let id = h
            .shared
            .create_session(new_session(h.folder, "router", Some(central)))
            .await
            .unwrap()
            .id;

        set_session_highlight_profile(&h.shared, &h.profiles, Some(&h.local), id, Some(mine))
            .await
            .unwrap();
        assert_eq!(h.local.get(id).await.unwrap(), Some(mine));
        // The row is another user's, as far as this goes: it is not written.
        assert_eq!(row(&h.shared, id).await.highlight_profile_id, Some(central));

        // Not a profile of the shared database, and not for an ID that is
        // no session's.
        set_session_highlight_profile(&h.shared, &h.profiles, Some(&h.local), id, Some(central))
            .await
            .unwrap_err();
        let missing = Uuid::new_v4();
        set_session_highlight_profile(&h.shared, &h.profiles, Some(&h.local), missing, Some(mine))
            .await
            .unwrap_err();
        assert_eq!(h.local.get(id).await.unwrap(), Some(mine));
        assert_eq!(h.local.get(missing).await.unwrap(), None);

        set_session_highlight_profile(&h.shared, &h.profiles, Some(&h.local), id, None)
            .await
            .unwrap();
        assert_eq!(h.local.get(id).await.unwrap(), None);
    }

    #[tokio::test]
    async fn in_postgresql_mode_a_bulk_edit_chooses_the_highlight_profile_locally() {
        let h = highlighting().await;
        let mine = profile_in(&*h.profiles.0, "mine").await;
        let central = profile_in(&h.shared, "central").await;
        let below = h
            .shared
            .create_folder(NewFolder {
                name: "below".to_string(),
                parent_id: Some(h.folder),
            })
            .await
            .unwrap()
            .id;
        let elsewhere = h
            .shared
            .create_folder(NewFolder {
                name: "elsewhere".to_string(),
                parent_id: None,
            })
            .await
            .unwrap()
            .id;
        let mut ids = Vec::new();
        for (folder, name) in [(h.folder, "a"), (below, "b"), (elsewhere, "c")] {
            ids.push(
                h.shared
                    .create_session(new_session(folder, name, Some(central)))
                    .await
                    .unwrap()
                    .id,
            );
        }
        let (a, b, c) = (ids[0], ids[1], ids[2]);
        let edit = |highlight: Option<Uuid>, icon: Option<&str>| crate::db::BulkSessionEdit {
            highlight_profile_id: Some(highlight),
            icon: icon.map(str::to_string),
            ..Default::default()
        };
        // The SQLite provider writes the placeholder as every row's owner.
        let role = Some("local");

        let touched = bulk_edit_sessions(
            &h.shared,
            &h.profiles,
            Some(&h.local),
            role,
            h.folder,
            edit(Some(mine), Some("server")),
        )
        .await
        .unwrap();
        assert_eq!(touched, 2);
        for id in [a, b] {
            let session = row(&h.shared, id).await;
            assert_eq!(session.icon, "server");
            assert_eq!(session.highlight_profile_id, Some(central));
            assert_eq!(h.local.get(id).await.unwrap(), Some(mine));
        }
        assert_eq!(row(&h.shared, c).await.icon, "desktop");
        assert_eq!(h.local.get(c).await.unwrap(), None);

        // The profile alone: nothing goes to the shared database.
        let touched = bulk_edit_sessions(
            &h.shared,
            &h.profiles,
            Some(&h.local),
            role,
            below,
            edit(None, None),
        )
        .await
        .unwrap();
        assert_eq!(touched, 1);
        assert_eq!(h.local.get(a).await.unwrap(), Some(mine));
        assert_eq!(h.local.get(b).await.unwrap(), None);

        // Only the role's own sessions, and none while the role is not known.
        for role in [Some("someone else"), None] {
            let touched = bulk_edit_sessions(
                &h.shared,
                &h.profiles,
                Some(&h.local),
                role,
                h.folder,
                edit(None, None),
            )
            .await
            .unwrap();
            assert_eq!(touched, 0);
            assert_eq!(h.local.get(a).await.unwrap(), Some(mine));
        }

        // A profile that is not the user's changes nothing, the icon included.
        bulk_edit_sessions(
            &h.shared,
            &h.profiles,
            Some(&h.local),
            role,
            h.folder,
            edit(Some(central), Some("router")),
        )
        .await
        .unwrap_err();
        assert_eq!(row(&h.shared, a).await.icon, "server");
        assert_eq!(h.local.get(a).await.unwrap(), Some(mine));
    }

    #[tokio::test]
    async fn the_sessions_that_go_with_a_folder_are_found_before_it_is_deleted() {
        let h = highlighting().await;
        let below = h
            .shared
            .create_folder(NewFolder {
                name: "below".to_string(),
                parent_id: Some(h.folder),
            })
            .await
            .unwrap()
            .id;
        let a = h
            .shared
            .create_session(new_session(h.folder, "a", None))
            .await
            .unwrap()
            .id;
        let b = h
            .shared
            .create_session(new_session(below, "b", None))
            .await
            .unwrap()
            .id;
        let profile = Uuid::new_v4();
        h.local.set(&[a, b], Some(profile)).await.unwrap();

        let mut gone = visible_sessions_below(&h.shared, h.folder).await;
        gone.sort();
        let mut both = vec![a, b];
        both.sort();
        assert_eq!(gone, both);

        forget_highlights(&h.local, &[b]).await;
        assert_eq!(h.local.get(a).await.unwrap(), Some(profile));
        assert_eq!(h.local.get(b).await.unwrap(), None);
    }

    #[test]
    fn a_query_is_searched_for_by_its_first_200_bytes() {
        assert_eq!(capped_query(""), "");
        assert_eq!(capped_query("core"), "core");
        let exact = "a".repeat(MAX_SEARCH_QUERY_LEN);
        assert_eq!(capped_query(&exact), exact);
        let long = "a".repeat(MAX_SEARCH_QUERY_LEN + 50);
        assert_eq!(capped_query(&long), exact);
        // A character that begins before the limit is kept whole, and
        // nothing is cut inside one.
        let wide = format!("{}übc", "a".repeat(MAX_SEARCH_QUERY_LEN - 1));
        assert_eq!(
            capped_query(&wide),
            format!("{}ü", "a".repeat(MAX_SEARCH_QUERY_LEN - 1))
        );
    }

    fn names_found(results: &SearchResults) -> (Vec<&str>, Vec<&str>) {
        (
            results.folders.iter().map(|f| f.name.as_str()).collect(),
            results.sessions.iter().map(|s| s.name.as_str()).collect(),
        )
    }

    #[tokio::test]
    async fn a_search_finds_folders_by_name_and_sessions_by_their_own_fields() {
        let db = SqliteProvider::new(memory_pool("ON").await);
        let production = db
            .create_folder(NewFolder {
                name: "Production".to_string(),
                parent_id: None,
            })
            .await
            .unwrap()
            .id;
        let lab = db
            .create_folder(NewFolder {
                name: "lab".to_string(),
                parent_id: Some(production),
            })
            .await
            .unwrap()
            .id;
        db.create_session(new_session(production, "web", None))
            .await
            .unwrap();
        db.create_session(new_session(lab, "prod-db", None))
            .await
            .unwrap();
        let none: Vec<&str> = Vec::new();

        // The folder by its name, and the session by its own: the session
        // in the folder that was found is not a result itself.
        let found = search(&db, "PROD").await.unwrap();
        assert_eq!(names_found(&found), (vec!["Production"], vec!["prod-db"]));
        assert_eq!(found.folders[0].id, production);
        // A folder is found by its own name, not by the one above it.
        let found = search(&db, "lab").await.unwrap();
        assert_eq!(names_found(&found), (vec!["lab"], none.clone()));
        assert_eq!(found.folders[0].parent_id, Some(production));
        // A session's host is searched, and a folder has none.
        let found = search(&db, "web.example").await.unwrap();
        assert_eq!(names_found(&found), (none.clone(), vec!["web"]));
        // A query with a NUL in it finds neither.
        let found = search(&db, "prod\0").await.unwrap();
        assert_eq!(names_found(&found), (none.clone(), none.clone()));

        // The query is cut for both alike: what is past the limit keeps
        // neither the folder nor the session from being found.
        let long_name = "f".repeat(MAX_SEARCH_QUERY_LEN);
        db.create_folder(NewFolder {
            name: long_name.clone(),
            parent_id: None,
        })
        .await
        .unwrap();
        db.create_session(new_session(lab, &long_name, None))
            .await
            .unwrap();
        let found = search(&db, &format!("{long_name}-and-more")).await.unwrap();
        assert_eq!(
            names_found(&found),
            (vec![long_name.as_str()], vec![long_name.as_str()])
        );
        // Without the cut it finds neither.
        assert!(db
            .search_folders(&format!("{long_name}-and-more"))
            .await
            .unwrap()
            .is_empty());
    }

    /// Lets the tasks that were spawned run until search number `number`
    /// has asked `gate` for its turn. A task asks and begins to wait in one
    /// step on the runtime of a `#[tokio::test]`, which has one thread, so
    /// it waits by then.
    async fn until_asked(gate: &SearchGate, number: u64) {
        let asked = async {
            while *gate.asked.borrow() < number {
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), asked)
            .await
            .expect("the search asks for its turn");
    }

    /// Lets the tasks that were spawned go as far as they can.
    async fn let_run() {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    /// A search that has no answer until `answer` is sent or dropped.
    fn waiting_search() -> (
        tokio::sync::oneshot::Sender<()>,
        impl Future<Output = Result<u8, String>> + Send + 'static,
    ) {
        let (answer, answered) = tokio::sync::oneshot::channel::<()>();
        (answer, async move {
            answered.await.map_err(|e| e.to_string())?;
            Ok(7)
        })
    }

    #[tokio::test]
    async fn one_search_runs_at_a_time_and_of_those_that_wait_the_last() {
        let gate = Arc::new(SearchGate::default());
        // Nothing runs, so the first has its turn at once.
        let first = gate.turn().await.expect("the first search runs");

        // Three more ask while it runs, one after the other. Each says
        // whether it was run, and one that is keeps its turn until told.
        let (answers, mut answered) = tokio::sync::mpsc::unbounded_channel();
        let (finish, finished) = tokio::sync::watch::channel(false);
        let mut later = Vec::new();
        for number in 2..=4u64 {
            let asking = gate.clone();
            let answers = answers.clone();
            let mut finished = finished.clone();
            later.push(tokio::spawn(async move {
                let turn = asking.turn().await;
                answers.send((number, turn.is_some())).unwrap();
                if turn.is_some() {
                    finished.wait_for(|done| *done).await.unwrap();
                }
            }));
            until_asked(&gate, number).await;
            let_run().await;
            // The one that waited before it is answered now, while the
            // first still runs, and is not run. The one that asked last
            // waits.
            if number > 2 {
                assert_eq!(answered.try_recv(), Ok((number - 1, false)));
            }
            assert!(answered.try_recv().is_err());
        }

        // The first is done, and the last one runs.
        drop(first);
        assert_eq!(answered.recv().await, Some((4, true)));

        // One that asks while that one runs waits for it, and runs then:
        // nothing has asked after it.
        let fifth = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.turn().await.is_some() })
        };
        until_asked(&gate, 5).await;
        let_run().await;
        assert!(!fifth.is_finished());
        finish.send(true).unwrap();
        assert!(fifth.await.unwrap());
        for task in later {
            task.await.unwrap();
        }

        // And with nothing running, the next has its turn at once again.
        assert!(gate.turn().await.is_some());
    }

    #[tokio::test]
    async fn of_many_that_ask_one_waits_and_none_runs_beside_another() {
        let gate = Arc::new(SearchGate::default());
        let running = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let ran = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let order = std::sync::atomic::Ordering::SeqCst;
        let first = gate.turn().await.expect("the first search runs");

        let mut tasks = Vec::new();
        for number in 2..=50u64 {
            let asking = gate.clone();
            let running = running.clone();
            let ran = ran.clone();
            tasks.push(tokio::spawn(async move {
                let Some(_turn) = asking.turn().await else {
                    return;
                };
                assert_eq!(running.fetch_add(1, order), 0);
                let_run().await;
                running.fetch_sub(1, order);
                ran.fetch_add(1, order);
            }));
            until_asked(&gate, number).await;
        }
        // While the first runs, all that asked but the last have their
        // answer: one search waits, and not forty-nine.
        let_run().await;
        assert_eq!(tasks.iter().filter(|task| !task.is_finished()).count(), 1);
        assert_eq!(ran.load(order), 0);

        drop(first);
        for task in tasks {
            task.await.unwrap();
        }
        // The last one ran, and no other.
        assert_eq!(ran.load(order), 1);
    }

    #[tokio::test]
    async fn a_search_replaced_as_the_turn_comes_free_does_not_take_it() {
        // Which of two things that have both happened a search goes by is
        // the gate's choice, so this is tried many times over.
        for _ in 0..64 {
            let gate = Arc::new(SearchGate::default());
            let first = gate.turn().await.expect("the first search runs");
            let second = {
                let gate = gate.clone();
                tokio::spawn(async move { gate.turn().await.is_some() })
            };
            until_asked(&gate, 2).await;
            let_run().await;
            // A later search asks and the first is done, with nothing run
            // in between: the second finds both.
            gate.asked.send_modify(|last| *last += 1);
            drop(first);
            assert!(!second.await.unwrap());
        }
    }

    #[tokio::test]
    async fn a_search_keeps_its_turn_until_it_has_its_answer() {
        let gate = Arc::new(SearchGate::default());
        let limit = Duration::from_secs(5);
        let (answer, search) = waiting_search();
        let first = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.run(limit, search).await })
        };
        until_asked(&gate, 1).await;
        let_run().await;

        // The next one waits for it, and is answered once the first is.
        let second = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.run(limit, async { Ok(8) }).await })
        };
        until_asked(&gate, 2).await;
        let_run().await;
        assert!(!first.is_finished());
        assert!(!second.is_finished());

        answer.send(()).unwrap();
        assert_eq!(first.await.unwrap(), Ok(7));
        assert_eq!(second.await.unwrap(), Ok(8));

        // One that waits when a later one asks is told so, and is not run.
        let (answer, search) = waiting_search();
        let third = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.run(limit, search).await })
        };
        until_asked(&gate, 3).await;
        let_run().await;
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fourth = {
            let gate = gate.clone();
            let ran = ran.clone();
            tokio::spawn(async move {
                gate.run(limit, async move {
                    ran.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(9)
                })
                .await
            })
        };
        until_asked(&gate, 4).await;
        let_run().await;
        let fifth = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.run(limit, async { Ok(10) }).await })
        };
        until_asked(&gate, 5).await;
        assert_eq!(fourth.await.unwrap(), Err(SEARCH_REPLACED.to_string()));
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
        answer.send(()).unwrap();
        assert_eq!(third.await.unwrap(), Ok(7));
        assert_eq!(fifth.await.unwrap(), Ok(10));
    }

    #[tokio::test]
    async fn a_search_is_answered_with_what_it_found_or_how_it_failed() {
        let gate = SearchGate::default();
        let limit = Duration::from_secs(5);
        assert_eq!(gate.run(limit, async { Ok(7) }).await, Ok(7));
        let failed: Result<u8, String> = gate
            .run(limit, async { Err("Failed to search folders".to_string()) })
            .await;
        assert_eq!(failed, Err("Failed to search folders".to_string()));

        // One that panics is an error, without what it panicked with, and
        // not a panic of the command. It has ended, so it holds up nothing.
        let panicked: Result<u8, String> = gate
            .run(limit, async { panic!("with a secret in the message") })
            .await;
        assert_eq!(panicked, Err(SEARCH_FAILED.to_string()));
        assert_eq!(gate.unended.available_permits(), SEARCHES_UNENDED_AT_MOST);
        assert_eq!(gate.run(limit, async { Ok(7) }).await, Ok(7));
    }

    #[tokio::test]
    async fn a_search_without_an_answer_is_given_up_and_left_to_end_by_itself() {
        let gate = SearchGate::default();
        let limit = Duration::from_millis(50);
        let (answer, search) = waiting_search();
        let (ended, mut has_ended) = tokio::sync::mpsc::unbounded_channel();
        let given_up = gate
            .run(limit, async move {
                let found = search.await;
                ended.send(()).unwrap();
                found
            })
            .await;
        assert_eq!(
            given_up,
            Err("Search given up: the database has not answered within 50ms".to_string())
        );

        // Its turn has ended: the next search runs, beside it.
        assert_eq!(
            gate.unended.available_permits(),
            SEARCHES_UNENDED_AT_MOST - 1
        );
        assert_eq!(
            gate.run(Duration::from_secs(5), async { Ok(8) }).await,
            Ok(8)
        );

        // It was not dropped: once the database answers after all, it goes
        // on to its end, and counts no longer.
        answer.send(()).expect("the search is still there");
        assert_eq!(has_ended.recv().await, Some(()));
        let_run().await;
        assert_eq!(gate.unended.available_permits(), SEARCHES_UNENDED_AT_MOST);
    }

    #[tokio::test]
    async fn no_more_searches_are_left_unended_than_the_gate_allows() {
        let gate = SearchGate::default();
        let limit = Duration::from_millis(20);
        // As many as may be unended are given up, one after the other.
        let mut answers = Vec::new();
        for _ in 0..SEARCHES_UNENDED_AT_MOST {
            let (answer, search) = waiting_search();
            answers.push(answer);
            let given_up = gate.run(limit, search).await;
            assert!(given_up.unwrap_err().starts_with("Search given up: "));
        }

        // The next one is refused, and is not run.
        let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let refused: Result<u8, String> = {
            let ran = ran.clone();
            gate.run(limit, async move {
                ran.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(7)
            })
            .await
        };
        assert_eq!(refused, Err(SEARCH_REFUSED.to_string()));
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));

        // One of them ends, as when its connection is closed at last, and a
        // search runs again.
        drop(answers.pop());
        let ended = async {
            while gate.unended.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), ended)
            .await
            .expect("the search ends");
        assert_eq!(
            gate.run(Duration::from_secs(5), async { Ok(7) }).await,
            Ok(7)
        );
    }
}
