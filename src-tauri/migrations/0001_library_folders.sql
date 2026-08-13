-- Root folders the user picked for their local music library.
--
-- LOCAL-ONLY: a filesystem path is meaningless (or wrong) on another device,
-- so this table must never be included in the future cross-device sync path.
-- That is also why a plain autoincrement id is safe here -- these ids never
-- cross a device boundary. Tracks and playlists will need client-generated
-- uuids instead.
--
-- `path`     canonical form, original casing, for display.
-- `path_key` comparison form, used only for uniqueness and containment checks.
CREATE TABLE library_folders (
    id       INTEGER PRIMARY KEY,
    path     TEXT    NOT NULL,
    path_key TEXT    NOT NULL UNIQUE,
    added_at INTEGER NOT NULL DEFAULT (unixepoch())
);
