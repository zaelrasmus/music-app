//! Per-track loudness, measured once and applied at playback.
//!
//! This library spans about ten decibels of mastered loudness -- measured,
//! `Mi4 - Reincarnation Program` is -6.6 LUFS and `Beyond Infinite` is -12.7 --
//! and no volume curve can make both comfortable at one slider position. That
//! is arithmetic, not tuning. The only fix is to know how loud each track
//! actually is and correct for it.
//!
//! Measuring is cheap: EBU R128 through the bundled ffmpeg runs at 200-500x
//! realtime (measured, 471 ms for a 143 s track and 797 ms for a 396 s one), so
//! a thousand-file library is minutes of background work, once.
//!
//! Measuring *ahead of time* is the part that cannot always be done. A local
//! file can be analysed whenever; a stream cannot be analysed before it has
//! been fetched, and yt-dlp exposes no loudness metadata to borrow instead
//! (checked: 118 KB of JSON for a YouTube track, no `loudness` field anywhere).
//! So a stream is measured from the copy the audio cache keeps after its first
//! complete play, and the *first* play of a never-heard stream is the one case
//! that goes out unnormalised. That is a small slice of listening and not a
//! reason to skip the feature.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use sqlx::{Row, SqlitePool};

use crate::audio_cache::AudioCache;

/// Where a normalised track lands, unless the listener says otherwise.
///
/// -14 LUFS is what YouTube and Spotify converge on, which matters because it
/// is the level this library's streamed half was mastered to sit near. It is
/// the default rather than the only choice: the right target depends on what
/// someone is listening on, and the range below is what makes sense to offer.
pub const TARGET_LUFS: f32 = -14.0;

/// The quietest and loudest targets worth offering.
///
/// Quieter than -23 is broadcast territory and would leave this library barely
/// audible on a laptop. Louder than -9 asks for more boost than most tracks
/// have headroom for, so the limiter would spend its time undoing it and the
/// levelling would stop being a level change.
pub const MIN_TARGET_LUFS: f32 = -23.0;
pub const MAX_TARGET_LUFS: f32 = -9.0;

/// The most a track may be pushed up, and pulled down.
///
/// Bounded because a wildly wrong measurement -- a corrupt file, a track that
/// is thirty seconds of silence and one drum hit -- should misbehave quietly
/// rather than deafen anyone.
pub const MAX_BOOST_DB: f32 = 12.0;
pub const MAX_CUT_DB: f32 = 24.0;

/// The highest true peak a *boosted* track may be pushed to.
///
/// Boosting is only safe because the look-ahead limiter in `engine.rs` catches
/// what would clip. It is not free, though: measured on a +4.99 dBFS master the
/// limiter costs about 1 dB of average level and leaves a flat -20 dB error, so
/// driving peaks far past full scale would trade one audible problem for
/// another. Three decibels is inside what measured transparent.
///
/// Only applied to boosts. Attenuating can never clip.
pub const PEAK_CEILING_DB: f32 = 3.0;

/// How many tracks one pass of the analyser measures before yielding.
const BATCH: i64 = 24;

/// How long the analyser waits between passes once it has caught up.
///
/// Long, because the only thing that creates new work is a scan or a stream
/// finishing, and neither is urgent -- the track is already playing by then.
const IDLE_PAUSE: Duration = Duration::from_secs(60);

/// How long a single measurement may take before it is abandoned.
///
/// Generous against the measured 200-500x realtime: an hour-long upload is
/// still seconds of work, so anything near this is a file that will never
/// finish rather than a slow one.
const MEASURE_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loudness {
    /// Integrated loudness, EBU R128.
    pub lufs: f32,
    /// True peak in dBFS. Positive is above full scale, which is ordinary for
    /// a lossy decode -- 26 of 28 sampled tracks in this library are.
    pub true_peak_db: f32,
}

