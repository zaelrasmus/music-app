-- Unified track table: local files and YouTube-sourced tracks share one shape,
-- so playlists and the player never branch on source.
--
-- The YouTube columns are created now but stay NULL until that phase -- adding
-- `source` to a local-only table later would be a far worse migration.
--
-- DEVICE-LOCAL COLUMNS -- never include these in the future sync path:
--   local_path, file_mtime, file_size, folder_id, last_seen_scan
-- A path is meaningless on another device. When sync lands, these move to a
-- separate `track_local_files` table; identity (title/artist/yt_video_id)
-- stays here.
--
-- KNOWN FUTURE SPLIT: `state` currently carries two unrelated ideas --
-- local availability ('present'/'missing') and YouTube storage mode
-- ('saved'/'downloaded'). Interpreting it requires knowing `source`. Splitting
-- into `local_state` + `yt_state` is likely once the YouTube phase starts.
CREATE TABLE tracks (
    id                INTEGER PRIMARY KEY,
    source            TEXT    NOT NULL CHECK (source IN ('local', 'youtube')),

    -- Metadata. `title` is NOT NULL because the scanner falls back to the file
    -- name, so the UI never has to render a nameless track.
    title             TEXT    NOT NULL,
    artist            TEXT,
    album             TEXT,
    duration_secs     INTEGER,
    cover_path        TEXT,

    -- YouTube identity. Unused this phase.
    yt_video_id       TEXT,
    yt_channel        TEXT,
    yt_original_title TEXT,
    yt_thumbnail_url  TEXT,

    -- Local availability (device-local, see note above).
    local_path        TEXT,
    file_mtime        INTEGER,
    file_size         INTEGER,
    -- SET NULL, not CASCADE: removing a library folder must not destroy the
    -- track rows, or every playlist referencing them loses its entries and the
    -- play history goes with it. remove_library_folder marks them 'missing'.
    folder_id         INTEGER REFERENCES library_folders(id) ON DELETE SET NULL,
    -- Scan generation. Rows touched by scan N carry N; anything left behind
    -- under a scanned folder is what disappeared from disk.
    last_seen_scan    INTEGER NOT NULL DEFAULT 0,

    state             TEXT    NOT NULL
                              CHECK (state IN ('present', 'missing', 'saved', 'downloaded')),

    date_added        INTEGER NOT NULL DEFAULT (unixepoch()),
    last_played       INTEGER,
    play_count        INTEGER NOT NULL DEFAULT 0,

    -- SQLite treats NULLs as distinct in UNIQUE, so every local row may share
    -- NULL yt_video_id (and every YouTube row NULL local_path).
    UNIQUE (local_path),
    UNIQUE (yt_video_id),

    -- A YouTube row is identified by its video id; a local row by its path.
    CHECK (
        (source = 'youtube' AND yt_video_id IS NOT NULL)
        OR
        (source = 'local' AND yt_video_id IS NULL AND local_path IS NOT NULL)
    )
);

-- Every reconcile pass filters by folder; every list query filters by state.
CREATE INDEX idx_tracks_folder_id ON tracks (folder_id);
CREATE INDEX idx_tracks_state ON tracks (state);
