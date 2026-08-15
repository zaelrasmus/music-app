-- Re-examines MP4s whose artwork was silently dropped.
--
-- Every `.m4a` scanned before this migration was recorded as examined
-- (`cover_checked = 1`) and coverless, because the picture never survived
-- lofty's conversion into its unified tag -- see `mp4_cover` in `scanner.rs`.
-- The scanner now reads the concrete MP4 tag instead, but a rescan alone would
-- not help: `cover_checked = 1` is exactly what stops a file being read again.
--
-- Clearing the flag costs one re-read of the affected files, after which the
-- flag is set again and they are skipped as before.
--
-- Deliberately narrow on all three counts:
--
--   * `cover_key IS NULL` -- a file that already found its cover has nothing to
--     gain, and re-reading it would be pure cost.
--   * the extension list -- the bug was specific to MP4 containers. MP3 and
--     FLAC artwork was never affected, so their untagged files must not be
--     dragged through another read.
--   * `source = 'local'` -- remote rows take their artwork from the provider
--     thumbnail, not from a file on disk.
--
-- SQLite's LIKE is case-insensitive for ASCII, so `.M4A` matches too.
UPDATE tracks
SET cover_checked = 0
WHERE source = 'local'
  AND cover_key IS NULL
  AND (
    local_path LIKE '%.m4a'
    OR local_path LIKE '%.mp4'
    OR local_path LIKE '%.m4b'
  );