/// The gain to play a track at, in decibels.
pub fn gain_db(measured: Loudness, target: f32) -> f32 {
    if !measured.lufs.is_finite() {
        return 0.0;
    }
    let wanted = (target - measured.lufs).clamp(-MAX_CUT_DB, MAX_BOOST_DB);
    if wanted <= 0.0 {
        // Turning a track down cannot make it clip, so nothing else to check.
        return wanted;
    }
    let allowed = if measured.true_peak_db.is_finite() {
        (PEAK_CEILING_DB - measured.true_peak_db).max(0.0)
    } else {
        0.0
    };
    wanted.min(allowed)
}

/// Reads ffmpeg's `ebur128` summary.
///
/// Split out from the process handling so the parsing can be tested against
/// real output rather than against what this file assumes the output looks
/// like -- the two have differed before elsewhere in this crate.
pub fn parse_summary(stderr: &str) -> Option<Loudness> {
    let mut lufs: Option<f32> = None;
    let mut peak: Option<f32> = None;
    let mut in_true_peak = false;

    for line in stderr.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("True peak:") {
            in_true_peak = true;
            continue;
        }
        // Every other heading ends the True peak block.
        if trimmed.ends_with(':') && !trimmed.starts_with("Peak:") {
            in_true_peak = trimmed.starts_with("True peak:");
        }
        if let Some(rest) = trimmed.strip_prefix("I:") {
            // "I:          -6.6 LUFS"
            lufs = rest.split_whitespace().next().and_then(|v| v.parse().ok());
        }
        if in_true_peak {
            if let Some(rest) = trimmed.strip_prefix("Peak:") {
                // "Peak:        2.7 dBFS"
                peak = rest.split_whitespace().next().and_then(|v| v.parse().ok());
            }
        }
    }

    match (lufs, peak) {
        // A track of pure silence measures -inf and is not worth a gain.
        (Some(l), Some(p)) if l.is_finite() => Some(Loudness {
            lufs: l,
            true_peak_db: p,
        }),
        _ => None,
    }
}

/// Measures one file. Blocking -- call it off the async runtime.
pub fn measure(ffmpeg: &Path, path: &Path) -> Result<Loudness, String> {
    let mut child = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostats"])
        .arg("-i")
        .arg(path)
        // `peak=true` is what adds the True peak block; without it there is no
        // way to know how much headroom a boost has to play with.
        .args(["-af", "ebur128=peak=true:framelog=quiet"])
        .args(["-f", "null", "-"])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;

    let deadline = std::time::Instant::now() + MEASURE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                return Err("measurement timed out".to_string());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("ffmpeg failed: {e}")),
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("could not read ffmpeg: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    parse_summary(&stderr).ok_or_else(|| "ffmpeg reported no loudness".to_string())
}

