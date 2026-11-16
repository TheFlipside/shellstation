use tauri::State;
use uuid::Uuid;

use crate::db::models::{NewHighlightProfile, UpdateHighlightProfile, UpdateSession};
use crate::db::session_highlights::{SessionHighlightState, SessionHighlights};
use crate::db::{DatabaseProvider, DbState, HighlightDbState};
use crate::highlight::{parse_securecrt_highlight_ini, HighlightRule, MAX_RULES_PER_PROFILE};

use super::MAX_NAME_LEN;

/// Maximum size for highlight rules JSON (1 MB).
const MAX_RULES_LEN: usize = 1_048_576;

/// Maximum size for imported `INI` content (10 MB).
const MAX_IMPORT_SIZE: usize = 10 * 1024 * 1024;

fn validate_rules(rules: &str) -> Result<(), String> {
    if rules.len() > MAX_RULES_LEN {
        return Err(format!("Rules too large (max {MAX_RULES_LEN} bytes)"));
    }
    let parsed: Vec<HighlightRule> =
        serde_json::from_str(rules).map_err(|e| format!("Invalid rules JSON: {e}"))?;
    if parsed.len() > MAX_RULES_PER_PROFILE {
        return Err(format!(
            "Too many rules (max {MAX_RULES_PER_PROFILE}, got {})",
            parsed.len()
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn highlight_profile_create(
    state: State<'_, HighlightDbState>,
    name: String,
    rules: String,
) -> Result<crate::db::models::HighlightProfile, String> {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err("Profile name is required (max 255 characters)".to_string());
    }
    validate_rules(&rules)?;
    let db = &*state.0;
    db.create_highlight_profile(NewHighlightProfile { name, rules })
        .await
}

#[tauri::command]
pub async fn highlight_profile_list(
    state: State<'_, HighlightDbState>,
) -> Result<Vec<crate::db::models::HighlightProfile>, String> {
    let db = &*state.0;
    db.list_highlight_profiles().await
}

#[tauri::command]
pub async fn highlight_profile_get(
    state: State<'_, HighlightDbState>,
    id: String,
) -> Result<Option<crate::db::models::HighlightProfile>, String> {
    let uuid = Uuid::parse_str(&id).map_err(|e| format!("Invalid UUID: {e}"))?;
    let db = &*state.0;
    db.get_highlight_profile(uuid).await
}

#[tauri::command]
pub async fn highlight_profile_update(
    state: State<'_, HighlightDbState>,
    id: String,
    name: Option<String>,
    rules: Option<String>,
) -> Result<(), String> {
    let uuid = Uuid::parse_str(&id).map_err(|e| format!("Invalid UUID: {e}"))?;
    if let Some(ref n) = name {
        if n.is_empty() || n.len() > MAX_NAME_LEN {
            return Err("Profile name is required (max 255 characters)".to_string());
        }
    }
    if let Some(ref r) = rules {
        validate_rules(r)?;
    }
    let db = &*state.0;
    db.update_highlight_profile(uuid, UpdateHighlightProfile { name, rules })
        .await
}

#[tauri::command]
pub async fn highlight_profile_delete(
    state: State<'_, HighlightDbState>,
    highlights: State<'_, SessionHighlightState>,
    id: String,
) -> Result<(), String> {
    let uuid = Uuid::parse_str(&id).map_err(|e| format!("Invalid UUID: {e}"))?;
    let db = &*state.0;
    db.delete_highlight_profile(uuid).await?;
    // In SQLite mode the foreign key takes the profile from the sessions
    // that named it. The choices kept beside a shared database have none.
    // The profile is gone by now, so a failure here is not the delete's: it
    // is logged, and the sessions name a profile that is not there, which
    // shows as none.
    if let Some(local) = highlights.0.as_ref() {
        if let Err(e) = local.forget_profile(uuid).await {
            tracing::warn!("Could not drop the choices of a deleted highlight profile: {e:?}");
        }
    }
    Ok(())
}

/// That `profile` is one of the user's highlight profiles. In `SQLite` mode
/// the foreign key of a session row says so; the choices kept beside a
/// shared database have none.
pub(crate) async fn require_highlight_profile(
    profiles: &HighlightDbState,
    profile: Uuid,
) -> Result<(), String> {
    match profiles.0.get_highlight_profile(profile).await? {
        Some(_) => Ok(()),
        None => Err(format!("Highlight profile {profile} not found")),
    }
}

/// Choose the highlight profile a session is shown with, or none.
///
/// In `PostgreSQL` mode that is the user's own choice, for any session they
/// can see: it is kept in the local database and changes nothing for anyone
/// else, so it is also how a user gets highlighting in another user's shared
/// session, which is not theirs to edit. In `SQLite` mode it is the session
/// row's profile.
#[tauri::command]
pub async fn session_set_highlight_profile(
    state: State<'_, DbState>,
    profiles: State<'_, HighlightDbState>,
    highlights: State<'_, SessionHighlightState>,
    session_id: String,
    highlight_profile_id: Option<String>,
) -> Result<(), String> {
    let session = Uuid::parse_str(&session_id).map_err(|e| format!("Invalid UUID: {e}"))?;
    let profile = highlight_profile_id
        .as_deref()
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|e| format!("Invalid UUID: {e}"))?;
    set_session_highlight_profile(
        &*state.0,
        &profiles,
        highlights.0.as_ref(),
        session,
        profile,
    )
    .await
}

/// See [`session_set_highlight_profile`]. `local` is `None` in `SQLite`
/// mode.
pub(crate) async fn set_session_highlight_profile(
    db: &dyn DatabaseProvider,
    profiles: &HighlightDbState,
    local: Option<&SessionHighlights>,
    session: Uuid,
    profile: Option<Uuid>,
) -> Result<(), String> {
    let Some(local) = local else {
        return db
            .update_session(
                session,
                UpdateSession {
                    highlight_profile_id: Some(profile),
                    ..Default::default()
                },
            )
            .await;
    };
    // Only for a session that is there for this user, so that nothing is
    // kept for an ID that is no session's.
    if db.get_session(session).await?.is_none() {
        return Err(format!("Session {session} not found"));
    }
    if let Some(profile) = profile {
        require_highlight_profile(profiles, profile).await?;
    }
    local.set(&[session], profile).await?;
    Ok(())
}

#[derive(serde::Serialize)]
pub struct ImportHighlightResult {
    pub profiles_created: u32,
    pub total_rules: u32,
}

#[tauri::command]
pub async fn import_securecrt_highlights(
    state: State<'_, HighlightDbState>,
    content: String,
) -> Result<ImportHighlightResult, String> {
    if content.len() > MAX_IMPORT_SIZE {
        return Err("File too large (max 10 MB)".to_string());
    }
    let parsed = parse_securecrt_highlight_ini(&content)?;

    let db = &*state.0;
    let mut profiles_created = 0u32;
    let mut total_rules = 0u32;

    for profile in parsed {
        let rules_json = serde_json::to_string(&profile.rules)
            .map_err(|e| format!("Failed to serialize rules: {e}"))?;
        total_rules += profile.rules.len() as u32;
        db.create_highlight_profile(NewHighlightProfile {
            name: profile.name,
            rules: rules_json,
        })
        .await?;
        profiles_created += 1;
    }

    Ok(ImportHighlightResult {
        profiles_created,
        total_rules,
    })
}
