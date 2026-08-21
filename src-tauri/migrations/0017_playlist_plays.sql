-- When a playlist was last played, and how often.
--
-- Deliberately *not* derived from the tracks inside it. `MAX(tracks.last_played)`
-- over the members costs nothing and is free of new state, but it attributes
-- the wrong thing: play one song from the library that happens to sit in some
-- playlist, and that playlist rises to the top of "recently played" though it
-- was never opened. An ordering nobody can explain is one nobody trusts, and a
-- sort that is not trusted is not used.
--
-- So these count *plays of the playlist*: putting this list on, deliberately.
-- That is the question "which did I listen to last" is actually asking, and it
-- is the only reading that stays true as tracks move between playlists.
--
-- Both start empty for every existing playlist, so the orders they feed mean
-- nothing until the app has been used for a while. `created_at` breaks the tie,
-- which is the order the grid had before this.
ALTER TABLE playlists ADD COLUMN last_played INTEGER;
ALTER TABLE playlists ADD COLUMN play_count INTEGER NOT NULL DEFAULT 0;
