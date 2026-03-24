use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct NewFolder {
    pub name: String,
    pub parent_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    pub folder_id: Uuid,
    pub name: String,
    pub hostname: String,
    pub port: i32,
    pub protocol: String,
    pub username: String,
    pub auth_method: String,
    pub jump_host_id: Option<Uuid>,
    pub tags: String,
}

#[derive(Debug, Deserialize)]
pub struct NewSession {
    pub folder_id: Uuid,
    pub name: String,
    pub hostname: String,
    pub port: i32,
    pub protocol: String,
    pub username: String,
    pub auth_method: String,
    pub jump_host_id: Option<Uuid>,
    pub tags: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSession {
    pub name: Option<String>,
    pub hostname: Option<String>,
    pub port: Option<i32>,
    pub username: Option<String>,
    pub auth_method: Option<String>,
    pub jump_host_id: Option<Option<Uuid>>,
    pub tags: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub id: Uuid,
    pub session_id: Uuid,
    pub auth_type: String,
    pub keychain_ref: String,
    /// The actual secret (password or key path). Stored in DB for now;
    /// will migrate to OS keychain once platform backends are sorted.
    #[serde(skip_serializing)]
    pub secret: String,
}
