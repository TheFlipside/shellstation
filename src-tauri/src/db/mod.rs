pub mod models;
pub mod postgres;
pub mod session_highlights;
pub mod sqlite;
pub mod tree_order;
pub mod update_builder;

use std::cmp::Ordering;
use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use models::{
    Credential, CredentialProfile, DataFingerprint, Folder, HighlightProfile, LoginSequence,
    NewCredentialProfile, NewFolder, NewHighlightProfile, NewLoginSequence, NewSession, Session,
    UpdateCredentialProfile, UpdateHighlightProfile, UpdateLoginSequence, UpdateSession,
};

pub type DbResult<T> = Result<T, String>;

/// The `LIKE` pattern of a search for sessions or folders: `query` anywhere in the text, and
/// taken as written. `%` and `_` are a pattern's wildcards, and `\` is the
/// character the search statements name as its escape, so each of the three
/// gets a `\` before it. Without one before the `\` itself, a query with a
/// backslash in it, as a user name with a domain has (`CORP\admin`), was read
/// as an escape of the character after it and found nothing.
///
/// `None` for a query with a NUL in it, which finds nothing. `PostgreSQL` has
/// no text with one and refuses it as a parameter, and `SQLite` reads a
/// pattern up to its first NUL, so it would search for the part before it.
pub(crate) fn search_pattern(query: &str) -> Option<String> {
    if query.contains('\0') {
        return None;
    }
    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for c in query.chars() {
        if matches!(c, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    Some(pattern)
}

/// Compare two hostnames with numeric IP-address awareness.
/// IPv4 addresses sort before IPv6 (enum variant order of `IpAddr`), both
/// sort before non-IP hostnames.  Among IPs, octets/hextets are compared
/// numerically (so 10.0.0.2 < 10.0.0.10).  Non-IP hostnames fall back to
/// case-insensitive lexicographic order.
pub fn cmp_hostname(a: &str, b: &str) -> Ordering {
    match (a.parse::<IpAddr>(), b.parse::<IpAddr>()) {
        (Ok(ia), Ok(ib)) => ia.cmp(&ib),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => a.to_lowercase().cmp(&b.to_lowercase()),
    }
}

/// Fields that can be updated in a bulk edit. `None` means "leave alone"; for
/// nullable columns, `Some(None)` means "clear to NULL".
#[derive(Debug, Default)]
pub struct BulkSessionEdit {
    pub jump_host_id: Option<Option<Uuid>>,
    pub highlight_profile_id: Option<Option<Uuid>>,
    pub login_sequence_id: Option<Option<Uuid>>,
    pub icon: Option<String>,
}

/// Tauri managed state wrapping the database provider (folders + sessions).
pub struct DbState(pub Arc<dyn DatabaseProvider>);

/// Tauri managed state wrapping the local credential provider.
/// Credentials are always stored locally (never in a shared central DB)
/// so each user keeps their own secrets.
pub struct CredentialDbState(pub Arc<dyn DatabaseProvider>);

/// Tauri managed state wrapping the local login sequence provider.
/// Login sequences are always stored locally, same as credentials.
pub struct LoginSequenceDbState(pub Arc<dyn DatabaseProvider>);

/// Tauri managed state wrapping the provider that keeps the highlight
/// profiles. In `SQLite` mode that is the one database. In `PostgreSQL` mode
/// it is the local one, as for credentials: a profile's patterns run in the
/// window of whoever uses it, so each user keeps their own, out of reach of
/// the other roles of a shared database. Which profile a session is shown
/// with is then kept locally as well, see [`session_highlights`].
pub struct HighlightDbState(pub Arc<dyn DatabaseProvider>);

/// Direct access to the `PostgreSQL` connection pool for operations that
/// bypass the `DatabaseProvider` trait (e.g. `session_credentials` table).
/// `None` in `SQLite` mode.
pub struct PgPoolState(pub Option<sqlx::PgPool>);

/// The `PostgreSQL` `current_user` at connection time. Used by the frontend
/// to determine item ownership, and by `session_connect` to tell the user's
/// own sessions from other users' shared ones. `None` in `SQLite` mode.
pub struct PgUserState(pub Option<String>);

#[async_trait]
pub trait DatabaseProvider: Send + Sync {
    // ── Folders ──────────────────────────────────────────────────────────

    async fn create_folder(&self, folder: NewFolder) -> DbResult<Folder>;
    async fn list_folders(&self) -> DbResult<Vec<Folder>>;

    /// The folders in `parent` (`None`: the top level) that can be seen, by
    /// ID with their owner. A provider for a large shared database reads
    /// only that place; this one reads every folder.
    async fn folder_owners_in(&self, parent: Option<Uuid>) -> DbResult<Vec<(Uuid, String)>> {
        Ok(self
            .list_folders()
            .await?
            .into_iter()
            .filter(|f| f.parent_id == parent)
            .map(|f| (f.id, f.owner))
            .collect())
    }

    /// The sessions in `folder` that can be seen, by ID with their owner, as
    /// [`Self::folder_owners_in`] has the folders.
    async fn session_owners_in(&self, folder: Uuid) -> DbResult<Vec<(Uuid, String)>> {
        Ok(self
            .list_all_sessions()
            .await?
            .into_iter()
            .filter(|s| s.folder_id == folder)
            .map(|s| (s.id, s.owner))
            .collect())
    }
    /// The folders whose name has `query` in it, by the rules of
    /// [`search_pattern`] and in the order of [`Self::list_folders`]. Only a
    /// folder's own name is looked at, not the names of the folders above it.
    async fn search_folders(&self, query: &str) -> DbResult<Vec<Folder>>;
    async fn rename_folder(&self, id: Uuid, name: &str) -> DbResult<()>;
    async fn move_folder(&self, id: Uuid, new_parent_id: Option<Uuid>) -> DbResult<()>;
    async fn delete_folder(&self, id: Uuid) -> DbResult<()>;

    // ── Sessions ─────────────────────────────────────────────────────────

    async fn create_session(&self, session: NewSession) -> DbResult<Session>;
    async fn get_session(&self, id: Uuid) -> DbResult<Option<Session>>;
    async fn list_all_sessions(&self) -> DbResult<Vec<Session>>;
    async fn update_session(&self, id: Uuid, update: UpdateSession) -> DbResult<()>;
    async fn move_session(&self, id: Uuid, new_folder_id: Uuid) -> DbResult<()>;
    async fn delete_session(&self, id: Uuid) -> DbResult<()>;
    async fn search_sessions(&self, query: &str) -> DbResult<Vec<Session>>;
    /// What a search for `query` finds: the folders of
    /// [`Self::search_folders`] and the sessions of
    /// [`Self::search_sessions`], read one after the other. This is what the
    /// app searches by. As it is here it sets no limit to the two reads,
    /// which is how `SQLite` is searched, where the database is the user's
    /// own file. The `PostgreSQL` provider has its own, in which the two
    /// statements have a time limit shorter than the one the app's other
    /// statements have; a provider for a database that is shared needs the
    /// same.
    async fn search(&self, query: &str) -> DbResult<(Vec<Folder>, Vec<Session>)> {
        let folders = self.search_folders(query).await?;
        let sessions = self.search_sessions(query).await?;
        Ok((folders, sessions))
    }

    // ── Reordering ───────────────────────────────────────────────────────

    /// Persist a custom order for sibling folders under the given parent.
    /// `ordered_ids` contains every folder `ID` in the desired sequence. In
    /// `PostgreSQL` mode only the user's own folders are numbered, by their
    /// place in it; where other users' come among them is kept locally
    /// (`tree_order`).
    async fn reorder_folders(
        &self,
        parent_id: Option<Uuid>,
        ordered_ids: Vec<Uuid>,
    ) -> DbResult<()>;

    /// Persist a custom order for sessions inside the given folder.
    async fn reorder_sessions(&self, folder_id: Uuid, ordered_ids: Vec<Uuid>) -> DbResult<()>;

    /// Reset `sort_order` for sibling folders so they appear alphabetically.
    /// Returns the folders there in that order, as a reorder takes them:
    /// in `PostgreSQL` mode other users' as well, which are not numbered.
    async fn sort_folders_alphabetically(&self, parent_id: Option<Uuid>) -> DbResult<Vec<Uuid>>;

    /// Reset `sort_order` for sessions inside a folder so they appear
    /// alphabetically. Returns them in that order, as
    /// [`Self::sort_folders_alphabetically`] does.
    async fn sort_sessions_alphabetically(&self, folder_id: Uuid) -> DbResult<Vec<Uuid>>;

    /// Reset `sort_order` for sessions inside a folder so they appear sorted by
    /// hostname, with proper numeric ordering for IP addresses. Returns them
    /// in that order, as [`Self::sort_folders_alphabetically`] does.
    async fn sort_sessions_by_hostname(&self, folder_id: Uuid) -> DbResult<Vec<Uuid>>;

    // ── Credentials (metadata — secrets live in OS keychain) ─────────────
    //
    // Legacy per-session credentials. These methods remain only so that the
    // one-shot `migrate_legacy_credentials` routine can drain the old table
    // into `credential_profiles`. Do not call from new code.

    async fn upsert_credential(&self, cred: Credential) -> DbResult<()>;
    #[allow(dead_code)]
    async fn get_credential(&self, session_id: Uuid) -> DbResult<Option<Credential>>;
    async fn delete_credential(&self, session_id: Uuid) -> DbResult<()>;
    async fn list_all_credentials(&self) -> DbResult<Vec<Credential>>;

    // ── Credential Profiles ──────────────────────────────────────────────

    async fn create_credential_profile(
        &self,
        profile: NewCredentialProfile,
    ) -> DbResult<CredentialProfile>;
    async fn list_credential_profiles(&self) -> DbResult<Vec<CredentialProfile>>;
    async fn get_credential_profile(&self, id: Uuid) -> DbResult<Option<CredentialProfile>>;
    async fn update_credential_profile(
        &self,
        id: Uuid,
        update: UpdateCredentialProfile,
    ) -> DbResult<()>;
    async fn delete_credential_profile(&self, id: Uuid) -> DbResult<()>;

    /// Assign `profile_id` to every session in `folder_id` and its descendants.
    /// Telnet sessions are skipped. Returns the number of sessions updated.
    async fn bulk_set_session_credential_profile(
        &self,
        folder_id: Uuid,
        profile_id: Option<Uuid>,
    ) -> DbResult<u32>;

    /// Bulk-update optional session fields across `folder_id` and all its
    /// descendants. Each field is set only when its outer `Option` is `Some`;
    /// the inner `Option` distinguishes "clear to NULL" from "leave alone"
    /// for nullable columns. `jump_host_id` is skipped for telnet sessions.
    /// Returns the number of sessions touched.
    async fn bulk_edit_sessions(&self, folder_id: Uuid, edit: BulkSessionEdit) -> DbResult<u32>;

    // ── Highlight Profiles ────────────────────────────────────────────────

    async fn create_highlight_profile(
        &self,
        profile: NewHighlightProfile,
    ) -> DbResult<HighlightProfile>;
    async fn list_highlight_profiles(&self) -> DbResult<Vec<HighlightProfile>>;
    async fn get_highlight_profile(&self, id: Uuid) -> DbResult<Option<HighlightProfile>>;
    async fn update_highlight_profile(
        &self,
        id: Uuid,
        update: UpdateHighlightProfile,
    ) -> DbResult<()>;
    async fn delete_highlight_profile(&self, id: Uuid) -> DbResult<()>;

    // ── Login Sequences ───────────────────────────────────────────────

    async fn create_login_sequence(&self, sequence: NewLoginSequence) -> DbResult<LoginSequence>;
    async fn list_login_sequences(&self) -> DbResult<Vec<LoginSequence>>;
    async fn get_login_sequence(&self, id: Uuid) -> DbResult<Option<LoginSequence>>;
    async fn update_login_sequence(&self, id: Uuid, update: UpdateLoginSequence) -> DbResult<()>;
    async fn delete_login_sequence(&self, id: Uuid) -> DbResult<()>;

    /// Assign `sequence_id` to every session in `folder_id` and its descendants.
    /// Returns the number of sessions updated.
    async fn bulk_set_session_login_sequence(
        &self,
        folder_id: Uuid,
        sequence_id: Option<Uuid>,
    ) -> DbResult<u32>;

    /// Return a lightweight fingerprint derived from row counts and a hash of
    /// all folder/session IDs and names.  The frontend polls this to decide
    /// whether a full `loadAll()` is needed, avoiding the cost of serialising
    /// and transferring thousands of rows every few seconds.
    async fn data_fingerprint(&self) -> DbResult<DataFingerprint>;
}
