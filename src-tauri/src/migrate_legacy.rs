//! One-shot migration from per-session credentials to shared credential
//! profiles.
//!
//! Background: early beta releases stored one keychain entry per session
//! (`session-<uuid>`), which blew Windows Credential Manager's storage limit
//! once a user reached ~1000 sessions. The replacement architecture holds one
//! keychain entry per named profile, and sessions reference a profile by id.
//!
//! This routine runs on every startup. It is a no-op when the legacy table
//! is empty, so it is safe to leave in place indefinitely.

use std::collections::HashMap;
use std::sync::Arc;

use crate::db::models::{NewCredentialProfile, UpdateSession};
use crate::db::DatabaseProvider;

/// Group key for deduplicating legacy credentials. We intentionally include
/// the secret so two sessions that happen to share a username but have
/// different passwords do not get collapsed into the same profile.
type DedupKey = (String, String, String);

/// Migrate any legacy per-session credentials into shared profiles.
///
/// `sessions_db` is the provider that owns the `sessions` table (and, in
/// single-DB mode, also owns everything else). `cred_db` is the provider
/// that owns `credentials` and `credential_profiles` — in PostgreSQL mode
/// this is the per-user local SQLite.
///
/// Returns `(profiles_created, sessions_linked)` so the caller can log it.
pub async fn migrate_legacy_credentials(
    sessions_db: &Arc<dyn DatabaseProvider>,
    cred_db: &Arc<dyn DatabaseProvider>,
) -> Result<(u32, u32), String> {
    let legacy = cred_db.list_all_credentials().await?;
    if legacy.is_empty() {
        return Ok((0, 0));
    }

    tracing::info!(
        count = legacy.len(),
        "Found legacy per-session credentials — migrating to shared profiles"
    );

    // Group legacy rows by (auth_type, username, retrieved_secret). Each
    // unique triple becomes one profile. If secret retrieval fails we fall
    // back to an empty string so the row still gets migrated — the user will
    // need to re-enter the secret via the Credentials Manager, but at least
    // the session metadata is preserved.
    let mut groups: HashMap<DedupKey, Vec<usize>> = HashMap::new();
    let mut secrets: Vec<String> = Vec::with_capacity(legacy.len());
    for (i, cred) in legacy.iter().enumerate() {
        let secret = crate::credentials::retrieve(&cred.keychain_ref)
            .map(|z| (*z).clone())
            .unwrap_or_default();
        let key = (
            cred.auth_type.clone(),
            cred.username.clone(),
            secret.clone(),
        );
        secrets.push(secret);
        groups.entry(key).or_default().push(i);
    }

    // For each group create exactly one credential profile and store its
    // secret once in the OS keychain.
    let mut session_to_profile: HashMap<uuid::Uuid, uuid::Uuid> = HashMap::new();
    let mut profiles_created = 0u32;
    let mut name_counter: u32 = 1;

    for ((auth_type, username, _secret), indices) in &groups {
        // Translate the legacy auth_type ("password", "publickey") to the
        // profile auth_type vocabulary ("password", "key").
        let profile_auth_type = match auth_type.as_str() {
            "publickey" => "key".to_string(),
            other => other.to_string(),
        };

        // For "key" profiles, the legacy secret was the key path. For
        // "password" profiles it was the password itself.
        let (key_path, secret_for_keychain) = if profile_auth_type == "key" {
            (secrets[indices[0]].clone(), String::new())
        } else {
            (String::new(), secrets[indices[0]].clone())
        };

        // Build a human-readable unique name: prefer username, fall back to
        // a numbered default. Collisions are resolved by appending a suffix.
        let base_name = if username.is_empty() {
            format!("Migrated profile {name_counter}")
        } else {
            format!("{username} ({profile_auth_type})")
        };
        name_counter += 1;

        let profile = match cred_db
            .create_credential_profile(NewCredentialProfile {
                name: base_name.clone(),
                auth_type: profile_auth_type,
                username: username.clone(),
                key_path,
            })
            .await
        {
            Ok(p) => p,
            Err(e) => {
                // A UNIQUE collision on `name` is the likely cause; retry
                // with a numeric suffix derived from the profile count.
                let fallback_name = format!("{base_name} #{profiles_created}");
                cred_db
                    .create_credential_profile(NewCredentialProfile {
                        name: fallback_name,
                        auth_type: match auth_type.as_str() {
                            "publickey" => "key".to_string(),
                            other => other.to_string(),
                        },
                        username: username.clone(),
                        key_path: if auth_type == "publickey" {
                            secrets[indices[0]].clone()
                        } else {
                            String::new()
                        },
                    })
                    .await
                    .map_err(|e2| {
                        format!("Legacy migration failed to create profile: {e} / {e2}")
                    })?
            }
        };

        if !secret_for_keychain.is_empty() {
            if let Err(e) = crate::credentials::store(&profile.keychain_ref, &secret_for_keychain) {
                tracing::error!(profile_id = %profile.id, "Failed to store migrated secret: {e}");
            }
        }

        for &idx in indices {
            session_to_profile.insert(legacy[idx].session_id, profile.id);
        }
        profiles_created += 1;
    }

    // Link each session to its new profile. Use the sessions DB so in
    // PostgreSQL mode the update lands in the central store.
    let mut sessions_linked = 0u32;
    for (session_id, profile_id) in &session_to_profile {
        let update = UpdateSession {
            credential_profile_id: Some(Some(*profile_id)),
            ..Default::default()
        };
        match sessions_db.update_session(*session_id, update).await {
            Ok(()) => sessions_linked += 1,
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    "Failed to link migrated profile to session: {e}"
                );
            }
        }
    }

    // Clean up legacy keychain entries and DB rows. Failure here is
    // non-fatal — the profile table is already authoritative.
    for cred in &legacy {
        let _ = crate::credentials::delete(&cred.keychain_ref);
        if let Err(e) = cred_db.delete_credential(cred.session_id).await {
            tracing::warn!(
                session_id = %cred.session_id,
                "Failed to delete legacy credential row: {e}"
            );
        }
    }

    tracing::info!(
        profiles_created,
        sessions_linked,
        "Legacy credential migration complete"
    );

    Ok((profiles_created, sessions_linked))
}
