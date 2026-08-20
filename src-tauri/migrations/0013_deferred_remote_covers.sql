-- Stop paying for artwork nobody asked to keep.
--
-- Auditioning a streamed track writes a row, and that row used to pull a
-- ~30 KB cover down with it. Nine rejected auditions out of ten still left
-- their picture on disk, referenced by a row the user never wanted, so the
-- sweep could not reclaim it -- it was not orphaned, just unwanted. Five
-- hundred previews came to roughly 15 MB, invisible until a disk filled.
--
-- Cover art is now fetched at the moment a track is *kept*: added to the
-- library, or downloaded. Everything else falls back to the provider's own
-- thumbnail URL, which is already on the row and which the webview is allowed
-- to load directly -- so nothing looks any different, it just costs nothing.
--
-- This clears the keys already written for tracks in that state. The files
-- themselves go on the next sweep, which now also runs once at startup.
UPDATE tracks
   SET cover_key = NULL
 WHERE source <> 'local'
   AND in_library = 0
   AND state <> 'downloaded';
