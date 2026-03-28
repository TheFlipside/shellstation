use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
    pub sort_order: i32,
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
    pub icon: String,
    pub sort_order: i32,
}

#[derive(Debug, Deserialize)]
pub struct NewSession {
    pub folder_id: Uuid,
    pub name: String,
    pub hostname: String,
    pub port: i32,
    pub protocol: String,
    pub auth_method: String,
    pub jump_host_id: Option<Uuid>,
    pub tags: String,
    pub icon: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateSession {
    pub name: Option<String>,
    pub hostname: Option<String>,
    pub port: Option<i32>,
    pub protocol: Option<String>,
    pub auth_method: Option<String>,
    pub jump_host_id: Option<Option<Uuid>>,
    pub tags: Option<String>,
    pub icon: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub id: Uuid,
    pub session_id: Uuid,
    pub username: String,
    pub auth_type: String,
    pub keychain_ref: String,
    /// The actual secret (password or key path). Stored in DB for now;
    /// will migrate to OS keychain once platform backends are sorted.
    #[serde(skip_serializing)]
    pub secret: String,
}

/// Credential with secret included in serialization, for export/import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportCredential {
    pub id: Uuid,
    pub session_id: Uuid,
    pub username: String,
    pub auth_type: String,
    pub keychain_ref: String,
    pub secret: String,
}

impl From<Credential> for ExportCredential {
    fn from(c: Credential) -> Self {
        Self {
            id: c.id,
            session_id: c.session_id,
            username: c.username,
            auth_type: c.auth_type,
            keychain_ref: c.keychain_ref,
            secret: c.secret,
        }
    }
}

impl From<ExportCredential> for Credential {
    fn from(c: ExportCredential) -> Self {
        Self {
            id: c.id,
            session_id: c.session_id,
            username: c.username,
            auth_type: c.auth_type,
            keychain_ref: c.keychain_ref,
            secret: c.secret,
        }
    }
}

/// Lightweight fingerprint for polling-based change detection.
/// The frontend compares this string across polls — if it changes,
/// a full `loadAll()` is triggered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataFingerprint {
    pub hash: String,
}

/// Complete database export for migration between backends.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExportData {
    pub folders: Vec<Folder>,
    pub sessions: Vec<Session>,
    pub credentials: Vec<ExportCredential>,
}
