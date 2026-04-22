CREATE TABLE IF NOT EXISTS login_sequences (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE,
    send_initial_cr INTEGER NOT NULL DEFAULT 0,
    steps TEXT NOT NULL DEFAULT '[]',
    sort_order INTEGER NOT NULL DEFAULT 0
);

ALTER TABLE sessions ADD COLUMN login_sequence_id TEXT;
CREATE INDEX idx_sessions_login_sequence_id ON sessions(login_sequence_id);

CREATE TABLE IF NOT EXISTS session_login_sequences (
    session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    user_ident TEXT NOT NULL,
    login_sequence_id TEXT NOT NULL,
    PRIMARY KEY (session_id, user_ident)
);
CREATE INDEX IF NOT EXISTS idx_session_login_sequences_user ON session_login_sequences(user_ident);
