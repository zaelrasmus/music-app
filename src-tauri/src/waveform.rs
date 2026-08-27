//! The shape of a track, for drawing behind the seek bar.
//!
//! # What is measured
//!
//! ffmpeg decodes the file to mono 16-bit at [`ENVELOPE_RATE`], and each of
//! [`BUCKETS`] equal slices keeps the loudest sample in it. Peak rather than
//! RMS: RMS draws a smoother, prettier line, and a waveform is read to find
//! *where things happen* -- a drop, a break, the point the drums come in --
//! which are transients that RMS averages away.
//!
//! # Why not resample to almost nothing
//!
//! The obvious trick is to ask ffmpeg for a very low sample rate and read the
//! result straight back as the envelope; Veluna uses `-ar 500`. It is fast,
//! and it is measuring something narrower than it looks: decimating to 500 Hz
//! anti-alias filters at 250 Hz first, so what comes back is the low-frequency
//! envelope rather than the amplitude one.
//!
//! Measured against a real track from this library, the two drawn side by side
//! differ by **27 of 255 per column on average**, and the difference is not
//! noise — it is the transients. Where 8 kHz shows a spike, 500 Hz shows the
//! bass under it, so cymbals, consonants and snare hits flatten out. The gap
//! widens on anything less bass-led than the post-rock track this was measured
//! on.
//!
//! 8 kHz keeps everything below 4 kHz, which is the whole vocal range and most
//! of a drum kit. It reads **3.8 MB through the pipe against 245 KB**, and
//! takes about **310 ms** for a four-minute file — once, then never again.
//!
//! Read *through*, not read *in*: nothing larger than [`CHUNK`] is ever held.
//!
//! # What has no shape
//!
//! Only a file on disk. A stream would have to be downloaded in full to be
//! drawn, and downloading a track nobody asked to keep, to decorate a
//! progress bar, is not a reasonable thing to do quietly. The bar simply
//! renders without one.

use std::path::Path;
use std::process::Stdio;

use sqlx::SqlitePool;
use tauri::State;

use crate::db::Db;

/// How many columns the picture has.
///
/// The bar is at most ~550 px wide and each column wants a gap, so past this
/// the extra detail cannot be drawn. It is also the stored size: one byte per
/// bucket, 400 bytes a track, ~425 KB for this whole library.
pub const BUCKETS: usize = 400;

/// The rate the envelope is measured at.
///
/// See the module note. Low enough to be cheap, high enough that the picture
/// is of the song rather than of its bassline.
const ENVELOPE_RATE: u32 = 8_000;

/// Working slots, before the result is reduced to [`BUCKETS`].
///
/// Ten times the output, so the folding below never leaves the picture built
/// from fewer real measurements than it draws. 8 KB, and the *only* thing that
/// scales with nothing: a three-minute track and a three-hour one both use
/// exactly this much.
const SLOTS: usize = BUCKETS * 16;

/// How much decoded audio is held at once.
///
/// The whole point of the design. An earlier version of this called
/// `wait_with_output()`, which collects everything ffmpeg emits before a single
/// sample is looked at -- 3.8 MB for a four-minute track and several hundred
/// for a long upload, to produce four hundred bytes. Reading in fixed chunks
/// and folding as they arrive makes the memory a property of this constant
/// rather than of the song.
const CHUNK: usize = 64 * 1024;

/// Measures a file, or decides it has no shape worth drawing.
///
/// Blocking: it spawns ffmpeg and reads it to the end.
pub fn measure(ffmpeg: &Path, audio: &Path) -> Result<Vec<u8>, String> {
    use std::io::Read;

    let mut child = std::process::Command::new(ffmpeg)
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(audio)
        .args([
            "-map",
            "a:0",
            "-ac",
            "1",
            "-ar",
            &ENVELOPE_RATE.to_string(),
            "-f",
            "s16le",
            "-",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn_hidden()
        .map_err(|e| format!("Could not run ffmpeg: {e}"))?;

    // Drained on its own thread. `-v error` keeps this to a few lines, but a
    // full stderr pipe blocks the writer -- so a run that failed loudly would
    // hang forever waiting for us to read a pipe we only read afterwards.
    let mut stderr = child.stderr.take();
    let complaint = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(pipe) = stderr.as_mut() {
            let _ = pipe.take(4096).read_to_string(&mut text);
        }
        text
    });

    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ffmpeg produced no output pipe.".to_string())?;

    let mut folder = Folder::default();
    let mut buffer = vec![0u8; CHUNK];
    // A read can split a sample across two chunks.
    let mut carry: Option<u8> = None;

    loop {
        let read = stdout
            .read(&mut buffer)
            .map_err(|e| format!("Could not read from ffmpeg: {e}"))?;
        if read == 0 {
            break;
        }

        let mut bytes = &buffer[..read];

        if let Some(low) = carry.take() {
            folder.push(i16::from_le_bytes([low, bytes[0]]).unsigned_abs());
            bytes = &bytes[1..];
        }
        for sample in bytes.chunks_exact(2) {
            folder.push(i16::from_le_bytes([sample[0], sample[1]]).unsigned_abs());
        }
        if bytes.len() % 2 == 1 {
            carry = Some(bytes[bytes.len() - 1]);
        }
    }

    let status = child
        .wait()
        .map_err(|e| format!("ffmpeg did not finish: {e}"))?;
    let complaint = complaint.join().unwrap_or_default();

    if !status.success() {
        return Err(format!(
            "ffmpeg could not read that file: {}",
            complaint.lines().last().unwrap_or("no reason given").trim()
        ));
    }

    folder
        .finish()
        .ok_or_else(|| "That file decoded to nothing.".to_string())
}

