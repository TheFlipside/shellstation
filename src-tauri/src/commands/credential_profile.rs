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

/// The type of a profile that has a secret in the keychain. A key is named
/// by its path and a keyboard-interactive login asks for what it needs, and
/// no connection with either reads the keychain. A passphrase for a key is
/// not kept for a profile: whoever adds that has this rule to change.
const PASSWORD: &str = "password";

/// What a change of a profile does to the secret it has in the keychain.
#[derive(Debug, PartialEq, Eq)]
enum SecretChange<'a> {
    /// Write this one, in place of what is there.
    Store(&'a str),
    /// Take away what is there.
    Delete,
    /// Leave it as it is.
    Keep,
}

/// What becomes of a profile's secret when a profile of the type `was` is
/// saved as one of the type `becomes`, with `secret` if one was sent.
///
/// A password is a password profile's. One that is sent with a profile of
/// another type is not stored, and a password profile that becomes one of
/// another type loses the password it had: it would lie in the keychain
/// where no connection reads it, until the profile is deleted. A profile
/// that was no password profile has nothing there that this would know of,
/// so the keychain is not asked, which on some systems asks the user.
///
/// An empty secret is none: a password profile keeps the one it has, as a
/// new profile is given none for it.
fn secret_change<'a>(was: &str, becomes: &str, secret: Option<&'a str>) -> SecretChange<'a> {
    if becomes == PASSWORD {
        return secret
            .filter(|sent| !sent.is_empty())
            .map_or(SecretChange::Keep, SecretChange::Store);
    }
    if was == PASSWORD {
        SecretChange::Delete
    } else {
        SecretChange::Keep
    }
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
    // A secret that comes with a profile of another type is not stored.
    let stores_secret = auth_type == PASSWORD && !secret.is_empty();

    let profile = cred_db
        .0
        .create_credential_profile(NewCredentialProfile {
            name,
            auth_type,
            username,
            key_path,
        })
        .await?;

    if stores_secret {
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
    // Not between the two steps of a change of the profile.
    let _entry = super::entry_lock::hold(uuid).await;
    let profile = cred_db
        .0
        .get_credential_profile(uuid)
        .await?
        .ok_or_else(|| format!("Credential profile {id} not found"))?;
    crate::credentials::retrieve(&profile.keychain_ref)
        .map(|z| (*z).clone())
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
    // One change of the profile at a time, its row and its secret together.
    let _entry = super::entry_lock::hold(uuid).await;
    let existing = cred_db
        .0
        .get_credential_profile(uuid)
        .await?
        .ok_or_else(|| format!("Credential profile {id} not found"))?;
    let becomes = auth_type.as_deref().unwrap_or(&existing.auth_type);
    let change = secret_change(&existing.auth_type, becomes, secret.as_deref());

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

    match change {
        SecretChange::Store(new_secret) => {
            if let Err(e) = crate::credentials::store(&existing.keychain_ref, new_secret) {
                return Err(format!("Failed to store secret in OS keychain: {e}"));
            }
        }
        SecretChange::Delete => {
            if let Err(e) = crate::credentials::delete_if_present(&existing.keychain_ref) {
                // The password is still there, with a profile that is no
                // password profile by now, and no later change of it would
                // take the password away. The row is put back as it was,
                // so that nothing is changed and the change can be tried
                // again. The row is changed first so that one that cannot
                // be changed, as with a name that is taken, has cost no
                // password.
                let undone = cred_db
                    .0
                    .update_credential_profile(
                        uuid,
                        UpdateCredentialProfile {
                            name: Some(existing.name),
                            auth_type: Some(existing.auth_type),
                            username: Some(existing.username),
                            key_path: Some(existing.key_path),
                        },
                    )
                    .await;
                return Err(match undone {
                    Ok(()) => {
                        format!("Failed to delete the stored password from the OS keychain: {e}")
                    }
                    Err(undo) => {
                        tracing::error!(
                            profile_id = %uuid,
                            "a profile was changed to another type, its password could not \
                             be deleted from the keychain, and the change could not be undone: {undo}"
                        );
                        format!(
                            "The profile was changed, but its stored password could not be \
                             deleted from the OS keychain ({e}), and the change could not be \
                             undone: {undo}"
                        )
                    }
                });
            }
        }
        SecretChange::Keep => {}
    }

    Ok(())
}

#[tauri::command]
pub async fn credential_profile_delete(
    cred_db: State<'_, CredentialDbState>,
    id: String,
) -> Result<(), String> {
    let uuid = parse_uuid(&id)?;
    // Not while a change of the profile is under way.
    let _entry = super::entry_lock::hold(uuid).await;
    // Fetch keychain_ref before deleting the row so we can clean up. A
    // profile without a secret, as one with a key, has nothing there, which
    // is no failure and is not logged as one.
    // A keychain that refuses is logged and does not keep the profile from
    // being deleted: its secret is then left there, under a name nothing
    // refers to.
    if let Ok(Some(profile)) = cred_db.0.get_credential_profile(uuid).await {
        let _ = crate::credentials::delete_if_present(&profile.keychain_ref);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_password_profile_stores_the_secret_it_is_sent_and_keeps_its_own_without_one() {
        assert_eq!(
            secret_change("password", "password", Some("new")),
            SecretChange::Store("new")
        );
        assert_eq!(
            secret_change("password", "password", None),
            SecretChange::Keep
        );
        // One that becomes a password profile has what it is sent, or none.
        assert_eq!(
            secret_change("key", "password", Some("new")),
            SecretChange::Store("new")
        );
        assert_eq!(secret_change("key", "password", None), SecretChange::Keep);
    }

    #[test]
    fn an_empty_secret_is_none() {
        for was in ["password", "key", "keyboard-interactive"] {
            assert_eq!(secret_change(was, "password", Some("")), SecretChange::Keep);
        }
        // And for a password profile that becomes another, nothing to keep.
        assert_eq!(
            secret_change("password", "key", Some("")),
            SecretChange::Delete
        );
    }

    #[test]
    fn a_password_profile_that_becomes_another_loses_its_password() {
        for becomes in ["key", "keyboard-interactive"] {
            assert_eq!(
                secret_change("password", becomes, None),
                SecretChange::Delete
            );
            // Also when a password is sent along: it is not stored instead.
            assert_eq!(
                secret_change("password", becomes, Some("sent along")),
                SecretChange::Delete
            );
        }
    }

    #[test]
    fn a_profile_that_was_no_password_profile_leaves_the_keychain_alone() {
        for was in ["key", "keyboard-interactive"] {
            for becomes in ["key", "keyboard-interactive"] {
                assert_eq!(secret_change(was, becomes, None), SecretChange::Keep);
                assert_eq!(
                    secret_change(was, becomes, Some("sent along")),
                    SecretChange::Keep
                );
            }
        }
    }
}
