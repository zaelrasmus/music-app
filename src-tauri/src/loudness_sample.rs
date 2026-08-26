//! Measuring a track's loudness without downloading it.
//!
//! # Why this exists
//!
//! Levelling needs integrated loudness, and integrated loudness is defined over
//! a whole track. The obvious reading of that -- and the one the app shipped --
//! is that a stream must be fetched in full before it can be levelled, so a
//! track nobody has played either waits for a download or plays uncorrected and
//! is corrected from the next time.
//!
//! That reading is wrong, and it cost a redundant download. Playback already
//! streams the audio through ffmpeg; the bytes are arriving anyway. What a cold
//! stream lacks is not the audio but the *end* of it -- which only matters if
//! the measurement has to be exact before the first note.
//!
//! It does not. Two things measured against this library:
//!
//! - Sampling four slices of a tenth of the track each lands **within 1 LU on
//!   every track measured**, worst case 0.60 LU. That is a correction worth
//!   applying.
//! - Fetched in parallel while the song is already playing, those slices cost
//!   no latency at all -- one measured 0.35 s, and playback started 0.2 s in.
//!
//! So the correction arrives a second or two into a track that started
//! instantly, and is glided in rather than switched, which is inaudible. The
//! exact figure follows at the end of the track, from the cache copy the stream
//! already wrote for free, and is what every later play uses.
//!
//! # Why slices and not simply the opening
//!
//! Because openings lie. Measured across 42 tracks, the first ten seconds read
//! **5.05 LU quiet on average and 18 LU quiet at worst** -- intros, fade-ins and
//! sparse first verses. Trusting them would make the app over-boost and the
//! chorus arrive too loud, which is worse than not levelling at all. Spread the
//! same audio across the track and that bias disappears.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::loudness::Loudness;

/// Where to sample, as `(start, length)` fractions of the track.
///
/// Four slices of a tenth each. Measured against alternatives on this library:
/// three slices reached 98% of tracks within 1 LU, four reached 100%, and five
/// bought nothing more. The offsets avoid the first tenth, which is the intro,
/// and stop short of the last, which is usually a fade.
pub const PLAN: [(f64, f64); 4] = [(0.10, 0.10), (0.35, 0.10), (0.60, 0.10), (0.82, 0.10)];

/// Below this, sampling is pointless and the whole thing is measured instead.
///
/// The slices would overlap into one another and the fetch would cover most of
/// the track anyway, so the exact answer is available for the same cost as the
/// approximate one.
pub const MIN_SAMPLED_SECS: f64 = 90.0;

/// The rate slices are decoded at before being joined.
///
/// Stereo, deliberately. R128 sums channel energies, so measuring a downmix
/// reads about 1 LU quiet against the same track measured in stereo -- an error
/// that showed up as a suspiciously uniform bias across every sampling plan
/// tried, which is what gave it away.
const SAMPLE_RATE: u32 = 48_000;
const CHANNELS: u32 = 2;

/// Decodes one slice to raw interleaved f32.
///
/// `input` is whatever ffmpeg can open: a local path or a stream URL. For a URL
/// the `-ss` before `-i` is what makes this a range request rather than a
/// download-and-discard -- ffmpeg asks the server for the byte offset the
/// container index points at, so a slice from two thirds of the way in does not
/// pull the first two thirds with it.
fn decode_slice(ffmpeg: &Path, input: &str, start: f64, len: f64) -> Result<Vec<u8>, String> {
    let mut command = Command::new(ffmpeg);
    crate::sidecar::quiet(&mut command);
    let out = command
        .args(["-hide_banner", "-loglevel", "error"])
        .args(["-ss", &format!("{start:.3}")])
        .args(["-i", input])
        .args(["-t", &format!("{len:.3}")])
        .args(["-vn", "-f", "f32le", "-acodec", "pcm_f32le"])
        .args(["-ar", &SAMPLE_RATE.to_string()])
        .args(["-ac", &CHANNELS.to_string()])
        .arg("-")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;

    if !out.status.success() {
        return Err("ffmpeg could not read that part of the track".to_string());
    }
    Ok(out.stdout)
}

