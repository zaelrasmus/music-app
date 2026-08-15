-- Cover art.
--
-- Both columns hold a *key* into the cover store, never a path. The key is a
-- content hash, so two tracks from the same album point at one file, and the
-- store can be deleted wholesale and rebuilt from the files and URLs the art
-- came from. A path would tie a row to a directory layout and would break the
-- moment the app data folder moved.
--
-- NULL means "no art known", which the UI draws as generated artwork rather
-- than as a gap.
ALTER TABLE tracks ADD COLUMN cover_key TEXT;

-- `cover_path` was reserved by 0004 and never written to. It is renamed rather
-- than reused because it now holds a key, and a column called `path` holding
-- something that is not a path is how the next person loses an hour.
ALTER TABLE playlists RENAME COLUMN cover_path TO cover_key;
