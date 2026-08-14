-- Binds `state` to `source`, and ties a YouTube track's state to whether it
-- actually has a file.
--
-- Two silent-corruption bugs become database errors instead:
--
--   * The scanner upserts by `local_path`. If the downloads directory ever sat
--     inside a library folder, it would set `state='present'` on a YouTube row
--     and quietly turn a downloaded track into a half-local one.
--   * Deleting a downloaded file must clear `local_path` as well as flipping
--     the state back to 'saved'. Forgetting the former leaves a row claiming a
--     file that is gone, and the UNIQUE index then blocks re-downloading it.
--
-- SQLite cannot add a CHECK to an existing table, so this is the standard
-- rebuild. Nothing references `tracks` by foreign key yet, which is what makes
-- dropping it safe.
CREATE TABLE tracks_rebuilt (
    id                INTEGER PRIMARY KEY,
    source            TEXT    NOT NULL CHECK (source IN ('local', 'youtube')),

    title             TEXT    NOT NULL,
    artist            TEXT,
    album             TEXT,
    duration_secs     INTEGER,
    cover_path        TEXT,

    yt_video_id       TEXT,
    yt_channel        TEXT,
    yt_original_title TEXT,
    yt_thumbnail_url  TEXT,

    local_path        TEXT,
    file_mtime        INTEGER,
    file_size         INTEGER,
    folder_id         INTEGER REFERENCES library_folders(id) ON DELETE SET NULL,
    last_seen_scan    INTEGER NOT NULL DEFAULT 0,

    state             TEXT    NOT NULL,

    date_added        INTEGER NOT NULL DEFAULT (unixepoch()),
    last_played       INTEGER,
    play_count        INTEGER NOT NULL DEFAULT 0,

    UNIQUE (local_path),
    UNIQUE (yt_video_id),

    -- Identity: a YouTube row is known by its video id, a local one by its path.
    CHECK (
        (source = 'youtube' AND yt_video_id IS NOT NULL)
        OR
        (source = 'local' AND yt_video_id IS NULL AND local_path IS NOT NULL)
    ),

    -- State belongs to a source, and for YouTube it implies file presence.
    CHECK (
        (source = 'local' AND state IN ('present', 'missing'))
        OR
        (source = 'youtube' AND (
            (state = 'saved' AND local_path IS NULL)
            OR
            (state = 'downloaded' AND local_path IS NOT NULL)
        ))
    )
);

INSERT INTO tracks_rebuilt (
    id, source, title, artist, album, duration_secs, cover_path,
    yt_video_id, yt_channel, yt_original_title, yt_thumbnail_url,
    local_path, file_mtime, file_size, folder_id, last_seen_scan,
    state, date_added, last_played, play_count
)
SELECT
    id, source, title, artist, album, duration_secs, cover_path,
    yt_video_id, yt_channel, yt_original_title, yt_thumbnail_url,
    local_path, file_mtime, file_size, folder_id, last_seen_scan,
    state, date_added, last_played, play_count
FROM tracks;

DROP TABLE tracks;

ALTER TABLE tracks_rebuilt RENAME TO tracks;

-- Indexes went with the old table.
CREATE INDEX idx_tracks_folder_id ON tracks (folder_id);
CREATE INDEX idx_tracks_state ON tracks (state);
