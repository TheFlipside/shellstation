use tauri::State;
use uuid::Uuid;

use crate::db::models::{LoginSequence, LoginSequenceStep, NewLoginSequence, UpdateLoginSequence};
// LoginSequenceDbState is always backed by local SQLite (same as CredentialDbState),
// never the shared PostgreSQL database. No RLS policies are needed on login_sequences.
use crate::db::LoginSequenceDbState;

const MAX_NAME_LEN: usize = 128;
const MAX_PATTERN_LEN: usize = 1024;
const MAX_RESPONSE_LEN: usize = 4096;
const MAX_STEPS: usize = 50;

fn parse_uuid(s: &str) -> Result<Uuid, String> {
    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

fn validate_steps(steps: &[LoginSequenceStep]) -> Result<(), String> {
    if steps.len() > MAX_STEPS {
        return Err(format!("Too many steps (max {MAX_STEPS})"));
    }
    for (i, step) in steps.iter().enumerate() {
        if step.pattern.len() > MAX_PATTERN_LEN {
            return Err(format!(
                "Step {}: pattern too long (max {MAX_PATTERN_LEN} characters)",
                i + 1
            ));
        }
        if step.response.len() > MAX_RESPONSE_LEN {
            return Err(format!(
                "Step {}: response too long (max {MAX_RESPONSE_LEN} characters)",
                i + 1
            ));
        }
        regex::Regex::new(&step.pattern)
            .map_err(|e| format!("Step {}: invalid regex pattern: {e}", i + 1))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn login_sequence_create(
    db: State<'_, LoginSequenceDbState>,
    name: String,
    send_initial_cr: bool,
    steps: Vec<LoginSequenceStep>,
) -> Result<LoginSequence, String> {
    if name.is_empty() || name.len() > MAX_NAME_LEN {
        return Err(format!(
            "Sequence name must be 1\u{2013}{MAX_NAME_LEN} characters"
        ));
    }
    validate_steps(&steps)?;

    db.0.create_login_sequence(NewLoginSequence {
        name,
        send_initial_cr,
        steps,
    })
    .await
}

#[tauri::command]
pub async fn login_sequence_list(
    db: State<'_, LoginSequenceDbState>,
) -> Result<Vec<LoginSequence>, String> {
    db.0.list_login_sequences().await
}

#[tauri::command]
pub async fn login_sequence_get(
    db: State<'_, LoginSequenceDbState>,
    id: String,
) -> Result<Option<LoginSequence>, String> {
    let uuid = parse_uuid(&id)?;
    db.0.get_login_sequence(uuid).await
}

#[tauri::command]
pub async fn login_sequence_update(
    db: State<'_, LoginSequenceDbState>,
    id: String,
    name: Option<String>,
    send_initial_cr: Option<bool>,
    steps: Option<Vec<LoginSequenceStep>>,
) -> Result<(), String> {
    if let Some(ref n) = name {
        if n.is_empty() || n.len() > MAX_NAME_LEN {
            return Err(format!(
                "Sequence name must be 1\u{2013}{MAX_NAME_LEN} characters"
            ));
        }
    }
    if let Some(ref s) = steps {
        validate_steps(s)?;
    }

    let uuid = parse_uuid(&id)?;
    db.0.update_login_sequence(
        uuid,
        UpdateLoginSequence {
            name,
            send_initial_cr,
            steps,
        },
    )
    .await
}

#[tauri::command]
pub async fn login_sequence_delete(
    db: State<'_, LoginSequenceDbState>,
    id: String,
) -> Result<(), String> {
    let uuid = parse_uuid(&id)?;
    db.0.delete_login_sequence(uuid).await
}

#[tauri::command]
pub async fn folder_apply_login_sequence(
    db: State<'_, crate::db::DbState>,
    folder_id: String,
    sequence_id: Option<String>,
) -> Result<u32, String> {
    let folder_uuid = parse_uuid(&folder_id)?;
    let sequence_uuid = sequence_id.as_deref().map(parse_uuid).transpose()?;
    db.0.bulk_set_session_login_sequence(folder_uuid, sequence_uuid)
        .await
}
