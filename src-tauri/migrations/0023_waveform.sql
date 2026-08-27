-- The shape of a track, for drawing behind the seek bar.
--
-- One byte per column, 400 columns: about 400 bytes a track and ~425 KB for a
-- thousand-track library. Small enough to live in the row, which is what makes
-- reading it a query the UI already makes rather than a second round trip to a
-- file store.
--
-- Stored normalised against the track's own loudest sample, so it is a picture
-- of this recording's dynamics rather than of its level. That is the right
-- choice *because* this app corrects loudness at playback: a waveform scaled
-- by absolute level would draw every normalised track the same height.
ALTER TABLE tracks ADD COLUMN waveform BLOB;

-- Set even when measurement fails, so a file ffmpeg cannot read is attempted
-- once rather than on every play. Same idiom as `loudness_at` in 0019:
-- "never measured" is NULL, "measured and shapeless" is a timestamp with no
-- blob beside it.
ALTER TABLE tracks ADD COLUMN waveform_at INTEGER;
