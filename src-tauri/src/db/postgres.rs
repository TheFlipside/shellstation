use async_trait::async_trait;
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use super::models::{Credential, Folder, NewFolder, NewSession, Session, UpdateSession};
use super::{DatabaseProvider, DbResult};

pub struct PostgresProvider {
    pool: PgPool,
}

impl PostgresProvider {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn parse_uuid(s: &str) -> DbResult<Uuid> {
    Uuid::parse_str(s).map_err(|e| format!("Invalid UUID '{s}': {e}"))
}

fn parse_optional_uuid(s: Option<&str>) -> DbResult<Option<Uuid>> {
    match s {
        Some(v) => Ok(Some(parse_uuid(v)?)),
        None => Ok(None),
    }
}

fn row_to_folder(row: &PgRow) -> DbResult<Folder> {
    Ok(Folder {
        id: parse_uuid(row.get("id"))?,
        name: row.get("name"),
        parent_id: parse_optional_uuid(row.get("parent_id"))?,
    })
}

fn row_to_session(row: &PgRow) -> DbResult<Session> {
    Ok(Session {
        id: parse_uuid(row.get("id"))?,
        folder_id: parse_uuid(row.get("folder_id"))?,
        name: row.get("name"),
        hostname: row.get("hostname"),
        port: row.get("port"),
        protocol: row.get("protocol"),
        username: row.get("username"),
        auth_method: row.get("auth_method"),
        jump_host_id: parse_optional_uuid(row.get("jump_host_id"))?,
        tags: row.get("tags"),
    })
}

fn row_to_credential(row: &PgRow) -> DbResult<Credential> {
    Ok(Credential {
        id: parse_uuid(row.get("id"))?,
        session_id: parse_uuid(row.get("session_id"))?,
        auth_type: row.get("auth_type"),
        keychain_ref: row.get("keychain_ref"),
        secret: row.get("secret"),
    })
}

#[async_trait]
impl DatabaseProvider for PostgresProvider {
    // ── Folders ──────────────────────────────────────────────────────────

    async fn create_folder(&self, folder: NewFolder) -> DbResult<Folder> {
        let id = Uuid::new_v4();
        let id_str = id.to_string();
        let parent_str = folder.parent_id.map(|u| u.to_string());

        sqlx::query("INSERT INTO folders (id, name, parent_id) VALUES ($1, $2, $3)")
            .bind(&id_str)
            .bind(&folder.name)
            .bind(&parent_str)
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to create folder: {e}"))?;

        Ok(Folder {
            id,
            name: folder.name,
            parent_id: folder.parent_id,
        })
    }

    async fn list_folders(&self) -> DbResult<Vec<Folder>> {
        let rows = sqlx::query("SELECT id, name, parent_id FROM folders ORDER BY name")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| format!("Failed to list folders: {e}"))?;

        rows.iter().map(row_to_folder).collect()
    }

