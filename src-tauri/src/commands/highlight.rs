use tauri::State;
use uuid::Uuid;

use crate::db::models::{NewHighlightProfile, UpdateHighlightProfile};
use crate::db::DbState;
use crate::highlight::{parse_securecrt_highlight_ini, HighlightRule};

use super::MAX_NAME_LEN;

/// Maximum size for highlight rules JSON (1 MB).
const MAX_RULES_LEN: usize = 1_048_576;

/// Maximum number of rules per profile.
const MAX_RULES_COUNT: usize = 500;

/// Maximum size for imported INI content (10 MB).
const MAX_IMPORT_SIZE: usize = 10 * 1024 * 1024;

fn validate_rules(rules: &str) -> Result<(), String> {
    if rules.len() > MAX_RULES_LEN {
        return Err(format!("Rules too large (max {MAX_RULES_LEN} bytes)"));
    }
    let parsed: Vec<HighlightRule> =
        serde_json::from_str(rules).map_err(|e| format!("Invalid rules JSON: {e}"))?;
    if parsed.len() > MAX_RULES_COUNT {
        return Err(format!(
            "Too many rules (max {MAX_RULES_COUNT}, got {})",
            parsed.len()
        ));
    }
    Ok(())
}

#[tauri::command]
pub async fn highlight_profile_create(
    state: State<'_, DbState>,
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
    state: State<'_, DbState>,
) -> Result<Vec<crate::db::models::HighlightProfile>, String> {
    let db = &*state.0;
    db.list_highlight_profiles().await
}

#[tauri::command]
pub async fn highlight_profile_get(
    state: State<'_, DbState>,
    id: String,
) -> Result<Option<crate::db::models::HighlightProfile>, String> {
    let uuid = Uuid::parse_str(&id).map_err(|e| format!("Invalid UUID: {e}"))?;
    let db = &*state.0;
    db.get_highlight_profile(uuid).await
}

#[tauri::command]
pub async fn highlight_profile_update(
    state: State<'_, DbState>,
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
pub async fn highlight_profile_delete(state: State<'_, DbState>, id: String) -> Result<(), String> {
    let uuid = Uuid::parse_str(&id).map_err(|e| format!("Invalid UUID: {e}"))?;
    let db = &*state.0;
    db.delete_highlight_profile(uuid).await
}

#[derive(serde::Serialize)]
pub struct ImportHighlightResult {
    pub profiles_created: u32,
    pub total_rules: u32,
}

#[tauri::command]
pub async fn import_securecrt_highlights(
    state: State<'_, DbState>,
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
