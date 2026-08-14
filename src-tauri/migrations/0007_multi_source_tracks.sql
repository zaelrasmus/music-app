-- Generalises remote tracks from "YouTube" to "any provider".
--
-- The YouTube columns were fine while YouTube was the only remote source, but
-- SoundCloud breaks two assumptions baked into them:
--
--   * A SoundCloud id is a plain integer ("199428706"), and its page URL
--     cannot be derived from it -- soundcloud.com/<user>/<slug> needs the
--     uploader's handle. YouTube never exposed this because
--     "watch?v=" || id always works. So identity now needs `remote_url`
--     stored, not just an id.
--
--   * Integer ids collide across providers. A single `UNIQUE (remote_id)`
--     would let one provider's track permanently block another's. Uniqueness
--     is therefore scoped to (source, remote_id).
--
-- Column renames:
--   yt_video_id       -> remote_id
--   yt_channel        -> remote_uploader
--   yt_original_title -> remote_title
--   yt_thumbnail_url  -> remote_thumbnail_url
--   (new)             -> remote_url
--
-- DELIBERATELY NOT DONE HERE: splitting `state` into local_state + yt_state,
-- which 0002 flagged as likely. Generalising its CHECK to "any non-local
-- source" covers SoundCloud completely, whereas the split would ripple through
-- every component that reads `track.state`. Not worth the blast radius yet.
--
-- ⚠ FTS: rebuilding `tracks` drops the triggers from 0006 and orphans
-- `tracks_fts`, which then returns stale rows *with no error*. 0006 says so in
-- capitals. The teardown and rebuild below is that warning being honoured.

-- --- FTS teardown ------------------------------------------------------
-- Must happen before the table goes, or the triggers vanish silently with it.
DROP TRIGGER IF EXISTS tracks_fts_ai;
DROP TRIGGER IF EXISTS tracks_fts_ad;
DROP TRIGGER IF EXISTS tracks_fts_au;
DROP TABLE IF EXISTS tracks_fts;

-- --- table rebuild -----------------------------------------------------
CREATE TABLE tracks_rebuilt (
    id                   INTEGER PRIMARY KEY,
    -- Open-ended by design: a new provider is one more value here plus one
    -- more variant in `providers.rs`.
    source               TEXT    NOT NULL
                                 CHECK (source IN ('local', 'youtube', 'soundcloud')),

    title                TEXT    NOT NULL,
    artist               TEXT,
    album                TEXT,
    duration_secs        INTEGER,
    cover_path           TEXT,

    -- Remote identity. NULL on local rows.
    remote_id            TEXT,
    -- The provider's own page for this track. Stored rather than derived --
    -- see the header.
    remote_url           TEXT,
    -- What the provider says, kept verbatim. `title`/`artist` above are the
    -- user-editable display copy, because remote metadata is dirty: a
    -- slowed+reverb upload has no clean artist tag.
    remote_uploader      TEXT,
    remote_title         TEXT,
    remote_thumbnail_url TEXT,

    -- Device-local. Never part of any future sync path.
    local_path           TEXT,
    file_mtime           INTEGER,
    file_size            INTEGER,
    folder_id            INTEGER REFERENCES library_folders(id) ON DELETE SET NULL,
    last_seen_scan       INTEGER NOT NULL DEFAULT 0,

    state                TEXT    NOT NULL,

    date_added           INTEGER NOT NULL DEFAULT (unixepoch()),
    last_played          INTEGER,
    play_count           INTEGER NOT NULL DEFAULT 0,

    UNIQUE (local_path),
    -- Scoped to the provider. SQLite exempts rows with any NULL in a composite
    -- UNIQUE, so every local row (NULL remote_id) coexists freely.
    UNIQUE (source, remote_id),

    -- Identity: a local row is known by its path, a remote row by its id, and
    -- a remote row is unplayable without a URL to resolve.
    CHECK (
        (source = 'local'
            AND remote_id IS NULL
            AND local_path IS NOT NULL)
        OR
        (source <> 'local'
            AND remote_id IS NOT NULL
            AND remote_url IS NOT NULL)
    ),

    -- State belongs to a source, and for remote tracks it implies whether a
    -- file exists on disk.
    CHECK (
        (source = 'local' AND state IN ('present', 'missing'))
        OR
        (source <> 'local' AND (
            (state = 'saved' AND local_path IS NULL)
            OR
            (state = 'downloaded' AND local_path IS NOT NULL)
        ))
    )
);

-- Every existing remote row is YouTube, and a YouTube URL *is* derivable from
-- its id -- which is precisely the assumption this migration removes going
-- forward. Backfilling it here is what lets `remote_url` be NOT NULL for
-- remote rows from now on.
INSERT INTO tracks_rebuilt (
    id, source, title, artist, album, duration_secs, cover_path,
    remote_id, remote_url, remote_uploader, remote_title, remote_thumbnail_url,
    local_path, file_mtime, file_size, folder_id, last_seen_scan,
    state, date_added, last_played, play_count
)
SELECT
    id, source, title, artist, album, duration_secs, cover_path,
    yt_video_id,
    CASE
        WHEN yt_video_id IS NOT NULL
        THEN 'https://www.youtube.com/watch?v=' || yt_video_id
    END,
    yt_channel, yt_original_title, yt_thumbnail_url,
    local_path, file_mtime, file_size, folder_id, last_seen_scan,
    state, date_added, last_played, play_count
FROM tracks;

DROP TABLE tracks;

ALTER TABLE tracks_rebuilt RENAME TO tracks;

-- Indexes went with the old table.
CREATE INDEX idx_tracks_folder_id ON tracks (folder_id);
CREATE INDEX idx_tracks_state ON tracks (state);

-- --- FTS rebuild -------------------------------------------------------
-- Identical to 0006. Kept verbatim rather than "improved": this migration is
-- about the `tracks` shape, and a search behaviour change smuggled in here
-- would be invisible to anyone reading either file.
CREATE VIRTUAL TABLE tracks_fts USING fts5(
    title,
    artist,
    album,
    content='tracks',
    content_rowid='id',
    tokenize="unicode61 remove_diacritics 2"
);

INSERT INTO tracks_fts (rowid, title, artist, album)
SELECT id, title, artist, album FROM tracks;

CREATE TRIGGER tracks_fts_ai AFTER INSERT ON tracks BEGIN
    INSERT INTO tracks_fts (rowid, title, artist, album)
    VALUES (new.id, new.title, new.artist, new.album);
END;

CREATE TRIGGER tracks_fts_ad AFTER DELETE ON tracks BEGIN
    INSERT INTO tracks_fts (tracks_fts, rowid, title, artist, album)
    VALUES ('delete', old.id, old.title, old.artist, old.album);
END;

-- Still scoped to the indexed columns: an unqualified AFTER UPDATE would
-- reindex the whole library on every rescan and undo the mtime-skip.
CREATE TRIGGER tracks_fts_au AFTER UPDATE OF title, artist, album ON tracks BEGIN
    INSERT INTO tracks_fts (tracks_fts, rowid, title, artist, album)
    VALUES ('delete', old.id, old.title, old.artist, old.album);

    INSERT INTO tracks_fts (rowid, title, artist, album)
    VALUES (new.id, new.title, new.artist, new.album);
END;
