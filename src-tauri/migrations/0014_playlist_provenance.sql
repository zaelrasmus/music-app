-- Where an imported playlist came from.
--
-- Both nullable, and both null for every playlist made by hand -- which is
-- what distinguishes the two kinds without a flag that could disagree with
-- them. A playlist that knows its origin can say so in the UI, and can later
-- be refreshed against it; one that does not is simply the user's own.
--
-- The tracks inside are ordinary rows either way. Nothing here makes an
-- imported playlist a second class of object -- it can be renamed, reordered,
-- added to and deleted exactly like any other, and doing so does not sever
-- this link, because the link records where the playlist *came from*, not what
-- it is obliged to stay.
ALTER TABLE playlists ADD COLUMN source TEXT
    CHECK (source IS NULL OR source IN ('youtube', 'soundcloud'));

ALTER TABLE playlists ADD COLUMN source_url TEXT;