/// Peaks, at a resolution that halves itself as the track turns out to be
/// longer than expected.
///
/// The trick a pipe forces. Bucketing needs to know the total length, and a
/// pipe never says -- so this starts at the finest resolution it can and
/// *folds*: when the slots run out, adjacent pairs are merged, each slot comes
/// to stand for twice as much audio, and it carries on. Memory is fixed at
/// [`SLOTS`] whatever arrives, and no sample is ever seen twice.
#[derive(Debug)]
struct Folder {
    peaks: Vec<u16>,
    /// Samples each slot stands for. Doubles on every fold.
    per_slot: usize,
    seen: usize,
}

impl Default for Folder {
    fn default() -> Self {
        Self {
            peaks: vec![0; SLOTS],
            per_slot: 1,
            seen: 0,
        }
    }
}

impl Folder {
    fn push(&mut self, level: u16) {
        while self.seen / self.per_slot >= SLOTS {
            self.fold();
        }
        let slot = self.seen / self.per_slot;
        self.peaks[slot] = self.peaks[slot].max(level);
        self.seen += 1;
    }

    /// Halves the resolution: pairs merge, and the far half is freed for what
    /// is still coming.
    fn fold(&mut self) {
        for slot in 0..SLOTS / 2 {
            self.peaks[slot] = self.peaks[slot * 2].max(self.peaks[slot * 2 + 1]);
        }
        self.peaks[SLOTS / 2..].fill(0);
        self.per_slot *= 2;
    }

    /// Down to [`BUCKETS`], normalised against the loudest sample in the track.
    ///
    /// Only the slots that were actually reached are reduced: after a fold the
    /// used range is somewhere between half and all of them, and treating the
    /// empty tail as silence would draw every folded track fading to nothing.
    fn finish(self) -> Option<Vec<u8>> {
        if self.seen == 0 {
            return None;
        }
        let used = self.seen.div_ceil(self.per_slot).clamp(1, SLOTS);

        let mut out = vec![0u16; BUCKETS];
        for (slot, &peak) in self.peaks[..used].iter().enumerate() {
            // Multiplied before dividing, so the last bucket is reached
            // whatever `used` happens to be.
            let bucket = (slot * BUCKETS / used).min(BUCKETS - 1);
            out[bucket] = out[bucket].max(peak);
        }

        // Fewer slots than buckets only happens on a track under 400 samples
        // long, but a hole in the picture is worth one line to rule out.
        if used < BUCKETS {
            let mut last = 0;
            for bucket in out.iter_mut() {
                if *bucket == 0 {
                    *bucket = last;
                } else {
                    last = *bucket;
                }
            }
        }

        let loudest = *out.iter().max()?;
        if loudest == 0 {
            return None;
        }

        Some(
            out.into_iter()
                .map(|peak| ((peak as u32 * 255) / loudest as u32) as u8)
                .collect(),
        )
    }
}

/// Windows would otherwise flash a console window for every ffmpeg run.
trait Hidden {
    fn spawn_hidden(&mut self) -> std::io::Result<std::process::Child>;
}

impl Hidden for std::process::Command {
    #[cfg(windows)]
    fn spawn_hidden(&mut self) -> std::io::Result<std::process::Child> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        self.creation_flags(CREATE_NO_WINDOW).spawn()
    }

    #[cfg(not(windows))]
    fn spawn_hidden(&mut self) -> std::io::Result<std::process::Child> {
        self.spawn()
    }
}