/// Measures raw interleaved f32 as one signal.
fn measure_pcm(ffmpeg: &Path, pcm: &[u8]) -> Result<Loudness, String> {
    use std::io::Write;

    if pcm.is_empty() {
        return Err("nothing was decoded".to_string());
    }

    let mut command = Command::new(ffmpeg);
    crate::sidecar::quiet(&mut command);
    let mut child = command
        .args(["-hide_banner", "-nostats"])
        .args(["-f", "f32le"])
        .args(["-ar", &SAMPLE_RATE.to_string()])
        .args(["-ac", &CHANNELS.to_string()])
        .args(["-i", "-"])
        .args(["-af", "ebur128=peak=true:framelog=quiet"])
        .args(["-f", "null", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;

    // Written from its own thread while ffmpeg is drained from this one.
    // Doing both here would deadlock on anything larger than the pipe buffer:
    // the write blocks waiting for ffmpeg to read, and ffmpeg blocks waiting
    // for its stderr to be read. Four tenths of a track as f32 is several
    // megabytes, comfortably past that.
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "ffmpeg would not take input".to_string())?;
    let owned = pcm.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&owned);
    });

    let out = child
        .wait_with_output()
        .map_err(|e| format!("could not read ffmpeg: {e}"))?;
    let _ = writer.join();

    crate::loudness::parse_summary(&String::from_utf8_lossy(&out.stderr))
        .ok_or_else(|| "ffmpeg reported no loudness".to_string())
}

/// Measures a track by sampling it, without fetching the whole thing.
///
/// Blocking: every slice is an ffmpeg process. Call it off the async runtime.
///
/// The slices are fetched **in parallel**, which is the whole reason this is
/// fast enough to matter. Each one is dominated by fixed setup -- connection
/// and container header, about 0.35 s, near enough the same for a four-minute
/// track as a six-minute one -- so four in sequence would cost four times that
/// while four at once cost roughly one.
///
/// Returns `Err` if any slice fails, rather than measuring the rest: a plan
/// that silently dropped a slice would be a different plan, with accuracy
/// nobody has measured.
pub fn sample(ffmpeg: &Path, input: &str, duration_secs: f64) -> Result<Loudness, String> {
    if !duration_secs.is_finite() || duration_secs <= 0.0 {
        return Err("that track has no usable duration".to_string());
    }

    let handles: Vec<_> = PLAN
        .iter()
        .map(|(start, len)| {
            let ffmpeg = ffmpeg.to_path_buf();
            let input = input.to_string();
            let (start, len) = (start * duration_secs, len * duration_secs);
            std::thread::spawn(move || decode_slice(&ffmpeg, &input, start, len))
        })
        .collect();

    let mut joined = Vec::new();
    for handle in handles {
        let slice = handle
            .join()
            .map_err(|_| "a slice fetch panicked".to_string())??;
        joined.extend_from_slice(&slice);
    }

    measure_pcm(ffmpeg, &joined)
}

