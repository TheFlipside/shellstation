use async_trait::async_trait;
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::models::{
    Credential, DataFingerprint, Folder, NewFolder, NewSession, Session, UpdateSession,
};
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
        sort_order: row.get("sort_order"),
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
        icon: row.get("icon"),
        sort_order: row.get("sort_order"),
    })
}

fn row_to_credential(row: &PgRow) -> DbResult<Credential> {
    Ok(Credential {
        id: parse_uuid(row.get("id"))?,
        session_id: parse_uuid(row.get("session_id"))?,
        username: row.get("username"),
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

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        // Lock sibling rows to prevent concurrent sort_order races.
        // FOR UPDATE cannot be combined with aggregates, so we lock first
        // and then compute the max in a separate query.
        sqlx::query(
            "SELECT id FROM folders \
             WHERE parent_id IS NOT DISTINCT FROM $1 FOR UPDATE",
        )
        .bind(&parent_str)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("Failed to lock sibling folders: {e}"))?;

        let sort_order: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM folders \
             WHERE parent_id IS NOT DISTINCT FROM $1",
        )
        .bind(&parent_str)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        sqlx::query(
            "INSERT INTO folders (id, name, parent_id, sort_order) VALUES ($1, $2, $3, $4)",
        )
        .bind(&id_str)
        .bind(&folder.name)
        .bind(&parent_str)
        .bind(sort_order)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to create folder: {e}"))?;

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;

        Ok(Folder {
            id,
            name: folder.name,
            parent_id: folder.parent_id,
            sort_order,
        })
    }

    async fn list_folders(&self) -> DbResult<Vec<Folder>> {
        let rows = sqlx::query(
            "SELECT id, name, parent_id, sort_order FROM folders ORDER BY sort_order ASC, name ASC",
        )
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

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        // Lock siblings in the target parent to prevent sort_order races.
        sqlx::query(
            "SELECT id FROM folders \
             WHERE parent_id IS NOT DISTINCT FROM $1 FOR UPDATE",
        )
        .bind(&parent_str)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("Failed to lock sibling folders: {e}"))?;

        let sort_order: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM folders \
             WHERE parent_id IS NOT DISTINCT FROM $1",
        )
        .bind(&parent_str)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        let result =
            sqlx::query("UPDATE folders SET parent_id = $1, sort_order = $2 WHERE id = $3")
                .bind(&parent_str)
                .bind(sort_order)
                .bind(id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("Failed to move folder: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Folder {id} not found"));
        }

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
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

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        // Lock sibling sessions to prevent concurrent sort_order races.
        sqlx::query("SELECT id FROM sessions WHERE folder_id = $1 FOR UPDATE")
            .bind(&folder_str)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| format!("Failed to lock sibling sessions: {e}"))?;

        let sort_order: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM sessions WHERE folder_id = $1",
        )
        .bind(&folder_str)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        sqlx::query(
            "INSERT INTO sessions (id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags, icon, sort_order) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        )
        .bind(&id_str)
        .bind(&folder_str)
        .bind(&session.name)
        .bind(&session.hostname)
        .bind(session.port)
        .bind(&session.protocol)
        .bind("")
        .bind(&session.auth_method)
        .bind(&jump_str)
        .bind(&session.tags)
        .bind(&session.icon)
        .bind(sort_order)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("Failed to create session: {e}"))?;

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;

        Ok(Session {
            id,
            folder_id: session.folder_id,
            name: session.name,
            hostname: session.hostname,
            port: session.port,
            protocol: session.protocol,
            username: String::new(),
            auth_method: session.auth_method,
            jump_host_id: session.jump_host_id,
            tags: session.tags,
            icon: session.icon,
            sort_order,
        })
    }

    async fn get_session(&self, id: Uuid) -> DbResult<Option<Session>> {
        let row = sqlx::query(
            "SELECT id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags, icon, sort_order \
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
            "SELECT id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags, icon, sort_order \
             FROM sessions ORDER BY sort_order ASC, name ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list sessions: {e}"))?;

        rows.iter().map(row_to_session).collect()
    }

    async fn update_session(&self, id: Uuid, update: UpdateSession) -> DbResult<()> {
        let mut sets = Vec::new();
        let mut idx: usize = 1;

        // Track bind values with their types so PostgreSQL receives correct
        // types (it does not coerce TEXT → INTEGER like SQLite does).
        enum BindVal {
            Text(String),
            Int(i32),
            Null,
        }
        let mut values: Vec<BindVal> = Vec::new();

        if let Some(ref name) = update.name {
            sets.push(format!("name = ${idx}"));
            values.push(BindVal::Text(name.clone()));
            idx += 1;
        }
        if let Some(ref hostname) = update.hostname {
            sets.push(format!("hostname = ${idx}"));
            values.push(BindVal::Text(hostname.clone()));
            idx += 1;
        }
        if let Some(port) = update.port {
            sets.push(format!("port = ${idx}"));
            values.push(BindVal::Int(port));
            idx += 1;
        }
        if let Some(ref protocol) = update.protocol {
            sets.push(format!("protocol = ${idx}"));
            values.push(BindVal::Text(protocol.clone()));
            idx += 1;
        }
        if let Some(ref auth_method) = update.auth_method {
            sets.push(format!("auth_method = ${idx}"));
            values.push(BindVal::Text(auth_method.clone()));
            idx += 1;
        }
        if let Some(ref jump_host_id) = update.jump_host_id {
            sets.push(format!("jump_host_id = ${idx}"));
            match jump_host_id {
                Some(u) => values.push(BindVal::Text(u.to_string())),
                None => values.push(BindVal::Null),
            }
            idx += 1;
        }
        if let Some(ref tags) = update.tags {
            sets.push(format!("tags = ${idx}"));
            values.push(BindVal::Text(tags.clone()));
            idx += 1;
        }
        if let Some(ref icon) = update.icon {
            sets.push(format!("icon = ${idx}"));
            values.push(BindVal::Text(icon.clone()));
            idx += 1;
        }

        if sets.is_empty() {
            return Ok(());
        }

        let sql = format!("UPDATE sessions SET {} WHERE id = ${idx}", sets.join(", "));
        let mut query = sqlx::query(&sql);
        for val in &values {
            match val {
                BindVal::Text(s) => query = query.bind(s.as_str()),
                BindVal::Int(n) => query = query.bind(*n),
                BindVal::Null => query = query.bind(None::<String>),
            }
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
        let folder_str = new_folder_id.to_string();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        // Lock sibling sessions in the target folder to prevent sort_order races.
        sqlx::query("SELECT id FROM sessions WHERE folder_id = $1 FOR UPDATE")
            .bind(&folder_str)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| format!("Failed to lock sibling sessions: {e}"))?;

        let sort_order: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM sessions WHERE folder_id = $1",
        )
        .bind(&folder_str)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| format!("Failed to compute sort_order: {e}"))?;

        let result =
            sqlx::query("UPDATE sessions SET folder_id = $1, sort_order = $2 WHERE id = $3")
                .bind(&folder_str)
                .bind(sort_order)
                .bind(id.to_string())
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("Failed to move session: {e}"))?;

        if result.rows_affected() == 0 {
            return Err(format!("Session {id} not found"));
        }

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
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
            "SELECT id, folder_id, name, hostname, port, protocol, username, auth_method, jump_host_id, tags, icon, sort_order \
             FROM sessions \
             WHERE name LIKE $1 ESCAPE '\\' \
                OR hostname LIKE $1 ESCAPE '\\' \
                OR username LIKE $1 ESCAPE '\\' \
                OR tags LIKE $1 ESCAPE '\\' \
             ORDER BY sort_order ASC, name ASC",
        )
        .bind(&pattern)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to search sessions: {e}"))?;

        rows.iter().map(row_to_session).collect()
    }

    // ── Reordering ─────────────────────────────────────────────────────

    async fn reorder_folders(
        &self,
        parent_id: Option<Uuid>,
        ordered_ids: Vec<Uuid>,
    ) -> DbResult<()> {
        let parent_str = parent_id.map(|u| u.to_string());

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        // Lock all affected rows before updating to prevent interleaved reorders.
        sqlx::query(
            "SELECT id FROM folders \
             WHERE parent_id IS NOT DISTINCT FROM $1 FOR UPDATE",
        )
        .bind(&parent_str)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("Failed to lock folders for reorder: {e}"))?;

        for (i, id) in ordered_ids.iter().enumerate() {
            sqlx::query(
                "UPDATE folders SET sort_order = $1 WHERE id = $2 AND parent_id IS NOT DISTINCT FROM $3",
            )
            .bind(i as i32)
            .bind(id.to_string())
            .bind(&parent_str)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("Failed to reorder folders: {e}"))?;
        }

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
        Ok(())
    }

    async fn reorder_sessions(&self, folder_id: Uuid, ordered_ids: Vec<Uuid>) -> DbResult<()> {
        let folder_str = folder_id.to_string();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        // Lock all affected rows before updating to prevent interleaved reorders.
        sqlx::query("SELECT id FROM sessions WHERE folder_id = $1 FOR UPDATE")
            .bind(&folder_str)
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| format!("Failed to lock sessions for reorder: {e}"))?;

        for (i, id) in ordered_ids.iter().enumerate() {
            sqlx::query("UPDATE sessions SET sort_order = $1 WHERE id = $2 AND folder_id = $3")
                .bind(i as i32)
                .bind(id.to_string())
                .bind(&folder_str)
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("Failed to reorder sessions: {e}"))?;
        }

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
        Ok(())
    }

    async fn sort_folders_alphabetically(&self, parent_id: Option<Uuid>) -> DbResult<()> {
        let parent_str = parent_id.map(|u| u.to_string());

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        // Lock and read in one step — FOR UPDATE prevents concurrent modifications.
        let rows = sqlx::query(
            "SELECT id FROM folders \
             WHERE parent_id IS NOT DISTINCT FROM $1 \
             ORDER BY name ASC FOR UPDATE",
        )
        .bind(&parent_str)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("Failed to sort folders: {e}"))?;

        for (i, row) in rows.iter().enumerate() {
            let id: String = row.get("id");
            sqlx::query("UPDATE folders SET sort_order = $1 WHERE id = $2")
                .bind(i as i32)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("Failed to sort folders: {e}"))?;
        }

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
        Ok(())
    }

    async fn sort_sessions_alphabetically(&self, folder_id: Uuid) -> DbResult<()> {
        let folder_str = folder_id.to_string();

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| format!("Failed to begin transaction: {e}"))?;

        // Lock and read in one step — FOR UPDATE prevents concurrent modifications.
        let rows = sqlx::query(
            "SELECT id FROM sessions WHERE folder_id = $1 \
             ORDER BY name ASC FOR UPDATE",
        )
        .bind(&folder_str)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| format!("Failed to sort sessions: {e}"))?;

        for (i, row) in rows.iter().enumerate() {
            let id: String = row.get("id");
            sqlx::query("UPDATE sessions SET sort_order = $1 WHERE id = $2")
                .bind(i as i32)
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(|e| format!("Failed to sort sessions: {e}"))?;
        }

        tx.commit()
            .await
            .map_err(|e| format!("Failed to commit: {e}"))?;
        Ok(())
    }

    // ── Credentials ──────────────────────────────────────────────────────

    async fn upsert_credential(&self, cred: Credential) -> DbResult<()> {
        sqlx::query(
            "INSERT INTO credentials (id, session_id, username, auth_type, keychain_ref, secret) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             ON CONFLICT(session_id) DO UPDATE SET \
               username = EXCLUDED.username, \
               auth_type = EXCLUDED.auth_type, \
               keychain_ref = EXCLUDED.keychain_ref, \
               secret = EXCLUDED.secret",
        )
        .bind(cred.id.to_string())
        .bind(cred.session_id.to_string())
        .bind(&cred.username)
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
            "SELECT id, session_id, username, auth_type, keychain_ref, secret FROM credentials WHERE session_id = $1",
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
            "SELECT id, session_id, username, auth_type, keychain_ref, secret FROM credentials ORDER BY session_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to list credentials: {e}"))?;

        rows.iter().map(row_to_credential).collect()
    }

    async fn data_fingerprint(&self) -> DbResult<DataFingerprint> {
        let folder_rows = sqlx::query("SELECT id, name, sort_order FROM folders ORDER BY id")
            .fetch_all(&self.pool)
            .await
            .map_err(|e| format!("Failed to fingerprint folders: {e}"))?;
        let session_rows = sqlx::query(
            "SELECT id, name, hostname, port, folder_id, sort_order FROM sessions ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| format!("Failed to fingerprint sessions: {e}"))?;

        let mut hasher = DefaultHasher::new();
        folder_rows.len().hash(&mut hasher);
        for row in &folder_rows {
            let id: String = row.get("id");
            let name: String = row.get("name");
            let sort_order: i32 = row.get("sort_order");
            id.hash(&mut hasher);
            name.hash(&mut hasher);
            sort_order.hash(&mut hasher);
        }
        session_rows.len().hash(&mut hasher);
        for row in &session_rows {
            let id: String = row.get("id");
            let name: String = row.get("name");
            let hostname: String = row.get("hostname");
            let port: i32 = row.get("port");
            let folder_id: String = row.get("folder_id");
            let sort_order: i32 = row.get("sort_order");
            id.hash(&mut hasher);
            name.hash(&mut hasher);
            hostname.hash(&mut hasher);
            port.hash(&mut hasher);
            folder_id.hash(&mut hasher);
            sort_order.hash(&mut hasher);
        }
        Ok(DataFingerprint {
            hash: format!("{:x}", hasher.finish()),
        })
    }
}