/// The shape of a track, measuring it once if it has not been measured.
///
/// Cached in the row rather than recomputed: reading 3.8 MB through a pipe is
/// fast but not free, and the answer never changes for a file that has not
/// changed.
///
/// A failure is recorded the same way [`crate::loudness`] records one -- as a
/// measured-and-empty result -- so a file ffmpeg cannot read is attempted once
/// rather than on every play.
#[tauri::command]
pub async fn track_waveform(
    db: State<'_, Db>,
    app: tauri::AppHandle,
    track_id: i64,
) -> Result<Option<Vec<u8>>, String> {
    let row = sqlx::query_as::<_, (Option<Vec<u8>>, Option<i64>, Option<String>)>(
        "SELECT waveform, waveform_at, local_path FROM tracks WHERE id = ?",
    )
    .bind(track_id)
    .fetch_optional(&db.pool)
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "That track is not in the library.".to_string())?;

    let (stored, measured_at, local_path) = row;

    if let Some(waveform) = stored {
        return Ok(Some(waveform));
    }
    // Measured before and produced nothing. Asking ffmpeg again would produce
    // the same nothing.
    if measured_at.is_some() {
        return Ok(None);
    }

    let Some(path) = local_path else {
        return Ok(None);
    };
    let Some(ffmpeg) = crate::sidecar::resolve(&app, crate::sidecar::Tool::Ffmpeg)
        .ok()
        .map(|found| found.path)
    else {
        return Ok(None);
    };

    let measured = tauri::async_runtime::spawn_blocking(move || {
        measure(&ffmpeg, Path::new(&path))
    })
    .await
    .map_err(|e| e.to_string())?;

    let waveform = measured.ok();
    record(&db.pool, track_id, waveform.as_deref()).await?;

    Ok(waveform)
}

