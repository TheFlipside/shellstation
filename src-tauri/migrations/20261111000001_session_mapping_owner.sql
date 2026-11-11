-- Per-user session mappings belong to a database role.
--
-- `session_credentials` and `session_login_sequences` were keyed by
-- `user_ident` alone, a name each client chooses for itself, so nothing tied
-- a row to the database role that wrote it and row-level security had
-- nothing to go by. `owner` is that role. The app writes `current_user`
-- there and reads only rows with it; in PostgreSQL the post-migration RLS
-- setup limits each role to its own rows.
--
-- Rows from before this migration get the placeholder 'local', as folders
-- and sessions did. A client claims the ones under its own `user_ident` at
-- startup; nothing connects with a row that is still 'local'.
--
-- `user_ident` stays in the key: people who share one database account are
-- told apart by it, as before.
--
-- The unique index is what the app's upserts go by. The old primary key
-- (session_id, user_ident) would let one role take a name for a session and
-- so keep another role from mapping it; the RLS setup replaces it with this
-- index in PostgreSQL. SQLite keeps it: it does not use these tables.

ALTER TABLE session_credentials ADD COLUMN owner TEXT NOT NULL DEFAULT 'local';
CREATE UNIQUE INDEX session_credentials_owner_key
    ON session_credentials(session_id, owner, user_ident);

ALTER TABLE session_login_sequences ADD COLUMN owner TEXT NOT NULL DEFAULT 'local';
CREATE UNIQUE INDEX session_login_sequences_owner_key
    ON session_login_sequences(session_id, owner, user_ident);
