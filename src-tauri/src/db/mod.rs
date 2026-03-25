pub mod models;
pub mod postgres;
pub mod sqlite;

use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use models::{Credential, Folder, NewFolder, NewSession, Session, UpdateSession};

pub type DbResult<T> = Result<T, String>;

/// Tauri managed state wrapping the database provider (folders + sessions).
pub struct DbState(pub Arc<dyn DatabaseProvider>);

/// Tauri managed state wrapping the local credential provider.
/// Credentials are always stored locally (never in a shared central DB)
/// so each user keeps their own secrets.
pub struct CredentialDbState(pub Arc<dyn DatabaseProvider>);

#[async_trait]
pub trait DatabaseProvider: Send + Sync {
    // ── Folders ──────────────────────────────────────────────────────────

    async fn create_folder(&self, folder: NewFolder) -> DbResult<Folder>;
    async fn list_folders(&self) -> DbResult<Vec<Folder>>;
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

    // ── Credentials (metadata — secrets live in OS keychain) ─────────────

    async fn upsert_credential(&self, cred: Credential) -> DbResult<()>;
    async fn get_credential(&self, session_id: Uuid) -> DbResult<Option<Credential>>;
    #[allow(dead_code)]
    async fn delete_credential(&self, session_id: Uuid) -> DbResult<()>;
    async fn list_all_credentials(&self) -> DbResult<Vec<Credential>>;
}
