use tauri::State;
use uuid::Uuid;

use crate::db::models::{CredentialProfile, NewCredentialProfile, UpdateCredentialProfile};
use crate::db::CredentialDbState;

/// Maximum lengths for credential profile string fields.
const MAX_NAME_LEN: usize = 128;
const MAX_USERNAME_LEN: usize = 128;
const MAX_KEY_PATH_LEN: usize = 4096;
const MAX_SECRET_LEN: usize = 16384;

fn parse_uuid(s: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

fn validate_auth_type(auth_type: &str) -> Result<(), String> {
    match auth_type {
        "password" | "key" | "keyboard-interactive" => Ok(()),
        other => Err(format!("Unsupported auth type: {other}")),
    }
}

fn validate_fields(
    name: Option<&str>,
    username: Option<&str>,
    key_path: Option<&str>,
    secret: Option<&str>,
) -> Result<(), String> {
    if let Some(v) = name {
        if v.is_empty() || v.len() > MAX_NAME_LEN {
            return Err(format!("Profile name must be 1–{MAX_NAME_LEN} characters"));
        }
    }
    if let Some(v) = username {
        if v.len() > MAX_USERNAME_LEN {
            return Err(format!(
                "Username too long (max {MAX_USERNAME_LEN} characters)"
            ));
        }
    }
    if let Some(v) = key_path {
        if v.len() > MAX_KEY_PATH_LEN {
            return Err(format!(
                "Key path too long (max {MAX_KEY_PATH_LEN} characters)"
            ));
        }
    }
    if let Some(v) = secret {
        if v.len() > MAX_SECRET_LEN {
            return Err(format!("Secret too long (max {MAX_SECRET_LEN} characters)"));
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn credential_profile_create(
    cred_db: State<'_, CredentialDbState>,
    name: String,
    auth_type: String,
    username: String,
    key_path: String,
    secret: String,
) -> Result<CredentialProfile, String> {
    validate_auth_type(&auth_type)?;
    validate_fields(Some(&name), Some(&username), Some(&key_path), Some(&secret))?;

    let profile = cred_db
        .0
        .create_credential_profile(NewCredentialProfile {
            name,
            auth_type,
            username,
            key_path,
        })
        .await?;

    if !secret.is_empty() {
        if let Err(e) = crate::credentials::store(&profile.keychain_ref, &secret) {
            // Roll back the DB row so the user can retry without leaving a
            // stub profile behind. If the rollback itself fails we still
            // return the original error, but log the dangling-row condition
            // so it can be investigated.
            if let Err(rollback_err) = cred_db.0.delete_credential_profile(profile.id).await {
                tracing::error!(
                    profile_id = %profile.id,
                    "failed to roll back credential profile after keychain store error: {rollback_err}"
                );
            }
            return Err(format!("Failed to store secret in OS keychain: {e}"));
        }
    }

    Ok(profile)
}

#[tauri::command]
pub async fn credential_profile_list(
    cred_db: State<'_, CredentialDbState>,
) -> Result<Vec<CredentialProfile>, String> {
    cred_db.0.list_credential_profiles().await
}

#[tauri::command]
pub async fn credential_profile_get_secret(
    cred_db: State<'_, CredentialDbState>,
    id: String,
) -> Result<String, String> {
    let uuid = parse_uuid(&id)?;
    let profile = cred_db
        .0
        .get_credential_profile(uuid)
        .await?
        .ok_or_else(|| format!("Credential profile {id} not found"))?;
    crate::credentials::retrieve(&profile.keychain_ref)
        .map_err(|e| format!("Failed to retrieve secret from keychain: {e}"))
}

#[tauri::command]
pub async fn credential_profile_update(
    cred_db: State<'_, CredentialDbState>,
    id: String,
    name: Option<String>,
    auth_type: Option<String>,
    username: Option<String>,
    key_path: Option<String>,
    secret: Option<String>,
) -> Result<(), String> {
    if let Some(ref at) = auth_type {
        validate_auth_type(at)?;
    }
    validate_fields(
        name.as_deref(),
        username.as_deref(),
        key_path.as_deref(),
        secret.as_deref(),
    )?;

    let uuid = parse_uuid(&id)?;
    let existing = cred_db
        .0
        .get_credential_profile(uuid)
        .await?
        .ok_or_else(|| format!("Credential profile {id} not found"))?;

    cred_db
        .0
        .update_credential_profile(
            uuid,
            UpdateCredentialProfile {
                name,
                auth_type,
                username,
                key_path,
            },
        )
        .await?;

    if let Some(new_secret) = secret {
        if let Err(e) = crate::credentials::store(&existing.keychain_ref, &new_secret) {
            return Err(format!("Failed to store secret in OS keychain: {e}"));
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn credential_profile_delete(
    cred_db: State<'_, CredentialDbState>,
    id: String,
) -> Result<(), String> {
    let uuid = parse_uuid(&id)?;
    // Fetch keychain_ref before deleting the row so we can clean up.
    if let Ok(Some(profile)) = cred_db.0.get_credential_profile(uuid).await {
        let _ = crate::credentials::delete(&profile.keychain_ref);
    }
    cred_db.0.delete_credential_profile(uuid).await
}

#[tauri::command]
pub async fn folder_apply_credential_profile(
    db: State<'_, crate::db::DbState>,
    folder_id: String,
    profile_id: Option<String>,
) -> Result<u32, String> {
    let folder_uuid = parse_uuid(&folder_id)?;
    let profile_uuid = profile_id.as_deref().map(parse_uuid).transpose()?;

    // The sessions DB holds credential_profile_id; in PostgreSQL mode that is
    // the central DB. The profile row itself lives in each user's local
    // SQLite (CredentialDbState), and referential integrity is soft — the
    // connect flow rejects sessions whose profile has been deleted.
    db.0.bulk_set_session_credential_profile(folder_uuid, profile_uuid)
        .await
}
