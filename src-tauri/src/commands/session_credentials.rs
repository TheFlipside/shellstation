use sqlx::Row;
use tauri::State;

use crate::config::ConfigState;
use crate::db::PgPoolState;

fn require_pg_pool(pg: &PgPoolState) -> Result<&sqlx::PgPool, String> {
    pg.0.as_ref()
        .ok_or_else(|| "Session credentials are only available in PostgreSQL mode".to_string())
}

fn require_user_ident(config: &ConfigState) -> Result<String, String> {
    let cfg = config
        .config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {e}"))?;
    cfg.user_ident
        .clone()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "User identity is not configured. Set it in Settings before using shared sessions."
                .to_string()
        })
}

/// The ids are stored as text, in the form `Uuid` prints. What is bound to a
/// statement is therefore the parsed id printed again, never the text as it
/// came: `Uuid` also reads upper case, braces and the form without hyphens,
/// none of which would match a stored id.
fn parse_uuid(s: &str) -> Result<uuid::Uuid, String> {
    uuid::Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

// A mapping belongs to the database role that wrote it (`owner`), and under
// that role to a `user_ident`, the name the client is configured with. The
// role is who `PostgreSQL` authenticated, so it is what keeps one user's
// mappings from another: every statement here names `owner = current_user`,
// and row-level security (`setup_postgres_rls`) holds a role to its own rows
// whatever statement it sends. `user_ident` is a name the client chooses for
// itself and protects nothing. It only tells people apart who share one
// database account.

/// Set (or clear) the current user's credential profile for a session.
#[tauri::command]
pub async fn set_session_credential(
    pg: State<'_, PgPoolState>,
    config: State<'_, ConfigState>,
    session_id: String,
    credential_profile_id: Option<String>,
) -> Result<(), String> {
    let pool = require_pg_pool(&pg)?;
    let user_ident = require_user_ident(&config)?;
    let session_id = parse_uuid(&session_id)?.to_string();
    let credential_profile_id = credential_profile_id
        .as_deref()
        .map(parse_uuid)
        .transpose()?
        .map(|id| id.to_string());
    set_user_credential(
        pool,
        &session_id,
        &user_ident,
        credential_profile_id.as_deref(),
    )
    .await
}

/// Set (or clear) the mapping of `session_id` that the current role has
/// under `user_ident`.
pub(crate) async fn set_user_credential(
    pool: &sqlx::PgPool,
    session_id: &str,
    user_ident: &str,
    credential_profile_id: Option<&str>,
) -> Result<(), String> {
    match credential_profile_id {
        Some(profile_id) => {
            // From `sessions`, so that only a session the role can see gets
            // a mapping, as in the folder-wide statement below. The foreign
            // key alone would take the id of any session there is.
            let result = sqlx::query(
                "INSERT INTO session_credentials (session_id, owner, user_ident, credential_profile_id) \
                 SELECT s.id, current_user, $2, $3 FROM sessions s WHERE s.id = $1 \
                 ON CONFLICT (session_id, owner, user_ident) \
                 DO UPDATE SET credential_profile_id = EXCLUDED.credential_profile_id",
            )
            .bind(session_id)
            .bind(user_ident)
            .bind(profile_id)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to set session credential: {e}"))?;
            if result.rows_affected() == 0 {
                return Err(format!("Session {session_id} not found"));
            }
        }
        None => {
            sqlx::query(
                "DELETE FROM session_credentials \
                 WHERE session_id = $1 AND owner = current_user AND user_ident = $2",
            )
            .bind(session_id)
            .bind(user_ident)
            .execute(pool)
            .await
            .map_err(|e| format!("Failed to clear session credential: {e}"))?;
        }
    }
    Ok(())
}

/// Get the current user's credential profile for a session.
#[tauri::command]
pub async fn get_session_credential(
    pg: State<'_, PgPoolState>,
    config: State<'_, ConfigState>,
    session_id: String,
) -> Result<Option<String>, String> {
    let pool = require_pg_pool(&pg)?;
    let user_ident = require_user_ident(&config)?;
    let session_id = parse_uuid(&session_id)?.to_string();

    let row = sqlx::query(
        "SELECT credential_profile_id FROM session_credentials \
         WHERE session_id = $1 AND owner = current_user AND user_ident = $2",
    )
    .bind(&session_id)
    .bind(&user_ident)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to get session credential: {e}"))?;

    Ok(row.map(|r| r.get::<String, _>("credential_profile_id")))
}

/// Bulk-set credential profile for all sessions in a folder subtree.
#[tauri::command]
pub async fn bulk_set_session_credentials(
    pg: State<'_, PgPoolState>,
    config: State<'_, ConfigState>,
    folder_id: String,
    credential_profile_id: Option<String>,
) -> Result<u32, String> {
    let pool = require_pg_pool(&pg)?;
    let user_ident = require_user_ident(&config)?;
    let folder_id = parse_uuid(&folder_id)?.to_string();
    let credential_profile_id = credential_profile_id
        .as_deref()
        .map(parse_uuid)
        .transpose()?
        .map(|id| id.to_string());
    bulk_set_user_credentials(
        pool,
        &folder_id,
        &user_ident,
        credential_profile_id.as_deref(),
    )
    .await
}

/// Set (or clear) the mapping that the current role has under `user_ident`
/// for every session in `folder_id` and the folders below it, and return
/// how many sessions that was.
pub(crate) async fn bulk_set_user_credentials(
    pool: &sqlx::PgPool,
    folder_id: &str,
    user_ident: &str,
    credential_profile_id: Option<&str>,
) -> Result<u32, String> {
    if let Some(profile_id) = credential_profile_id {
        let result = sqlx::query(
            "WITH RECURSIVE subtree(id) AS ( \
                 SELECT id FROM folders WHERE id = $3 \
                 UNION ALL \
                 SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id \
             ) \
             INSERT INTO session_credentials (session_id, owner, user_ident, credential_profile_id) \
             SELECT s.id, current_user, $1, $2 FROM sessions s \
             WHERE s.folder_id IN (SELECT id FROM subtree) AND s.protocol != 'telnet' \
             ON CONFLICT (session_id, owner, user_ident) \
             DO UPDATE SET credential_profile_id = EXCLUDED.credential_profile_id",
        )
        .bind(user_ident)
        .bind(profile_id)
        .bind(folder_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to bulk set session credentials: {e}"))?;
        Ok(result.rows_affected() as u32)
    } else {
        let result = sqlx::query(
            "WITH RECURSIVE subtree(id) AS ( \
                 SELECT id FROM folders WHERE id = $2 \
                 UNION ALL \
                 SELECT f.id FROM folders f JOIN subtree s ON f.parent_id = s.id \
             ) \
             DELETE FROM session_credentials \
             WHERE owner = current_user AND user_ident = $1 AND session_id IN ( \
                 SELECT s.id FROM sessions s \
                 WHERE s.folder_id IN (SELECT id FROM subtree) AND s.protocol != 'telnet' \
             )",
        )
        .bind(user_ident)
        .bind(folder_id)
        .execute(pool)
        .await
        .map_err(|e| format!("Failed to bulk clear session credentials: {e}"))?;
        Ok(result.rows_affected() as u32)
    }
}

/// Query the current user's credential profile `ID` for a session.
/// Used internally by `session_connect` for `PG`-mode credential resolution.
pub(crate) async fn query_user_credential(
    pool: &sqlx::PgPool,
    session_id: &str,
    user_ident: &str,
) -> Result<Option<uuid::Uuid>, String> {
    let row = sqlx::query(
        "SELECT credential_profile_id FROM session_credentials \
         WHERE session_id = $1 AND owner = current_user AND user_ident = $2",
    )
    .bind(session_id)
    .bind(user_ident)
    .fetch_optional(pool)
    .await
    .map_err(|e| format!("Failed to query session credential: {e}"))?;

    match row {
        Some(r) => {
            let id_str: String = r.get("credential_profile_id");
            let uuid = uuid::Uuid::parse_str(&id_str)
                .map_err(|e| format!("Invalid credential profile UUID: {e}"))?;
            Ok(Some(uuid))
        }
        None => Ok(None),
    }
}
