-- Being in the library is now a decision, not a side effect.
--
-- Playing or queueing a search result has to create a track row -- the queue,
-- history, the cache and cover art are all keyed by track id, so there is no
-- playing something that is not a row. But a row is not a library entry, and
-- conflating the two meant auditioning ten songs to find one left nine in the
-- library forever.
--
-- Defaults to 1 so nothing already there disappears: local files are always
-- library members, and remote tracks saved before this migration are already
-- visible in the library, so removing them would be the destructive reading of
-- an upgrade. New remote saves set 0 explicitly.
ALTER TABLE tracks ADD COLUMN in_library INTEGER NOT NULL DEFAULT 1;

-- When the upload was published, for sorting.
--
-- Populated from the provider's own timestamp at save time. SoundCloud reports
-- one in search results; YouTube does not, and getting it would cost a separate
-- extraction per result. NULL therefore means "not known", not "no date", and
-- sorts last in both directions rather than pretending to be the epoch.
ALTER TABLE tracks ADD COLUMN uploaded_at INTEGER;

-- Library listing filters on this, so it earns an index. Partial, because the
-- only query that runs is "the ones that are in it" -- indexing the excluded
-- rows would be paying to find things nothing ever looks for.
CREATE INDEX idx_tracks_in_library ON tracks (in_library) WHERE in_library = 1;
