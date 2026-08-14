-- Free-text tags, applied to any track regardless of source.
--
-- Case-insensitivity is done with a normalized key rather than COLLATE NOCASE,
-- which folds ASCII A-Z only. In a library full of Spanish titles, NOCASE would
-- happily create "Canción" and "CANCIÓN" as two separate tags -- the exact bug
-- `library_folders.path_key` already exists to avoid. `name_key` is written by
-- Rust's `to_lowercase()`, which is Unicode-aware.
CREATE TABLE tags (
    id         INTEGER PRIMARY KEY,
    -- What the user typed, with their casing preserved for display.
    name       TEXT    NOT NULL,
    -- Trimmed and lowercased. Uniqueness lives here, not on `name`.
    name_key   TEXT    NOT NULL UNIQUE,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE track_tags (
    track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    tag_id   INTEGER NOT NULL REFERENCES tags(id)   ON DELETE CASCADE,
    added_at INTEGER NOT NULL DEFAULT (unixepoch()),

    PRIMARY KEY (track_id, tag_id)
);

-- The primary key indexes (track_id, tag_id), which cannot answer "which
-- tracks carry this tag" -- the direction every filter query runs.
CREATE INDEX idx_track_tags_tag ON track_tags (tag_id);
