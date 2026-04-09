CREATE TABLE IF NOT EXISTS highlight_profiles (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    rules TEXT NOT NULL DEFAULT '[]',
    sort_order INTEGER NOT NULL DEFAULT 0
);

ALTER TABLE sessions ADD COLUMN highlight_profile_id TEXT
    REFERENCES highlight_profiles(id) ON DELETE SET NULL;
