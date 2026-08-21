-- Playlists that fill themselves from an artist.
--
-- The problem this solves is not "there is no way to collect an artist's
-- songs" -- a playlist already does that. It is that the collection goes stale
-- the moment a new track is saved, so keeping one playlist per artist means
-- maintaining fifty of them by hand. A rule turns that into a standing
-- statement: *everything by these names belongs here*, including what arrives
-- tomorrow.
--
-- Deliberately attached to `playlists` rather than being a new kind of object.
-- An artist collection and a themed collection are the same thing to a user --
-- a named list you open and play -- and giving them separate storage would
-- mean two of everything: two pages, two ways to reorder, two answers to "can
-- I rename this". The only real difference is where the membership comes from,
-- and that is exactly what these two tables record.

-- The names that count as this playlist's artist.
--
-- Several per playlist because one artist is routinely several names: a
-- SoundCloud handle and a YouTube channel differ (`ivycomb` against
-- `Ivycomb Music`), and providers rename. No attempt is made to detect that
-- automatically. Identity here is *asserted* by the user, never inferred --
-- the alternative is a similarity rule that eventually fuses two different
-- bands, which is the kind of wrong nobody notices until their library is.
CREATE TABLE playlist_artist_rules (
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,

    -- Trimmed and lowercased. Matching lives here, so the same name in two
    -- casings is one rule.
    artist_key  TEXT    NOT NULL,

    -- What the user saw when they picked it, for the chip. Display only.
    label       TEXT    NOT NULL,

    added_at    INTEGER NOT NULL DEFAULT (unixepoch()),

    PRIMARY KEY (playlist_id, artist_key)
);

-- Tracks a rule would include that the user has taken out.
--
-- Without this, "remove" is a button that does nothing: the row is deleted and
-- the rule puts it straight back, so the user is left fighting the app. The
-- exclusion is what makes removal mean something on a list nobody enumerated.
--
-- Only meaningful against rule matches. A hand-added track is removed by
-- deleting its `playlist_tracks` row, exactly as before.
CREATE TABLE playlist_excluded_tracks (
    playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    track_id    INTEGER NOT NULL REFERENCES tracks(id)    ON DELETE CASCADE,
    excluded_at INTEGER NOT NULL DEFAULT (unixepoch()),

    PRIMARY KEY (playlist_id, track_id)
);

-- Membership is resolved on every read rather than materialised into
-- `playlist_tracks`, so there is no sync step that can fall behind and no
-- second copy of the truth. The cost is a scan of `tracks` for a playlist that
-- has rules; this index is what keeps that to the artists actually named.
--
-- `remote_uploader` first because it is the provider's own name for the
-- channel and nothing in the app ever edits it. `artist` is the *display*
-- copy, which the user may rename -- matching on that would silently drop a
-- track out of its own artist's playlist the moment they tidied its title.
CREATE INDEX idx_tracks_artist_key
    ON tracks (lower(trim(COALESCE(NULLIF(trim(remote_uploader), ''), artist))));