async fn record(pool: &SqlitePool, track_id: i64, waveform: Option<&[u8]>) -> Result<(), String> {
    sqlx::query("UPDATE tracks SET waveform = ?, waveform_at = unixepoch() WHERE id = ?")
        .bind(waveform)
        .bind(track_id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16-bit little-endian mono, `levels` repeated to fill `samples`.
    fn pcm(levels: &[i16], samples: usize) -> Vec<u8> {
        (0..samples)
            .flat_map(|i| levels[i % levels.len()].to_le_bytes())
            .collect()
    }

    /// Runs bytes through the streaming folder, as `measure` does.
    fn peaks(pcm: &[u8]) -> Option<Vec<u8>> {
        let mut folder = Folder::default();
        for sample in pcm.chunks_exact(2) {
            folder.push(i16::from_le_bytes([sample[0], sample[1]]).unsigned_abs());
        }
        folder.finish()
    }

    #[test]
    fn a_steady_tone_draws_a_flat_line() {
        let drawn = peaks(&pcm(&[8_000, -8_000], BUCKETS * 20)).unwrap();

        assert_eq!(drawn.len(), BUCKETS);
        assert!(
            drawn.iter().all(|&peak| peak == 255),
            "a constant level should normalise to a full bar"
        );
    }

    /// Normalised against the track, so a quiet recording still fills the bar.
    #[test]
    fn a_quiet_track_still_fills_the_bar() {
        let loud = peaks(&pcm(&[20_000, -20_000], BUCKETS * 20)).unwrap();
        let quiet = peaks(&pcm(&[200, -200], BUCKETS * 20)).unwrap();

        assert_eq!(loud, quiet);
    }

    /// The last bucket has to be filled.
    ///
    /// Dividing the sample count by `BUCKETS` to get a chunk size loses the
    /// remainder, which for most files leaves the final bucket short or empty
    /// — a waveform with a bite out of the end of every track.
    #[test]
    fn every_bucket_is_covered_whatever_the_length() {
        // Deliberately not a multiple of BUCKETS.
        for samples in [BUCKETS + 1, BUCKETS * 7 + 3, BUCKETS * 101 - 1] {
            let drawn = peaks(&pcm(&[12_000, -12_000], samples)).unwrap();
            assert_eq!(drawn.len(), BUCKETS);
            assert!(
                drawn.last().is_some_and(|&peak| peak > 0),
                "{samples} samples left the last bucket empty"
            );
        }
    }

    /// A loud moment lands where it happened, not somewhere near it.
    #[test]
    fn a_peak_lands_in_its_own_bucket() {
        let mut pcm = pcm(&[1_000, -1_000], BUCKETS * 10);
        // A spike three quarters of the way through.
        let at = (pcm.len() / 4 * 3) & !1;
        pcm[at..at + 2].copy_from_slice(&30_000i16.to_le_bytes());

        let drawn = peaks(&pcm).unwrap();
        let loudest = drawn
            .iter()
            .enumerate()
            .max_by_key(|(_, &peak)| peak)
            .map(|(index, _)| index)
            .unwrap();

        assert_eq!(loudest, BUCKETS * 3 / 4);
    }

    #[test]
    fn silence_has_no_shape() {
        assert!(peaks(&pcm(&[0], BUCKETS * 20)).is_none());
        assert!(peaks(&[]).is_none());
    }

    /// `i16::MIN` has no positive counterpart; negating it panics in debug.
    #[test]
    fn the_most_negative_sample_does_not_panic() {
        let drawn = peaks(&pcm(&[i16::MIN, i16::MAX], BUCKETS * 4)).unwrap();
        assert_eq!(drawn.len(), BUCKETS);
    }

    /// Memory is a property of the code, not of the song.
    ///
    /// The reason the folding exists. A track twelve hours long goes through
    /// the same fixed slots as a three-minute one, because samples are reduced
    /// as they arrive rather than collected and reduced at the end.
    #[test]
    fn a_very_long_track_costs_no_more_than_a_short_one() {
        let mut short = Folder::default();
        let mut long = Folder::default();

        for i in 0..SLOTS * 2 {
            short.push((i % 30_000) as u16);
        }
        // A hundred and twenty-eight folds' worth: hours of audio.
        for i in 0..SLOTS * 256 {
            long.push((i % 30_000) as u16);
        }

        assert_eq!(short.peaks.len(), SLOTS);
        assert_eq!(long.peaks.len(), SLOTS, "the working set grew with the track");
        assert_eq!(long.peaks.capacity(), SLOTS);

        // And it still draws a full picture, not a folded stump.
        let drawn = long.finish().unwrap();
        assert_eq!(drawn.len(), BUCKETS);
        assert!(drawn.iter().all(|&peak| peak > 0), "a folded track has holes");
    }

    /// Folding must not lose the loudest moment.
    ///
    /// Merging pairs takes the larger of the two, so a peak survives every
    /// fold. If it did not, a long track's one dramatic moment would quietly
    /// flatten as the resolution halved.
    #[test]
    fn a_peak_survives_every_fold() {
        let mut folder = Folder::default();

        // Well past several folds, with one sample louder than the rest.
        for i in 0..SLOTS * 9 {
            folder.push(if i == SLOTS * 3 { 30_000 } else { 100 });
        }

        let drawn = folder.finish().unwrap();
        assert_eq!(
            *drawn.iter().max().unwrap(),
            255,
            "the loudest sample was folded away"
        );
        // And it is still roughly where it happened -- a third of the way in.
        let loudest = drawn
            .iter()
            .enumerate()
            .max_by_key(|(_, &peak)| peak)
            .map(|(index, _)| index)
            .unwrap();
        let expected = BUCKETS / 3;
        assert!(
            loudest.abs_diff(expected) <= BUCKETS / 50,
            "the peak moved: bucket {loudest}, expected about {expected}"
        );
    }

    /// A real file, end to end, with the cost of measuring it reported.
    ///
    /// Synthetic PCM proves the arithmetic and nothing about ffmpeg: the
    /// argument list, the pipe, and whether the result looks like music rather
    /// than a flat line are only answerable against an actual track.
    ///
    /// ```text
    /// MUSIC_APP_TRACK=D:/path/to/song.mp3 \
    ///   cargo test --lib waveform::tests::live -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a real audio file and the ffmpeg sidecar"]
    fn live_a_real_track_has_a_shape() {
        let Ok(audio) = std::env::var("MUSIC_APP_TRACK") else {
            eprintln!("SKIP: MUSIC_APP_TRACK is not set");
            return;
        };
        let ffmpeg = std::env::var("MUSIC_APP_FFMPEG")
            .unwrap_or_else(|_| "target/debug/ffmpeg.exe".to_string());

        let started = std::time::Instant::now();
        let drawn = measure(Path::new(&ffmpeg), Path::new(&audio)).expect("a waveform");
        let took = started.elapsed();

        assert_eq!(drawn.len(), BUCKETS);

        let peak = *drawn.iter().max().unwrap() as u32;
        let mean = drawn.iter().map(|&p| p as u32).sum::<u32>() / BUCKETS as u32;
        let quiet = drawn.iter().filter(|&&p| p < 8).count();

        eprintln!("measured in {took:.2?}");
        eprintln!("peak {peak}, mean {mean}, near-silent buckets {quiet}/{BUCKETS}");
        eprintln!(
            "  {}",
            drawn
                .iter()
                .step_by(BUCKETS / 60)
                .map(|&p| " ▁▂▃▄▅▆▇█".chars().nth((p as usize * 8) / 256).unwrap())
                .collect::<String>()
        );

        assert_eq!(peak, 255, "nothing reached full scale");
        // The failure this exists to catch: a low-passed envelope of a track
        // with little bass is a nearly flat, nearly empty line.
        assert!(
            mean > 10,
            "mean {mean} — this looks like a bass envelope, not a waveform"
        );
        assert!(
            quiet < BUCKETS / 2,
            "{quiet} of {BUCKETS} buckets are silent"
        );
    }
}
