-- Lyrics, cached per *song* rather than per track row.
--
-- The distinction is the whole design. A local FLAC of a song, the YouTube
-- upload of it and a SoundCloud re-upload are three `tracks` rows and one
-- song. Keying this table on `track_id` would fetch the same lyrics three
-- times and would leave a match found for one row unavailable to the others.
-- `identity_key` is a normalised (artist, title) pair -- see `lyrics::identify`
-- -- so all three share one entry.
--
-- Duration is deliberately *not* part of the key. It is what ranks candidates
-- at fetch time, not what identifies a song: a YouTube rip carrying fourteen
-- seconds of trailing silence is still the same song, and bucketing by length
-- would split it off from the file on disk.
--
-- NOT CACHED HERE: lyrics read from a `.lrc` beside the file or from the
-- file's own tags. Those cost a millisecond to read, belong to one file rather
-- than to a song, and caching them would only create a way for the cache to
-- disagree with the file. This table holds provider results.
CREATE TABLE lyrics (
    id           INTEGER PRIMARY KEY,
    identity_key TEXT    NOT NULL UNIQUE,

    -- LRC source text, stored verbatim rather than pre-parsed. It is small,
    -- it is the thing the provider actually said, and keeping it raw means a
    -- fix to the parser repairs every existing row instead of requiring a
    -- refetch. `[offset:]` inside it is applied at parse time.
    synced       TEXT,
    plain        TEXT,

    -- A positive answer with no words in it. lrclib reports this, and on a
    -- library of game soundtracks it is the *common* case -- "this track has
    -- no vocals" is a correct answer and must not be rendered as a failure.
    instrumental INTEGER NOT NULL DEFAULT 0 CHECK (instrumental IN (0, 1)),

    provider     TEXT    NOT NULL,
    fetched_at   INTEGER NOT NULL DEFAULT (unixepoch())
);

-- A row with no synced, no plain and instrumental = 0 is the negative cache:
-- we asked and there was nothing. Same idiom as `loudness_at` in 0019 --
-- "analysed and unmeasurable" is recorded, so it is not retried forever.
-- Without it, every replay of a track with no lyrics is another request to
-- lrclib, which is donated infrastructure.

-- How far a track's lyrics are shifted from its audio, in milliseconds.
--
-- Not redundant with the `[offset:]` tag inside the LRC: that one belongs to
-- the lyrics and is the same for everyone, this one belongs to *this row's
-- audio*. A YouTube upload with a three-second intro card needs it and the
-- release does not, and they can share a `lyrics` entry. The two are summed.
ALTER TABLE tracks ADD COLUMN lyrics_offset_ms INTEGER NOT NULL DEFAULT 0;
