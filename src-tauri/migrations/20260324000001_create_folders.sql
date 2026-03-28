CREATE TABLE IF NOT EXISTS folders (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    parent_id TEXT REFERENCES folders(id) ON DELETE CASCADE,
    sort_order INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_folders_parent_id ON folders(parent_id);
