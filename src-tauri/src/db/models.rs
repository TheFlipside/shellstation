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
    pub highlight_profile_id: Option<Uuid>,
    #[serde(default)]
    pub credential_profile_id: Option<Uuid>,
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
    pub highlight_profile_id: Option<Uuid>,
    #[serde(default)]
    pub credential_profile_id: Option<Uuid>,
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
    pub highlight_profile_id: Option<Option<Uuid>>,
    pub credential_profile_id: Option<Option<Uuid>>,
}

// ── Credential Profiles (shared, not per-session) ────────────────────────

/// A named credential profile. One row in the DB; one entry in the OS
/// keychain under `keychain_ref`. Sessions reference a profile by id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialProfile {
    pub id: Uuid,
    pub name: String,
    /// `password`, `key`, or `keyboard-interactive`.
    pub auth_type: String,
    pub username: String,
    pub keychain_ref: String,
    /// For `key` profiles: filesystem path to the private key. Empty
    /// otherwise.
    pub key_path: String,
    pub sort_order: i32,
}

#[derive(Debug, Deserialize)]
pub struct NewCredentialProfile {
    pub name: String,
    pub auth_type: String,
    pub username: String,
    pub key_path: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateCredentialProfile {
    pub name: Option<String>,
    pub auth_type: Option<String>,
    pub username: Option<String>,
    pub key_path: Option<String>,
}

/// Credential metadata stored in the database. The actual secret (password or
/// key path) is stored in the OS keychain via `keychain_ref`, never in the DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    pub id: Uuid,
    pub session_id: Uuid,
    pub username: String,
    pub auth_type: String,
    pub keychain_ref: String,
}

// ── Highlight Profiles ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighlightProfile {
    pub id: Uuid,
    pub name: String,
    pub rules: String,
    pub sort_order: i32,
}

#[derive(Debug, Deserialize)]
pub struct NewHighlightProfile {
    pub name: String,
    pub rules: String,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateHighlightProfile {
    pub name: Option<String>,
    pub rules: Option<String>,
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
    #[serde(default)]
    pub credentials: Vec<Credential>,
    #[serde(default)]
    pub highlight_profiles: Vec<HighlightProfile>,
    #[serde(default)]
    pub credential_profiles: Vec<CredentialProfile>,
}