/// The stored reading for a track, if it has one.
pub async fn stored(pool: &SqlitePool, track_id: i64) -> Option<Loudness> {
    let row = sqlx::query("SELECT loudness_lufs, loudness_peak FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_optional(pool)
        .await
        .ok()??;
    let lufs: Option<f64> = row.try_get("loudness_lufs").ok()?;
    let peak: Option<f64> = row.try_get("loudness_peak").ok()?;
    Some(Loudness {
        lufs: lufs? as f32,
        true_peak_db: peak? as f32,
    })
}

/// Records a reading, or the fact that one could not be taken.
///
/// `loudness_at` is stamped either way. A file that cannot be measured must
/// not be retried on every pass -- see the note in `0019_track_loudness.sql`.
pub(crate) async fn record(pool: &SqlitePool, track_id: i64, measured: Option<Loudness>) {
    write_reading(pool, track_id, measured, false).await
}

/// Records a reading taken by sampling rather than by measuring in full.
///
/// Kept apart from [`record`] because of what it must *not* do: mark the track
/// finished. The background pass looks for `loudness_at IS NULL`, so a sampled
/// reading written like an exact one would end the search and the estimate
/// would stand forever. The flag keeps the track on the list until the real
/// figure can be taken from the cache copy playback already wrote.
pub(crate) async fn record_sampled(pool: &SqlitePool, track_id: i64, measured: Loudness) {
    write_reading(pool, track_id, Some(measured), true).await
}

async fn write_reading(
    pool: &SqlitePool,
    track_id: i64,
    measured: Option<Loudness>,
    sampled: bool,
) {
    let _ = sqlx::query(
        "UPDATE tracks
            SET loudness_lufs = ?, loudness_peak = ?, loudness_at = datetime('now'),
                loudness_sampled = ?
          WHERE id = ?",
    )
    .bind(measured.map(|m| m.lufs as f64))
    .bind(measured.map(|m| m.true_peak_db as f64))
    .bind(i64::from(sampled))
    .bind(track_id)
    .execute(pool)
    .await;
}

/// How many files are measured at once by the background pass.
///
/// Half the machine's parallelism, capped. ffmpeg is a process per file and
/// will happily take every core, which is the wrong trade for work whose whole
/// point is to happen unnoticed while someone listens to music.
fn concurrency() -> usize {
    std::thread::available_parallelism()
        .map(|n| (n.get() / 2).clamp(1, 4))
        .unwrap_or(1)
}

/// A track waiting to be measured, and the file to measure.
struct Pending {
    id: i64,
    path: PathBuf,
}

/// The file to measure for one track: its own, or the cache's copy of it.
fn file_for(
    source: &str,
    local_path: Option<String>,
    remote_id: Option<String>,
    cache: Option<&AudioCache>,
) -> Option<PathBuf> {
    if let Some(local) = local_path.filter(|p| !p.is_empty()) {
        let path = PathBuf::from(local);
        if path.exists() {
            return Some(path);
        }
    }
    let remote = remote_id.filter(|r| !r.is_empty())?;
    cache?.lookup(source, &remote)
}

/// Finds tracks with no reading yet that have a file on disk to read.
///
/// A streamed track qualifies as soon as the audio cache holds a copy, which
/// happens after its first complete play. One query and one code path covers
/// both halves of the library.
async fn pending(pool: &SqlitePool, cache: Option<&AudioCache>, limit: i64) -> Vec<Pending> {
    let rows = sqlx::query(
        "SELECT id, source, local_path, remote_id
           FROM tracks
          WHERE loudness_at IS NULL OR loudness_sampled = 1
          ORDER BY id
          LIMIT ?",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut out = Vec::new();
    for row in rows {
        let id: i64 = row.try_get("id").unwrap_or_default();
        let source: String = row.try_get("source").unwrap_or_default();
        let local: Option<String> = row.try_get("local_path").ok().flatten();
        let remote: Option<String> = row.try_get("remote_id").ok().flatten();

        if let Some(path) = file_for(&source, local, remote, cache) {
            out.push(Pending { id, path });
        }
    }
    out
}

/// Measures one batch. Returns how many were attempted.
///
/// Concurrent, because sequential was too slow to live with: a thousand-file
/// library at roughly 0.6 s a track is ten minutes of waiting before anything is
/// levelled, and the whole feature is invisible until it finishes.
pub async fn analyse_batch(
    pool: &SqlitePool,
    ffmpeg: &Path,
    cache: Option<&AudioCache>,
    limit: i64,
) -> usize {
    let waiting = pending(pool, cache, limit).await;
    let mut done = 0;

    for group in waiting.chunks(concurrency()) {
        // Started together, collected in order. `spawn_blocking` runs each one
        // immediately, so awaiting them one after another does not serialise
        // them -- ffmpeg is a process and must not park a runtime worker.
        let mut running = Vec::with_capacity(group.len());
        for item in group {
            let ffmpeg = ffmpeg.to_path_buf();
            let path = item.path.clone();
            running.push((
                item.id,
                tokio::task::spawn_blocking(move || measure(&ffmpeg, &path)),
            ));
        }

        for (id, handle) in running {
            let measured = handle
                .await
                .unwrap_or_else(|e| Err(format!("measurement panicked: {e}")));
            record(pool, id, measured.ok()).await;
            done += 1;
        }
    }

    done
}

/// The longest track worth fetching purely so it can be measured.
///
/// The cost here is a whole extra download of something the listener may skip
/// in ten seconds. An hour-long mix is not worth fifty megabytes of traffic to
/// find out how loud it is -- and a track that long is being listened to for
/// its own sake, not as one of a sequence that needs matching.
const MEASURE_FETCH_MAX_SECS: f64 = 20.0 * 60.0;

/// Whether a track already has a settled answer, and what it is.
///
/// `loudness_at` is stamped even when measurement failed, so "tried and could
/// not" is distinguishable from "never tried" -- without that, an unmeasurable
/// file would be fetched and re-measured on every pass forever.
///
/// A *sampled* reading is deliberately not settled. It is good enough to level
/// by and not good enough to stop looking: the exact figure is cheap once the
/// audio is on disk, and leaving the estimate in place would make a stopgap
/// permanent.
async fn settled(pool: &SqlitePool, track_id: i64) -> Option<bool> {
    let row = sqlx::query(
        "SELECT loudness_at, loudness_lufs, loudness_sampled FROM tracks WHERE id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .ok()??;

    let sampled: i64 = row.try_get("loudness_sampled").unwrap_or(0);
    if sampled != 0 {
        return Some(false);
    }

    let at: Option<String> = row.try_get("loudness_at").ok().flatten();
    at?;
    let lufs: Option<f64> = row.try_get("loudness_lufs").ok().flatten();
    Some(lufs.is_some())
}

async fn measure_and_record(
    pool: &SqlitePool,
    ffmpeg: &Path,
    path: PathBuf,
    track_id: i64,
) -> bool {
    let ffmpeg = ffmpeg.to_path_buf();
    let measured = tokio::task::spawn_blocking(move || measure(&ffmpeg, &path))
        .await
        .unwrap_or_else(|e| Err(format!("measurement panicked: {e}")));
    let ok = measured.is_ok();
    record(pool, track_id, measured.ok()).await;
    ok
}

/// Makes sure a track has a reading, fetching a copy of it first if that is
/// what it takes.
///
/// This is the piece that closes the cold-stream gap. Measuring needs the whole
/// track, and a stream has no whole track until it has been fetched -- so for
/// anything not already on disk this downloads it into the audio cache (which
/// is where it would have ended up after a complete play anyway) and measures
/// that. Measured, the round trip costs about 12 seconds for a six-minute
/// track: 4 s of yt-dlp resolve and 8 s of fetch-and-measure, roughly 50x
/// realtime.
///
/// Returns whether a usable reading exists afterwards.
pub async fn ensure_measured(
    pool: &SqlitePool,
    ffmpeg: Option<&Path>,
    yt_dlp: Option<&Path>,
    cache: Option<&AudioCache>,
    track_id: i64,
) -> bool {
    if let Some(answered) = settled(pool, track_id).await {
        return answered;
    }
    let Some(ffmpeg) = ffmpeg else {
        return false;
    };

    let Ok(Some(row)) = sqlx::query(
        "SELECT source, local_path, remote_id, remote_url, duration_secs \
         FROM tracks WHERE id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    else {
        return false;
    };

    let source: String = row.try_get("source").unwrap_or_default();
    let local: Option<String> = row.try_get("local_path").ok().flatten();
    let remote_id: Option<String> = row.try_get("remote_id").ok().flatten();

    // Already something to read: a local file, or a cache copy from an earlier
    // play. No network needed.
    if let Some(path) = file_for(&source, local, remote_id.clone(), cache) {
        return measure_and_record(pool, ffmpeg, path, track_id).await;
    }

    // Otherwise it has to be fetched, which is only worth doing for a remote
    // track of reasonable length.
    let (Some(yt_dlp), Some(cache), Some(remote_id)) = (yt_dlp, cache, remote_id) else {
        return false;
    };
    let Some(remote_url): Option<String> = row.try_get("remote_url").ok().flatten() else {
        return false;
    };
    // No duration is a live stream, which has no whole track to fetch.
    let Some(duration) = row.try_get::<Option<i64>, _>("duration_secs").ok().flatten() else {
        return false;
    };
    let duration = duration as f64;
    if duration <= 0.0 || duration > MEASURE_FETCH_MAX_SECS {
        return false;
    }

    // `reserve_fetch` is also the lock: it hands back nothing when a copy
    // already exists or another task is already fetching this one.
    let Some(pending) = cache.reserve_fetch(&source, &remote_id) else {
        return false;
    };
    if crate::download::fetch_into_cache(yt_dlp, ffmpeg, &remote_url, pending)
        .await
        .is_err()
    {
        return false;
    }

    let Some(path) = cache.lookup(&source, &remote_id) else {
        return false;
    };
    measure_and_record(pool, ffmpeg, path, track_id).await
}

/// Measures one track now, on the user's say-so.
///
/// Exists because the background pass is deliberately unhurried, and waiting
/// several minutes to hear whether levelling helps on *this* track is not a
/// reasonable thing to ask. Re-measures even if a reading already exists: the
/// button is also how someone retries a track that failed.
#[tauri::command]
pub async fn measure_track(
    app: tauri::AppHandle,
    db: tauri::State<'_, crate::db::Db>,
    cache: tauri::State<'_, AudioCache>,
    track_id: i64,
) -> Result<Option<f32>, String> {
    let ffmpeg = crate::sidecar::resolve(&app, crate::sidecar::Tool::Ffmpeg)
        .map(|found| found.path)
        .map_err(|_| "ffmpeg was not found, so nothing can be measured.".to_string())?;

    let row = sqlx::query("SELECT source, local_path, remote_id FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_optional(&db.pool)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "That track is no longer in the library.".to_string())?;

    let source: String = row.try_get("source").unwrap_or_default();
    let local: Option<String> = row.try_get("local_path").ok().flatten();
    let remote: Option<String> = row.try_get("remote_id").ok().flatten();

    let path = file_for(&source, local, remote, Some(&cache)).ok_or_else(|| {
        "There is no copy of this track to measure yet. Streamed tracks are \
         measured after they have played through once."
            .to_string()
    })?;

    let measured = tokio::task::spawn_blocking(move || measure(&ffmpeg, &path))
        .await
        .map_err(|e| format!("measurement panicked: {e}"))?;

    match measured {
        Ok(value) => {
            record(&db.pool, track_id, Some(value)).await;
            Ok(Some(gain_db(value, TARGET_LUFS)))
        }
        Err(reason) => {
            record(&db.pool, track_id, None).await;
            Err(format!("Could not measure this track: {reason}"))
        }
    }
}

/// Which tracks have a usable reading.
///
/// Ids rather than a field on `Track`, so the badge can refresh on its own
/// without every query that builds a row learning about loudness -- the same
/// shape `audio_cache::cached_track_ids` already uses for the offline badge.
#[tauri::command]
pub async fn measured_track_ids(db: tauri::State<'_, crate::db::Db>) -> Result<Vec<i64>, String> {
    sqlx::query_scalar("SELECT id FROM tracks WHERE loudness_lufs IS NOT NULL")
        .fetch_all(&db.pool)
        .await
        .map_err(|e| e.to_string())
}

/// Runs forever, measuring whatever has no reading yet.
///
/// Deliberately a slow background loop rather than something hooked to a scan:
/// new work arrives from two unrelated places -- a library scan, and a stream
/// finishing and leaving a cache copy -- and polling covers both without either
/// having to know this exists.
pub async fn run(pool: SqlitePool, ffmpeg: Option<PathBuf>, cache: Option<AudioCache>) {
    let Some(ffmpeg) = ffmpeg else {
        // Nothing to measure with. Playback still works; it is just unlevelled.
        return;
    };

    loop {
        let done = analyse_batch(&pool, &ffmpeg, cache.as_ref(), BATCH).await;
        if done == 0 {
            tokio::time::sleep(IDLE_PAUSE).await;
        } else {
            // A short breath between batches so a first scan does not saturate
            // every core while someone is trying to listen to music.
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real output, captured from the bundled ffmpeg against
    /// `Mi4 - Reincarnation Program.mp3`. Written out rather than described,
    /// because the format is ffmpeg's to change and a guess at it would fail
    /// silently -- `parse_summary` returning `None` reads as "unmeasurable".
    const REAL_SUMMARY: &str = "\
[Parsed_ebur128_0 @ 000001] Summary:

  Integrated loudness:
    I:          -6.6 LUFS
    Threshold: -16.7 LUFS

  Loudness range:
    LRA:         6.3 LU
    Threshold: -26.7 LUFS
    LRA low:   -11.5 LUFS
    LRA high:   -5.2 LUFS

  True peak:
    Peak:        2.7 dBFS
";

    #[test]
    fn a_real_summary_is_read_correctly() {
        let got = parse_summary(REAL_SUMMARY).expect("the real format must parse");
        assert!((got.lufs - -6.6).abs() < 1e-3, "lufs came out {}", got.lufs);
        assert!(
            (got.true_peak_db - 2.7).abs() < 1e-3,
            "true peak came out {}",
            got.true_peak_db
        );
    }

    /// The LRA block also carries numbers that look like readings. Taking the
    /// wrong one would silently mis-level every track rather than fail.
    #[test]
    fn the_loudness_range_block_is_not_mistaken_for_the_reading() {
        let got = parse_summary(REAL_SUMMARY).unwrap();
        for wrong in [-16.7f32, -26.7, -11.5, -5.2, 6.3] {
            assert!(
                (got.lufs - wrong).abs() > 1e-3,
                "the parser picked up {wrong}, which is not the integrated loudness",
            );
        }
    }

    #[test]
    fn output_without_a_true_peak_block_is_unmeasurable() {
        let no_peak = "  Integrated loudness:\n    I:          -6.6 LUFS\n";
        assert!(parse_summary(no_peak).is_none());
    }

    #[test]
    fn silence_is_not_given_a_gain() {
        let silent = "  Integrated loudness:\n    I:         -inf LUFS\n\
                      \n  True peak:\n    Peak:      -inf dBFS\n";
        assert!(parse_summary(silent).is_none(), "silence must not measure");
    }

    /// The two tracks this feature exists for.
    #[test]
    fn the_library_spread_is_actually_closed() {
        // Measured with the bundled ffmpeg, 2026-08-22.
        let loud = Loudness {
            lufs: -6.6,
            true_peak_db: 2.7,
        };
        let quiet = Loudness {
            lufs: -12.7,
            true_peak_db: -0.7,
        };

        let loud_db = gain_db(loud, TARGET_LUFS);
        let quiet_db = gain_db(quiet, TARGET_LUFS);

        assert!(loud_db < 0.0, "the loud master should come down, got {loud_db}");
        // 6.1 dB apart before; whatever is left after the peak cap is the gap
        // the listener still hears.
        let gap_before = (-6.6f32 - -12.7).abs();
        let gap_after = ((-6.6 + loud_db) - (-12.7 + quiet_db)).abs();
        assert!(
            gap_after < gap_before,
            "normalisation widened the gap: {gap_before} dB became {gap_after} dB",
        );
        assert!(
            gap_after < 1.0,
            "the two reference tracks are still {gap_after} dB apart",
        );
    }

    /// Boosting past this is what the limiter would have to eat.
    ///
    /// Stated as "a boost never *raises* a peak past the allowance", not "no
    /// peak ends up past it" -- a track already peaking at +4.99 dBFS is simply
    /// not boosted, and leaving it where it is is the correct answer rather
    /// than a violation. The first version of this test asserted the stronger
    /// thing and failed on exactly that case.
    #[test]
    fn a_boost_never_raises_a_peak_past_the_allowance() {
        for peak in [-6.0f32, -0.7, 0.0, 2.7, 4.99] {
            let quiet = Loudness {
                lufs: -24.0,
                true_peak_db: peak,
            };
            let gain = gain_db(quiet, TARGET_LUFS);
            assert!(gain >= 0.0, "a -24 LUFS track should be boosted, got {gain}");
            let after = peak + gain;
            assert!(
                after <= peak.max(PEAK_CEILING_DB) + 1e-3,
                "a track peaking at {peak} came out at {after}",
            );
        }
    }

    /// Attenuation has no peak to guard, so the cap must not interfere with it.
    #[test]
    fn a_cut_is_never_blocked_by_the_peak_cap() {
        let hot = Loudness {
            lufs: -5.0,
            true_peak_db: 4.99,
        };
        assert!((gain_db(hot, TARGET_LUFS) - -9.0).abs() < 1e-3);
    }

    #[test]
    fn a_broken_reading_is_ignored_rather_than_obeyed() {
        let nonsense = Loudness {
            lufs: f32::NAN,
            true_peak_db: 0.0,
        };
        assert_eq!(gain_db(nonsense, TARGET_LUFS), 0.0);
    }

    async fn track_with(pool: &SqlitePool, lufs: Option<f64>, stamped: bool) -> i64 {
        sqlx::query(
            "INSERT INTO tracks (source, title, state, remote_id, remote_url, duration_secs, \
             loudness_lufs, loudness_at) \
             VALUES ('youtube', 'A Track', 'saved', 'abcdefghijk', \
                     'https://www.youtube.com/watch?v=abcdefghijk', 200, ?, ?)",
        )
        .bind(lufs)
        .bind(if stamped { Some("2026-08-22") } else { None })
        .execute(pool)
        .await
        .unwrap();
        sqlx::query_scalar("SELECT id FROM tracks ORDER BY id DESC LIMIT 1")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn scratch(name: &str) -> crate::db::Db {
        let dir = std::env::temp_dir().join(format!("music-app-loudness-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::db::init(&dir).await.unwrap()
    }

    /// The guard that stops a track being fetched over and over.
    ///
    /// A file that cannot be measured stamps `loudness_at` with a NULL reading,
    /// and that has to read as "asked and answered". Without this, every pass --
    /// and every queue advance -- would re-download a track to fail on it again.
    #[tokio::test]
    async fn a_track_that_could_not_be_measured_is_not_fetched_again() {
        let db = scratch("failed").await;
        let id = track_with(&db.pool, None, true).await;

        assert_eq!(settled(&db.pool, id).await, Some(false));
        // No ffmpeg, no yt-dlp, no cache: if the guard failed this would still
        // have to return without touching the network.
        assert!(!ensure_measured(&db.pool, None, None, None, id).await);
    }

    #[tokio::test]
    async fn a_track_already_measured_is_reported_without_work() {
        let db = scratch("done").await;
        let id = track_with(&db.pool, Some(-9.5), true).await;

        assert_eq!(settled(&db.pool, id).await, Some(true));
        // Returns true despite having no tools at all, because it never needed
        // them -- the answer was already stored.
        assert!(ensure_measured(&db.pool, None, None, None, id).await);
    }

    /// Never measured is the one state that should still do work.
    #[tokio::test]
    async fn a_track_never_measured_is_not_treated_as_settled() {
        let db = scratch("fresh").await;
        let id = track_with(&db.pool, None, false).await;

        assert_eq!(settled(&db.pool, id).await, None);
        // Still false here only because there is no ffmpeg to measure with.
        assert!(!ensure_measured(&db.pool, None, None, None, id).await);
    }

    #[test]
    fn the_gain_is_bounded_in_both_directions() {
        let absurdly_quiet = Loudness {
            lufs: -90.0,
            true_peak_db: -60.0,
        };
        let absurdly_loud = Loudness {
            lufs: 10.0,
            true_peak_db: 12.0,
        };
        assert!(gain_db(absurdly_quiet, TARGET_LUFS) <= MAX_BOOST_DB);
        assert!(gain_db(absurdly_loud, TARGET_LUFS) >= -MAX_CUT_DB);
    }
}

#[cfg(test)]
mod sampled_tests {
    use super::*;

    async fn track_with(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO tracks (source, title, local_path, state) \
             VALUES ('local', 'x', 'x.wav', 'present') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    /// A sampled reading must be usable *and* still be looking for better.
    ///
    /// This is the failure the flag exists for. The background pass finds work
    /// with `loudness_at IS NULL`, and a sampled reading writes that column --
    /// so without the flag the estimate would end the search and stand forever,
    /// which is precisely the thing it was introduced to avoid. It was written
    /// that way first, and this is what would have caught it.
    #[tokio::test]
    async fn a_sampled_reading_is_used_but_not_treated_as_final() {
        let dir = std::env::temp_dir().join("music-app-sampled-flag");
        let _ = std::fs::remove_dir_all(&dir);
        let db = crate::db::init(&dir).await.unwrap();
        let id = track_with(&db.pool).await;

        record_sampled(
            &db.pool,
            id,
            Loudness {
                lufs: -9.0,
                true_peak_db: -1.0,
            },
        )
        .await;

        // Usable: the player reads it and levels by it.
        let stored = stored(&db.pool, id).await;
        assert!(stored.is_some(), "a sampled reading should be readable");
        assert_eq!(stored.unwrap().lufs, -9.0);

        // Not final: the analyser must still pick this track up.
        assert_eq!(
            settled(&db.pool, id).await,
            Some(false),
            "a sampled reading was treated as final, so the exact figure would \
             never be taken and the estimate would be permanent",
        );

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// And once the real measurement lands, the flag clears.
    #[tokio::test]
    async fn measuring_in_full_replaces_a_sampled_reading() {
        let dir = std::env::temp_dir().join("music-app-sampled-replace");
        let _ = std::fs::remove_dir_all(&dir);
        let db = crate::db::init(&dir).await.unwrap();
        let id = track_with(&db.pool).await;

        record_sampled(
            &db.pool,
            id,
            Loudness {
                lufs: -9.0,
                true_peak_db: -1.0,
            },
        )
        .await;
        record(
            &db.pool,
            id,
            Some(Loudness {
                lufs: -8.4,
                true_peak_db: -0.8,
            }),
        )
        .await;

        assert_eq!(stored(&db.pool, id).await.unwrap().lufs, -8.4);
        assert_eq!(
            settled(&db.pool, id).await,
            Some(true),
            "an exact measurement should settle the track",
        );

        db.pool.close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod target_tests {
    use super::*;

    /// The target moves the gain, which is the whole point of offering it.
    #[test]
    fn a_quieter_target_asks_for_less_gain() {
        let quiet = Loudness {
            lufs: -20.0,
            true_peak_db: -3.0,
        };
        let at_14 = gain_db(quiet, TARGET_LUFS);
        let at_18 = gain_db(quiet, -18.0);

        assert!(
            at_18 < at_14,
            "aiming quieter should ask for less boost: {at_18} vs {at_14}",
        );
    }

    /// And the offered range stays inside what the gain limiter allows, so a
    /// choice at either end is still a choice rather than a no-op.
    #[test]
    fn both_ends_of_the_range_still_change_something() {
        let ordinary = Loudness {
            lufs: -14.0,
            true_peak_db: -1.0,
        };
        let quietest = gain_db(ordinary, MIN_TARGET_LUFS);
        let loudest = gain_db(ordinary, MAX_TARGET_LUFS);

        assert!(
            quietest < 0.0,
            "the quietest target should pull an already-on-target track down",
        );
        assert!(
            loudest > 0.0,
            "the loudest target should push an already-on-target track up",
        );
        assert!(
            loudest > quietest,
            "the range should be ordered: {quietest} then {loudest}",
        );
    }
}