    async fn rename_folder(&self, id: Uuid, name: &str) -> DbResult<()> {
        let result = sqlx::query("UPDATE folders SET name = $1 WHERE id = $2")
            .bind(name)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to rename folder: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Folder {id} not found"));
        }
        Ok(())
    }

    async fn move_folder(&self, id: Uuid, new_parent_id: Option<Uuid>) -> DbResult<()> {
        let parent_str = new_parent_id.map(|u| u.to_string());

        let result = sqlx::query("UPDATE folders SET parent_id = $1 WHERE id = $2")
            .bind(&parent_str)
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to move folder: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Folder {id} not found"));
        }
        Ok(())
    }

    async fn delete_folder(&self, id: Uuid) -> DbResult<()> {
        let result = sqlx::query("DELETE FROM folders WHERE id = $1")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete folder: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Folder {id} not found"));
        }
        Ok(())
    }

    // ── Sessions ─────────────────────────────────────────────────────────

    async fn create_session(&self, session: NewSession) -> DbResult<Session> {
        let id = Uuid::new_v4();
        let id_str = id.to_string();
        let folder_str = session.folder_id.to_string();
        let jump_str = session.jump_host_id.map(|u| u.to_string());

        sqlx::query(
            "INSERT INTO sessions (id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&id_str)
        .bind(&folder_str)
        .bind(&session.name)
        .bind(&session.hostname)
        .bind(session.port)
        .bind(&session.protocol)
        .bind(&session.username)
        .bind(&session.auth_method)
        .bind(&jump_str)
        .bind(&session.tags)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to create session: {e}"))?;

        Ok(Session {
            id,
            folder_id: session.folder_id,
            name: session.name,
            hostname: session.hostname,
            port: session.port,
            protocol: session.protocol,
            username: session.username,
            auth_method: session.auth_method,
            jump_host_id: session.jump_host_id,
            tags: session.tags,
        })
    }

    async fn get_session(&self, id: Uuid) -> DbResult<Option<Session>> {
        let row = sqlx::query(
            "SELECT id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags \
             FROM sessions WHERE id = $1",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("Failed to get session: {e}"))?;

        match row {
            Some(r) => Ok(Some(row_to_session(&r)?)),
            None => Ok(None),
        }
    }

    async fn list_all_sessions(&self) -> DbResult<Vec<Session>> {
        let rows = sqlx::query(
            "SELECT id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags \
             FROM sessions ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list sessions: {e}"))?;

        rows.iter().map(row_to_session).collect()
    }

    async fn update_session(&self, id: Uuid, update: UpdateSession) -> DbResult<()> {
        let mut sets = Vec::new();
        let mut values: Vec<String> = Vec::new();
        let mut idx: usize = 1;

        if let Some(ref name) = update.name {
            sets.push(format!("name = ${idx}"));
            values.push(name.clone());
            idx += 1;
        }
        if let Some(ref hostname) = update.hostname {
            sets.push(format!("hostname = ${idx}"));
            values.push(hostname.clone());
            idx += 1;
        }
        if let Some(port) = update.port {
            sets.push(format!("port = ${idx}"));
            values.push(port.to_string());
            idx += 1;
        }
        if let Some(ref username) = update.username {
            sets.push(format!("username = ${idx}"));
            values.push(username.clone());
            idx += 1;
        }
        if let Some(ref auth_method) = update.auth_method {
            sets.push(format!("auth_method = ${idx}"));
            values.push(auth_method.clone());
            idx += 1;
        }
        if let Some(ref jump_host_id) = update.jump_host_id {
            sets.push(format!("jump_host_id = ${idx}"));
            values.push(jump_host_id.map(|u| u.to_string()).unwrap_or_default());
            idx += 1;
        }
        if let Some(ref tags) = update.tags {
            sets.push(format!("tags = ${idx}"));
            values.push(tags.clone());
            idx += 1;
        }

        if sets.is_empty() {
            return Ok(());
        }

        let sql = format!("UPDATE sessions SET {} WHERE id = ${idx}", sets.join(", "));
        let mut query = sqlx::query(&sql);
        for val in &values {
            query = query.bind(val);
        }
        query = query.bind(id.to_string());

        let result = query
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to update session: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Session {id} not found"));
        }
        Ok(())
    }

    async fn move_session(&self, id: Uuid, new_folder_id: Uuid) -> DbResult<()> {
        let result = sqlx::query("UPDATE sessions SET folder_id = $1 WHERE id = $2")
            .bind(new_folder_id.to_string())
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to move session: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Session {id} not found"));
        }
        Ok(())
    }

    async fn delete_session(&self, id: Uuid) -> DbResult<()> {
        let result = sqlx::query("DELETE FROM sessions WHERE id = $1")
            .bind(id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete session: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Session {id} not found"));
        }
        Ok(())
    }

    async fn search_sessions(&self, query: &str) -> DbResult<Vec<Session>> {
        let escaped = query.replace('%', "\\%").replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let rows = sqlx::query(
            "SELECT id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags \
             FROM sessions \
             WHERE name LIKE $1 ESCAPE '\\' \
                OR hostname LIKE $1 ESCAPE '\\' \
                OR username LIKE $1 ESCAPE '\\' \
                OR tags LIKE $1 ESCAPE '\\' \
             ORDER BY name",
        )
        .bind(&pattern)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to search sessions: {e}"))?;

        rows.iter().map(row_to_session).collect()
    }

    // ── Credentials ──────────────────────────────────────────────────────

    async fn upsert_credential(&self, cred: Credential) -> DbResult<()> {
        sqlx::query(
            "INSERT INTO credentials (id, session_id, auth_type, keychain_ref, secret) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT(session_id) DO UPDATE SET \
               auth_type = EXCLUDED.auth_type, \
               keychain_ref = EXCLUDED.keychain_ref, \
               secret = EXCLUDED.secret",
        )
        .bind(cred.id.to_string())
        .bind(cred.session_id.to_string())
        .bind(&cred.auth_type)
        .bind(&cred.keychain_ref)
        .bind(&cred.secret)
        .execute(&self.pool)
        .await
        .map_err(|e| format!("Failed to upsert credential: {e}"))?;

        Ok(())
    }

    async fn get_credential(&self, session_id: Uuid) -> DbResult<Option<Credential>> {
        let row = sqlx::query(
            "SELECT id, session_id, auth_type, keychain_ref, secret FROM credentials WHERE session_id = $1",
        )
        .bind(session_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| format!("Failed to get credential: {e}"))?;

        match row {
            Some(r) => Ok(Some(row_to_credential(&r)?)),
            None => Ok(None),
        }
    }

    async fn delete_credential(&self, session_id: Uuid) -> DbResult<()> {
        sqlx::query("DELETE FROM credentials WHERE session_id = $1")
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| format!("Failed to delete credential: {e}"))?;

        Ok(())
    }

    async fn list_all_credentials(&self) -> DbResult<Vec<Credential>> {
        let rows = sqlx::query(
            "SELECT id, session_id, auth_type, keychain_ref, secret FROM credentials ORDER BY session_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list credentials: {e}"))?;

        rows.iter().map(row_to_credential).collect()
    }
}
