-- Per-track loudness, so playback can be levelled.
--
-- Measured with EBU R128 (the standard ReplayGain 2.0 uses), stored rather
-- than computed at play time because measuring means decoding the whole track:
-- fast in bulk, far too slow to do in the moment someone presses play.
--
-- `loudness_at` is set even when measurement fails, so a file that cannot be
-- read is not retried on every pass. "Never analysed" is `loudness_at IS NULL`;
-- "analysed and unmeasurable" is a timestamp with a NULL reading.
ALTER TABLE tracks ADD COLUMN loudness_lufs REAL;
ALTER TABLE tracks ADD COLUMN loudness_peak REAL;
ALTER TABLE tracks ADD COLUMN loudness_at TEXT;

-- The analysis pass asks for unmeasured tracks repeatedly and in order.
CREATE INDEX IF NOT EXISTS idx_tracks_loudness_pending
    ON tracks (loudness_at)
    WHERE loudness_at IS NULL;