/// Whether a track is worth sampling rather than measuring outright.
pub fn worth_sampling(duration_secs: f64) -> bool {
    duration_secs.is_finite() && duration_secs >= MIN_SAMPLED_SECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Instant;

    pub(super) fn ffmpeg() -> Option<PathBuf> {
        crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg)
    }

    fn cache_dir() -> PathBuf {
        PathBuf::from(std::env::var("APPDATA").unwrap_or_default())
            .join("com.kiza2.music-app")
            .join("cache")
            .join("audio")
    }

    fn cached_tracks() -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(cache_dir()) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "mka"))
            .collect();
        files.sort();
        files
    }


    /// Duration of a remote stream, from its container header.
    pub(super) fn remote_duration(ffmpeg: &Path, url: &str) -> Option<f64> {
        let mut command = Command::new(ffmpeg);
        crate::sidecar::quiet(&mut command);
        let out = command
            .args(["-hide_banner", "-nostats"])
            .args(["-i", url])
            .args(["-t", "0.01", "-f", "null", "-"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .output()
            .ok()?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        let after = stderr.split("Duration:").nth(1)?;
        let stamp = after.split(',').next()?.trim();
        let mut parts = stamp.split(':');
        let h: f64 = parts.next()?.trim().parse().ok()?;
        let m: f64 = parts.next()?.trim().parse().ok()?;
        let s: f64 = parts.next()?.trim().parse().ok()?;
        Some(h * 3600.0 + m * 60.0 + s)
    }

    /// Measures a remote stream in full: what the app does today.
    pub(super) fn measure_remote(ffmpeg: &Path, url: &str) -> Result<Loudness, String> {
        let mut command = Command::new(ffmpeg);
        crate::sidecar::quiet(&mut command);
        let out = command
            .args(["-hide_banner", "-nostats"])
            .args(["-i", url])
            .args(["-af", "ebur128=peak=true:framelog=quiet"])
            .args(["-f", "null", "-"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("{e}"))?;
        crate::loudness::parse_summary(&String::from_utf8_lossy(&out.stderr))
            .ok_or_else(|| "no loudness".to_string())
    }
    fn duration_of(ffmpeg: &Path, file: &Path) -> Option<f64> {
        let mut command = Command::new(ffmpeg);
        crate::sidecar::quiet(&mut command);
        let out = command
            .args(["-hide_banner", "-nostats"])
            .arg("-i")
            .arg(file)
            .args(["-f", "null", "-"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .output()
            .ok()?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        let after = stderr.split("Duration:").nth(1)?;
        let stamp = after.split(',').next()?.trim();
        let mut parts = stamp.split(':');
        let h: f64 = parts.next()?.trim().parse().ok()?;
        let m: f64 = parts.next()?.trim().parse().ok()?;
        let s: f64 = parts.next()?.trim().parse().ok()?;
        Some(h * 3600.0 + m * 60.0 + s)
    }

    // ---- the claim this whole module rests on ------------------------------

    /// Every track in the library, sampled against measured-in-full.
    ///
    /// The correction is only worth applying if it agrees with the truth. One
    /// LU is the bar: this library spans about 10 dB between its quietest and
    /// loudest track, so an error of 1 LU leaves 90% of the problem solved,
    /// while an error of 3 would leave the levelling audibly wrong in the
    /// other direction.
    ///
    /// Grouped by duration, because the plan is proportional and a short track
    /// gives each slice less to work with.
    #[test]
    #[ignore = "measurement against the real library"]
    fn sampling_agrees_with_measuring_in_full() {
        let Some(ffmpeg) = ffmpeg() else {
            eprintln!("SKIP: no staged ffmpeg");
            return;
        };
        let files = cached_tracks();
        if files.is_empty() {
            eprintln!("SKIP: audio cache is empty");
            return;
        }

        let buckets: [(&str, f64, f64); 4] = [
            ("1:30-2:30", 90.0, 150.0),
            ("2:30-3:30", 150.0, 210.0),
            ("3:30-5:00", 210.0, 300.0),
            ("over 5:00", 300.0, f64::MAX),
        ];
        let mut rows: Vec<(f64, f64)> = Vec::new();
        let mut skipped_short = 0usize;

        eprintln!(
            "\n{:<24} {:>7} {:>9} {:>9} {:>8}",
            "track", "secs", "full", "sampled", "error",
        );

        for file in &files {
            let Some(duration) = duration_of(&ffmpeg, file) else {
                continue;
            };
            if !worth_sampling(duration) {
                skipped_short += 1;
                continue;
            }
            let Ok(full) = crate::loudness::measure(&ffmpeg, file) else {
                continue;
            };
            let path = file.to_string_lossy().to_string();
            let Ok(sampled) = sample(&ffmpeg, &path, duration) else {
                eprintln!("{:<24} sampling failed", "");
                continue;
            };

            let error = (sampled.lufs - full.lufs) as f64;
            let name: String = file
                .file_stem()
                .map(|s| s.to_string_lossy().chars().take(22).collect())
                .unwrap_or_default();
            eprintln!(
                "{name:<24} {duration:>7.0} {:>9.1} {:>9.1} {error:>8.2}",
                full.lufs, sampled.lufs,
            );
            rows.push((duration, error));
        }

        assert!(!rows.is_empty(), "no track could be compared");

        eprintln!(
            "\n{:<12} {:>8} {:>9} {:>8} {:>9} {:>9}",
            "duration", "mean", "typical", "worst", "within1", "within2",
        );
        for (label, lo, hi) in buckets {
            let errs: Vec<f64> = rows
                .iter()
                .filter(|(d, _)| *d >= lo && *d < hi)
                .map(|(_, e)| *e)
                .collect();
            if errs.is_empty() {
                continue;
            }
            let n = errs.len() as f64;
            let mean = errs.iter().sum::<f64>() / n;
            let typical = errs.iter().map(|x| x.abs()).sum::<f64>() / n;
            let worst = errs.iter().cloned().fold(0.0f64, |m, x| m.max(x.abs()));
            let within1 = 100.0 * errs.iter().filter(|x| x.abs() <= 1.0).count() as f64 / n;
            let within2 = 100.0 * errs.iter().filter(|x| x.abs() <= 2.0).count() as f64 / n;
            eprintln!(
                "{label:<12} {mean:>8.2} {typical:>9.2} {worst:>8.2} {within1:>8.0}% {within2:>8.0}%   ({} tracks)",
                errs.len(),
            );
        }
        eprintln!("\n{skipped_short} tracks were under 1:30 and would be measured in full.");

        // The bar, asserted rather than eyeballed.
        let worst = rows.iter().map(|(_, e)| e.abs()).fold(0.0f64, f64::max);
        assert!(
            worst <= 2.0,
            "a sampled measurement was {worst:.2} LU out, which is worse than \
             not levelling that track at all",
        );
    }

    /// The gain that is actually applied, which is what a listener hears.
    ///
    /// Loudness error and *gain* error are not the same number: the gain is
    /// clamped so a quiet track cannot be boosted past the headroom its peaks
    /// leave. Two tracks can differ by 2 LU and be played at identical gain, or
    /// agree closely and diverge once the ceiling bites. This checks the number
    /// that reaches the volume stage.
    #[test]
    #[ignore = "measurement against the real library"]
    fn the_gain_applied_is_the_same_either_way() {
        let Some(ffmpeg) = ffmpeg() else {
            eprintln!("SKIP: no staged ffmpeg");
            return;
        };
        let files = cached_tracks();
        if files.is_empty() {
            eprintln!("SKIP: audio cache is empty");
            return;
        }

        let mut diffs: Vec<f64> = Vec::new();
        eprintln!("\n{:<24} {:>10} {:>10} {:>8}", "track", "gain full", "gain smp", "diff");

        for file in &files {
            let Some(duration) = duration_of(&ffmpeg, file) else {
                continue;
            };
            if !worth_sampling(duration) {
                continue;
            }
            let (Ok(full), path) = (
                crate::loudness::measure(&ffmpeg, file),
                file.to_string_lossy().to_string(),
            ) else {
                continue;
            };
            let Ok(sampled) = sample(&ffmpeg, &path, duration) else {
                continue;
            };

            let g_full = crate::loudness::gain_db(full, crate::loudness::TARGET_LUFS);
            let g_sampled = crate::loudness::gain_db(sampled, crate::loudness::TARGET_LUFS);
            let diff = (g_sampled - g_full) as f64;
            diffs.push(diff);

            let name: String = file
                .file_stem()
                .map(|s| s.to_string_lossy().chars().take(22).collect())
                .unwrap_or_default();
            eprintln!("{name:<24} {g_full:>10.2} {g_sampled:>10.2} {diff:>8.2}");
        }

        assert!(!diffs.is_empty(), "no track could be compared");
        let n = diffs.len() as f64;
        let typical = diffs.iter().map(|x| x.abs()).sum::<f64>() / n;
        let worst = diffs.iter().cloned().fold(0.0f64, |m, x| m.max(x.abs()));
        let within1 = 100.0 * diffs.iter().filter(|x| x.abs() <= 1.0).count() as f64 / n;
        eprintln!(
            "\ngain difference: typical {typical:.2} dB, worst {worst:.2} dB, \
             within 1 dB on {within1:.0}% of {} tracks",
            diffs.len(),
        );

        assert!(
            worst <= 2.0,
            "the gain applied differed by {worst:.2} dB between sampling and \
             measuring in full",
        );
    }

    // ---- edge cases --------------------------------------------------------

    /// A duration the caller could not determine must not be guessed at.
    #[test]
    fn a_track_with_no_duration_is_refused() {
        let Some(ffmpeg) = ffmpeg() else {
            return;
        };
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(
                sample(&ffmpeg, "irrelevant", bad).is_err(),
                "a duration of {bad} should have been refused",
            );
        }
    }

    /// Short tracks take the exact route, not the approximate one.
    #[test]
    fn short_tracks_are_measured_in_full() {
        assert!(!worth_sampling(0.0));
        assert!(!worth_sampling(30.0));
        assert!(!worth_sampling(89.0));
        assert!(worth_sampling(90.0));
        assert!(worth_sampling(600.0));
        // Never sampled on a number that cannot be reasoned about.
        assert!(!worth_sampling(f64::NAN));
        assert!(!worth_sampling(f64::INFINITY));
    }

    /// Nothing readable at that path is an error, not a wrong answer.
    ///
    /// The failure mode that matters: returning some plausible number for a
    /// track it never read would apply a gain nobody measured.
    #[test]
    fn an_unreadable_input_fails_rather_than_inventing_a_number() {
        let Some(ffmpeg) = ffmpeg() else {
            return;
        };
        let missing = std::env::temp_dir().join("music-app-no-such-track.mka");
        let _ = std::fs::remove_file(&missing);

        let result = sample(&ffmpeg, &missing.to_string_lossy(), 240.0);
        assert!(result.is_err(), "a missing file returned {result:?}");
    }

    /// Silence has no meaningful loudness and must not produce a gain.
    #[test]
    fn silence_is_not_given_a_loudness() {
        let Some(ffmpeg) = ffmpeg() else {
            return;
        };
        let dir = std::env::temp_dir().join("music-app-sample-silence");
        std::fs::create_dir_all(&dir).unwrap();
        let quiet = dir.join("silence.wav");

        let mut make = Command::new(&ffmpeg);
        crate::sidecar::quiet(&mut make);
        let made = make
            .args(["-hide_banner", "-nostats", "-y"])
            .args(["-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo", "-t", "120"])
            .arg(&quiet)
            .status()
            .expect("ffmpeg should run");
        assert!(made.success());

        let result = sample(&ffmpeg, &quiet.to_string_lossy(), 120.0);
        // -inf is not a number a gain can be computed from, and `parse_summary`
        // is what rejects it. Either an error or a non-finite reading is
        // correct; a finite one would be a fabrication.
        if let Ok(loudness) = result {
            assert!(
                !loudness.lufs.is_finite() || loudness.lufs < -60.0,
                "silence measured {} LUFS, which would produce a gain",
                loudness.lufs,
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Sampling must be repeatable: the same track, the same answer.
    ///
    /// Four slices are fetched on four threads and joined in order. If the
    /// join ever raced, the concatenation would differ between runs and so
    /// would the loudness -- a track whose level changed slightly every play.
    #[test]
    #[ignore = "measurement against the real library"]
    fn sampling_the_same_track_twice_gives_the_same_answer() {
        let Some(ffmpeg) = ffmpeg() else {
            return;
        };
        let files = cached_tracks();
        let Some(file) = files.iter().find(|f| {
            duration_of(&ffmpeg, f).is_some_and(worth_sampling)
        }) else {
            eprintln!("SKIP: no long enough track cached");
            return;
        };
        let duration = duration_of(&ffmpeg, file).unwrap();
        let path = file.to_string_lossy().to_string();

        let first = sample(&ffmpeg, &path, duration).expect("first run");
        let second = sample(&ffmpeg, &path, duration).expect("second run");

        assert_eq!(
            first.lufs, second.lufs,
            "the same track sampled twice measured {} then {} -- the slice \
             order is not deterministic",
            first.lufs, second.lufs,
        );
    }

    /// Parallel slices must actually be parallel.
    ///
    /// The whole latency argument rests on it: four slices at 0.35 s each cost
    /// 1.4 s in sequence and about 0.4 s at once. If the threads were
    /// serialising, the correction would arrive late enough to be heard
    /// landing.
    #[test]
    #[ignore = "measurement against the real library"]
    fn slices_are_fetched_in_parallel() {
        let Some(ffmpeg) = ffmpeg() else {
            return;
        };
        let files = cached_tracks();
        let Some(file) = files.iter().find(|f| {
            duration_of(&ffmpeg, f).is_some_and(worth_sampling)
        }) else {
            eprintln!("SKIP: no long enough track cached");
            return;
        };
        let duration = duration_of(&ffmpeg, file).unwrap();
        let path = file.to_string_lossy().to_string();

        // The fetch phase only, both ways.
        //
        // An earlier version compared four parallel slices *plus the
        // measurement pass* against four sequential slices without it. That is
        // not the same work, and it made parallelism look absent when it was
        // only being outweighed by a stage the other side never ran.
        let started = Instant::now();
        for (start, len) in PLAN {
            let _ = decode_slice(&ffmpeg, &path, start * duration, len * duration);
        }
        let sequential = started.elapsed().as_secs_f64();

        let started = Instant::now();
        let handles: Vec<_> = PLAN
            .iter()
            .map(|(start, len)| {
                let ffmpeg = ffmpeg.clone();
                let path = path.clone();
                let (start, len) = (start * duration, len * duration);
                std::thread::spawn(move || decode_slice(&ffmpeg, &path, start, len))
            })
            .collect();
        for handle in handles {
            let _ = handle.join();
        }
        let parallel = started.elapsed().as_secs_f64();

        eprintln!(
            "\nfour slices: {sequential:.3}s in sequence, {parallel:.3}s in parallel ({:.1}x)",
            sequential / parallel.max(1e-6),
        );
        assert!(
            parallel < sequential,
            "four slices took {parallel:.3}s in parallel against {sequential:.3}s \
             in sequence -- they are not actually running at the same time",
        );
    }
}

#[cfg(test)]
mod network_tests {
    use super::tests::{ffmpeg, remote_duration, measure_remote};
    use super::*;
    use std::time::Instant;

    /// The whole strategy, against real streams, end to end.
    ///
    /// Three questions at once, because they only mean anything together:
    ///
    /// - **Does it agree?** Sampled against the same stream measured in full.
    ///   The parity run on local files says yes; this is the same question
    ///   where the bytes arrive over a network and ffmpeg is seeking a remote
    ///   container rather than a file it can index freely.
    /// - **When does the correction land?** Measured from the moment the
    ///   sampling starts, which is the moment playback starts -- they run
    ///   together. Anything under a couple of seconds is inaudible as a
    ///   correction arriving, given it is glided in over 30 ms.
    /// - **What did it cost?** Against fetching the whole track, which is what
    ///   the app does today.
    #[test]
    #[ignore = "hits the network"]
    fn sampling_a_real_stream_agrees_and_lands_quickly() {
        let Some(ffmpeg) = ffmpeg() else {
            eprintln!("SKIP: no staged ffmpeg");
            return;
        };
        let Some(yt_dlp) = crate::sidecar::staged_for_tests(crate::sidecar::Tool::YtDlp) else {
            eprintln!("SKIP: no staged yt-dlp");
            return;
        };

        // Deliberately spread over length: a short single, an album track and
        // something long enough that the slices are far apart.
        let pages = [
            "https://www.youtube.com/watch?v=HJRz4pROLxE",
            "https://www.youtube.com/watch?v=MlcJQYON2Go",
            "https://www.youtube.com/watch?v=N5Q8EeyOiKY",
            "https://www.youtube.com/watch?v=kM0Fpbz0W8U",
        ];

        eprintln!(
            "\n{:<14} {:>7} {:>9} {:>9} {:>7} {:>10} {:>10}",
            "track", "secs", "full", "sampled", "error", "sample in", "full in",
        );

        let mut errors: Vec<f64> = Vec::new();
        let mut sample_times: Vec<f64> = Vec::new();

        for page in pages {
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            let Ok(resolved) = runtime.block_on(crate::youtube::resolve_stream_url(
                &yt_dlp,
                page,
                crate::stream_urls::Encoding::Preferred,
            )) else {
                eprintln!("{:<14} could not resolve", &page[32..]);
                continue;
            };
            let url = resolved.url;

            let Some(duration) = remote_duration(&ffmpeg, &url) else {
                eprintln!("{:<14} no duration", &page[32..]);
                continue;
            };
            if !worth_sampling(duration) {
                eprintln!("{:<14} {duration:>7.0}  too short to sample", &page[32..]);
                continue;
            }

            // The strategy, timed from the moment it starts.
            let started = Instant::now();
            let sampled = sample(&ffmpeg, &url, duration);
            let sample_secs = started.elapsed().as_secs_f64();

            // What the app does today, for comparison.
            let started = Instant::now();
            let full = measure_remote(&ffmpeg, &url);
            let full_secs = started.elapsed().as_secs_f64();

            match (&sampled, &full) {
                (Ok(s), Ok(f)) => {
                    let error = (s.lufs - f.lufs) as f64;
                    errors.push(error);
                    sample_times.push(sample_secs);
                    eprintln!(
                        "{:<14} {duration:>7.0} {:>9.1} {:>9.1} {error:>7.2} {sample_secs:>9.2}s {full_secs:>9.2}s",
                        &page[32..],
                        f.lufs,
                        s.lufs,
                    );
                }
                _ => eprintln!("{:<14} measurement failed", &page[32..]),
            }
        }

        if errors.is_empty() {
            eprintln!("SKIP: nothing could be measured -- network or yt-dlp");
            return;
        }

        let n = errors.len() as f64;
        let typical = errors.iter().map(|x| x.abs()).sum::<f64>() / n;
        let worst = errors.iter().cloned().fold(0.0f64, |m, x| m.max(x.abs()));
        let slowest = sample_times.iter().cloned().fold(0.0f64, f64::max);
        eprintln!(
            "\ntypical error {typical:.2} LU, worst {worst:.2} LU over {} streams.\n\
             Slowest correction landed {slowest:.2}s after playback began.",
            errors.len(),
        );

        assert!(
            worst <= 2.0,
            "a sampled stream was {worst:.2} LU out, which is worse than leaving \
             that track alone",
        );
    }

    /// What the strategy costs on a slower connection.
    ///
    /// The timings above are one machine on one link, and a link this session
    /// has no way to make worse -- shaping traffic needs a driver, not a test.
    /// What *can* be reasoned about is the shape: each slice is dominated by
    /// fixed setup, and they run at the same time, so the correction lands
    /// roughly one slice-time after playback starts however slow the link is.
    ///
    /// This measures the per-slice cost so that scaling is grounded in a real
    /// number rather than assumed, and states plainly what has not been tested.
    #[test]
    #[ignore = "hits the network"]
    fn what_one_slice_costs_over_the_network() {
        let Some(ffmpeg) = ffmpeg() else {
            return;
        };
        let Some(yt_dlp) = crate::sidecar::staged_for_tests(crate::sidecar::Tool::YtDlp) else {
            return;
        };

        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let Ok(resolved) = runtime.block_on(crate::youtube::resolve_stream_url(
            &yt_dlp,
            "https://www.youtube.com/watch?v=N5Q8EeyOiKY",
            crate::stream_urls::Encoding::Preferred,
        )) else {
            eprintln!("SKIP: could not resolve");
            return;
        };
        let url = resolved.url;
        let Some(duration) = remote_duration(&ffmpeg, &url) else {
            return;
        };

        eprintln!("\nper-slice cost over the network, five runs:");
        let mut times = Vec::new();
        for (start, len) in PLAN {
            let began = Instant::now();
            let got = decode_slice(&ffmpeg, &url, start * duration, len * duration);
            let secs = began.elapsed().as_secs_f64();
            eprintln!(
                "  slice at {:>5.0}s: {secs:>6.2}s  {}",
                start * duration,
                if got.is_ok() { "ok" } else { "failed" },
            );
            times.push(secs);
        }

        let slowest = times.iter().cloned().fold(0.0f64, f64::max);
        eprintln!(
            "\nSlowest slice {slowest:.2}s. Running in parallel, the correction lands\n\
             about that long after playback starts, not four times it.\n\
             NOT TESTED: a genuinely slow or lossy link. This machine cannot shape\n\
             its own traffic, so these numbers are one connection on one day.",
        );
    }
}
