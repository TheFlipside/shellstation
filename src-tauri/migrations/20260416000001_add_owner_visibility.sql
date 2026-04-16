-- Multi-user session isolation: owner tracking, visibility control, and
-- per-user credential mapping.
--
-- `owner` records who created the item. In PostgreSQL the post-migration RLS
-- setup replaces the placeholder 'local' with `current_user`. In SQLite the
-- value stays 'local' (single-user, no RLS).
--
-- `visibility` controls whether other users can see the item. Defaults to
-- 'personal' so existing data remains private after migration.
--
-- `session_credentials` allows each user (identified by `user_ident`, an
-- app-level identity string) to map their own local credential profile to
-- any session they can access. This avoids the conflict where a shared
-- `credential_profile_id` column gets overwritten by different users.

ALTER TABLE folders ADD COLUMN owner TEXT NOT NULL DEFAULT 'local';
ALTER TABLE folders ADD COLUMN visibility TEXT NOT NULL DEFAULT 'personal';

ALTER TABLE sessions ADD COLUMN owner TEXT NOT NULL DEFAULT 'local';
ALTER TABLE sessions ADD COLUMN visibility TEXT NOT NULL DEFAULT 'personal';

CREATE TABLE IF NOT EXISTS session_credentials (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    user_ident TEXT NOT NULL,
    credential_profile_id TEXT NOT NULL,
    PRIMARY KEY (session_id, user_ident)
);

CREATE INDEX IF NOT EXISTS idx_session_credentials_user
    ON session_credentials(user_ident);
