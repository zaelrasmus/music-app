-- Playlists, mixing local and YouTube tracks freely.
--
-- Both tables are portable identity data and are meant to participate in sync
-- later. They keep INTEGER primary keys for consistency with the rest of the
-- schema; making ids stable across devices is a separate, later problem.
CREATE TABLE playlists (
    id         INTEGER PRIMARY KEY,
    name       TEXT    NOT NULL,
    -- Not used yet. Reserved so adding playlist artwork later is not a
    -- migration.
    cover_path TEXT,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

-- A track appears at most once per playlist: the primary key says so.
-- Allowing the same track twice would need a surrogate key here, which is a
-- table rebuild rather than an ALTER -- worth knowing before wanting it.
CREATE TABLE playlist_tracks (
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id    INTEGER NOT NULL REFERENCES tracks(id)    ON DELETE CASCADE,
    -- Dense, 0-based, and kept that way by every write path: adding appends
    -- MAX+1, removing closes the gap, reordering shifts the affected span.
    --
    -- Density is what lets the UI send a plain ordinal ("move item 4 to 1")
    -- and have it mean the same thing as a stored position.
    position    INTEGER NOT NULL,
    added_at    INTEGER NOT NULL DEFAULT (unixepoch()),

    PRIMARY KEY (playlist_id, track_id)
);

-- Deliberately NOT unique.
--
-- Reordering is a single UPDATE that shifts a span of rows, and SQLite applies
-- an UPDATE row by row: partway through, two rows momentarily share a
-- position. A unique index would abort the statement every time. Ordering is
-- an application invariant here, not a database one.
CREATE INDEX idx_playlist_tracks_order ON playlist_tracks (playlist_id, position);

-- The primary key indexes (playlist_id, track_id), which cannot answer
-- "which playlists hold this track" or serve the ON DELETE CASCADE from
-- tracks without a scan.
CREATE INDEX idx_playlist_tracks_track ON playlist_tracks (track_id);
