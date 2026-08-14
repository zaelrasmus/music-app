-- Full-text search over the user's own library.
--
-- ⚠ IF `tracks` IS EVER REBUILT AGAIN, THIS BREAKS SILENTLY.
--
-- Migration 0003 rebuilt `tracks` the only way SQLite allows: CREATE new,
-- INSERT SELECT, DROP old, RENAME. A `DROP TABLE tracks` also drops the three
-- triggers below and leaves this external-content index pointing at a table
-- that no longer exists. Searches then return stale or missing rows with no
-- error at all.
--
-- Any future rebuild of `tracks` must drop `tracks_fts` and its triggers first,
-- then recreate and backfill them afterwards.
--
-- External content: the text lives in `tracks` and is not duplicated here.
-- `content_rowid='id'` works because `tracks.id` is an INTEGER PRIMARY KEY and
-- therefore the rowid itself.
--
-- `remove_diacritics 2` is what makes searching "cancion" find "canción".
-- Verified against the bundled SQLite (3.51.3) before committing to it.
CREATE VIRTUAL TABLE tracks_fts USING fts5(
    title,
    artist,
    album,
    content='tracks',
    content_rowid='id',
    tokenize="unicode61 remove_diacritics 2"
);

-- Existing rows would otherwise be invisible to search until each happened to
-- be edited.
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

-- Scoped to the indexed columns on purpose.
--
-- The scanner's fast path updates `last_seen_scan`, `state` and `folder_id` for
-- every unchanged file on every rescan. An unqualified AFTER UPDATE trigger
-- would delete and reinsert the FTS row for the entire library each time,
-- undoing the mtime-skip optimisation that makes rescans fast at all.
CREATE TRIGGER tracks_fts_au AFTER UPDATE OF title, artist, album ON tracks BEGIN
    INSERT INTO tracks_fts (tracks_fts, rowid, title, artist, album)
    VALUES ('delete', old.id, old.title, old.artist, old.album);

    INSERT INTO tracks_fts (rowid, title, artist, album)
    VALUES (new.id, new.title, new.artist, new.album);
END;
