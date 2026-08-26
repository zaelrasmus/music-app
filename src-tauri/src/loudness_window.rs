//! Can a short window stand in for the whole track's loudness?
//!
//! Run with:
//! `cargo test --release --lib loudness_window -- --ignored --nocapture`
//!
//! # The problem this is testing a fix for
//!
//! Levelling a track needs its integrated loudness, and integrated loudness is
//! defined over the whole programme. For a file that is free -- it is already
//! on disk. For a stream nobody has played before it is not: the whole thing
//! has to arrive before a single number can be computed, which is why a cold
//! stream currently either plays uncorrected or waits.
//!
//! The proposal is to measure only the first few seconds and start immediately.
//! Whether that works is entirely a question about *this* library: if the
//! opening of a track predicts the whole, the idea is sound; if openings are
//! systematically quieter than the body -- intros, fade-ins, a sparse first
//! verse -- then the app would boost the track and the chorus would arrive too
//! loud.
//!
//! That is not a question anyone should answer from intuition, so this measures
//! it against the real audio cache.
//!
//! # What is reported
//!
//! For each track, the integrated loudness of the whole excerpt, then the error
//! a shorter window would have made. Error in LU (= dB): positive means the
//! window measured *louder* than the truth, which would make the app under-
//! correct; negative means it measured quieter, and the app would over-boost.
//!
//! A correction is worth having if it lands within about ±1 LU. Beyond ±2 the
//! levelling is doing more harm than leaving the track alone.
//!
//! # What was found
//!
//! **The opening is the worst place to look.** The first ten seconds measured
//! 5.05 LU quiet on average and 18 LU quiet at worst -- intros, fade-ins and
//! sparse first verses, all systematically below the body of the track. An app
//! trusting it would over-boost and the chorus would arrive too loud.
//!
//! **Sampling the whole track fixes it.** Four slices of a tenth of the track
//! each, at 10%, 35%, 60% and 82%, measured as one concatenated signal:
//!
//! ```text
//! plan        mean  typical  worst  within 1 LU  fetches
//! one 60%     0.17     0.24   0.80        100%      60%
//! 3 x 10%     0.16     0.35   1.10         98%      30%
//! 4 x 10%     0.09     0.20   0.60        100%      40%
//! ```
//!
//! **But the speed argument for it does not hold.** Timed against real YouTube
//! streams, the whole-track measurement it was meant to avoid costs only 3 to 7
//! seconds, and sampling 40%% of the track costs 2.3 to 4.3 -- against a floor of
//! 0.12 to 0.31 seconds for starting with no correction at all. Four range
//! requests pay four lots of connection and container setup, and 40%% of the
//! bytes is still 40%% of the bytes.
//!
//! So the accuracy question has a good answer and the latency question does
//! not. Sampling roughly halves a wait that was never the dominant cost.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    /// Integrated loudness of a slice of a file, via ffmpeg's `ebur128`.
    ///
    /// `start` and `len` in seconds; `None` for `len` means to the end.
    fn lufs(ffmpeg: &Path, file: &Path, start: u32, len: Option<u32>) -> Option<f64> {
        let mut command = Command::new(ffmpeg);
        crate::sidecar::quiet(&mut command);
        command.args(["-hide_banner", "-nostats"]);
        if start > 0 {
            command.args(["-ss", &start.to_string()]);
        }
        command.arg("-i").arg(file);
        if let Some(len) = len {
            command.args(["-t", &len.to_string()]);
        }
        let out = command
            .args(["-af", "ebur128=peak=true:framelog=quiet"])
            .args(["-f", "null", "-"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .output()
            .ok()?;

        let stderr = String::from_utf8_lossy(&out.stderr);
        // The Summary block reports "I: -14.2 LUFS".
        let summary = stderr.split("Summary:").nth(1)?;
        for line in summary.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("I:") {
                let value: f64 = rest.split_whitespace().next()?.parse().ok()?;
                return value.is_finite().then_some(value);
            }
        }
        None
    }

    fn cache_dir() -> PathBuf {
        PathBuf::from(std::env::var("APPDATA").unwrap_or_default())
            .join("com.kiza2.music-app")
            .join("cache")
            .join("audio")
    }

    #[test]
    #[ignore = "measurement"]
    fn does_a_short_window_predict_the_whole_track() {
        let Some(ffmpeg) = crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg) else {
            eprintln!("SKIP: no staged ffmpeg");
            return;
        };
        let Ok(entries) = std::fs::read_dir(cache_dir()) else {
            eprintln!("SKIP: no audio cache");
            return;
        };

        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "mka"))
            .collect();
        files.sort();

        eprintln!(
            "\n{:<24} {:>7}   {:>7} {:>7} {:>7}   {:>7} {:>7}",
            "track", "whole", "0-10s", "0-20s", "0-30s", "@30s", "@60s",
        );
        eprintln!(
            "{:<24} {:>7}   {:>7} {:>7} {:>7}   {:>7} {:>7}",
            "", "LUFS", "err", "err", "err", "err", "err",
        );

        let mut errors: Vec<Vec<f64>> = vec![Vec::new(); 5];

        for file in &files {
            let Some(whole) = lufs(&ffmpeg, file, 0, None) else {
                continue;
            };

            let windows = [
                lufs(&ffmpeg, file, 0, Some(10)),
                lufs(&ffmpeg, file, 0, Some(20)),
                lufs(&ffmpeg, file, 0, Some(30)),
                lufs(&ffmpeg, file, 30, Some(10)),
                lufs(&ffmpeg, file, 60, Some(10)),
            ];

            let name: String = file
                .file_stem()
                .map(|s| s.to_string_lossy().chars().take(22).collect())
                .unwrap_or_default();

            let mut cells = String::new();
            for (i, w) in windows.iter().enumerate() {
                match w {
                    Some(v) => {
                        let err = v - whole;
                        errors[i].push(err);
                        cells.push_str(&format!("{err:>8.2}"));
                    }
                    None => cells.push_str(&format!("{:>8}", "--")),
                }
                if i == 2 {
                    cells.push_str("  ");
                }
            }

            eprintln!("{name:<24} {whole:>7.1}   {cells}");
        }

        eprintln!("\n{:-<74}", "");
        let labels = ["0-10s", "0-20s", "0-30s", "@30s", "@60s"];
        eprintln!(
            "{:<10} {:>8} {:>8} {:>8} {:>8} {:>8}",
            "window", "mean", "mean|e|", "worst", "within1", "within2",
        );
        for (i, label) in labels.iter().enumerate() {
            let e = &errors[i];
            if e.is_empty() {
                continue;
            }
            let n = e.len() as f64;
            let mean = e.iter().sum::<f64>() / n;
            let mean_abs = e.iter().map(|x| x.abs()).sum::<f64>() / n;
            let worst = e.iter().cloned().fold(0.0f64, |m, x| m.max(x.abs()));
            let within1 = 100.0 * e.iter().filter(|x| x.abs() <= 1.0).count() as f64 / n;
            let within2 = 100.0 * e.iter().filter(|x| x.abs() <= 2.0).count() as f64 / n;
            eprintln!(
                "{label:<10} {mean:>8.2} {mean_abs:>8.2} {worst:>8.2} {within1:>7.0}% {within2:>7.0}%",
            );
        }

        eprintln!(
            "\nerr: what the window measured minus the truth, in LU (= dB).\n\
             Negative means the window heard a quieter passage than the track\n\
             really is, so the app would boost too much and the loud part would\n\
             arrive too loud. mean is the systematic bias; mean|e| is the typical\n\
             size of the mistake regardless of direction.\n",
        );
    }

    /// A track's duration in seconds, from ffmpeg's own header read.
    ///
    /// This is the number the adaptive scheme keys off, and it is free: a
    /// stream's container header arrives before any audio does, so the
    /// decision about which window to fetch can be made before fetching it.
    fn duration_secs(ffmpeg: &Path, file: &Path) -> Option<f64> {
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

    /// Which windows are worth fetching, and what each one costs.
    ///
    /// The cost is the thing the accuracy has to be weighed against: for a cold
    /// stream this is literally seconds of audio pulled over the network before
    /// the first note can play.
    const CANDIDATES: [(&str, u32, Option<u32>); 7] = [
        ("whole", 0, None),
        ("10s@60", 60, Some(10)),
        ("20s@60", 60, Some(20)),
        ("30s@30", 30, Some(30)),
        ("50s@20", 20, Some(50)),
        ("60s@20", 20, Some(60)),
        ("90s@20", 20, Some(90)),
    ];

    /// Does a duration-aware window beat one fixed rule?
    ///
    /// The proposal: read the duration first -- free, it is in the container
    /// header -- and pick the window from it. A short track is cheap to fetch
    /// whole, so fetch it whole; a long one needs a wide enough sample that one
    /// quiet passage cannot dominate.
    ///
    /// What this measures is the second half of that: how wide is wide enough,
    /// and whether it depends on how long the track is.
    #[test]
    #[ignore = "measurement"]
    fn how_wide_a_window_a_track_needs() {
        let Some(ffmpeg) = crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg) else {
            eprintln!("SKIP: no staged ffmpeg");
            return;
        };
        let Ok(entries) = std::fs::read_dir(cache_dir()) else {
            eprintln!("SKIP: no audio cache");
            return;
        };

        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "mka"))
            .collect();
        files.sort();

        // (duration, [error per candidate])
        let mut rows: Vec<(f64, Vec<Option<f64>>)> = Vec::new();

        for file in &files {
            let Some(truth) = lufs(&ffmpeg, file, 0, None) else {
                continue;
            };
            let Some(duration) = duration_secs(&ffmpeg, file) else {
                continue;
            };

            let errors: Vec<Option<f64>> = CANDIDATES
                .iter()
                .map(|(_, start, len)| {
                    // A window that starts past the end measures nothing; that
                    // is the case the duration check exists to avoid.
                    if (*start as f64) >= duration {
                        return None;
                    }
                    lufs(&ffmpeg, file, *start, *len).map(|v| v - truth)
                })
                .collect();

            rows.push((duration, errors));
        }

        let buckets: [(&str, f64, f64); 4] = [
            ("under 1:30", 0.0, 90.0),
            ("1:30-3:00", 90.0, 180.0),
            ("3:00-5:20", 180.0, 320.0),
            ("over 5:20", 320.0, f64::MAX),
        ];

        for (label, lo, hi) in buckets {
            let in_bucket: Vec<&(f64, Vec<Option<f64>>)> = rows
                .iter()
                .filter(|(d, _)| *d >= lo && *d < hi)
                .collect();
            if in_bucket.is_empty() {
                continue;
            }

            eprintln!("\n──── {label}  ({} tracks) ────", in_bucket.len());
            eprintln!(
                "{:<10} {:>8} {:>8} {:>9} {:>9}",
                "window", "mean", "typical", "worst", "within1",
            );

            for (i, (name, _, len)) in CANDIDATES.iter().enumerate() {
                let errs: Vec<f64> = in_bucket
                    .iter()
                    .filter_map(|(_, e)| e[i])
                    .collect();
                if errs.is_empty() {
                    eprintln!("{name:<10} {:>8} -- window falls outside the track", "");
                    continue;
                }
                let n = errs.len() as f64;
                let mean = errs.iter().sum::<f64>() / n;
                let typical = errs.iter().map(|x| x.abs()).sum::<f64>() / n;
                let worst = errs.iter().cloned().fold(0.0f64, |m, x| m.max(x.abs()));
                let within1 = 100.0 * errs.iter().filter(|x| x.abs() <= 1.0).count() as f64 / n;

                let cost = match len {
                    None => "whole".to_string(),
                    Some(l) => format!("{l}s"),
                };
                eprintln!(
                    "{name:<10} {mean:>8.2} {typical:>8.2} {worst:>9.2} {within1:>8.0}%   fetches {cost}",
                );
            }
        }

        eprintln!(
            "\nmean: systematic bias. typical: average size of the mistake.\n\
             within1: how often the correction lands inside 1 LU of the truth.\n\
             fetches: seconds of audio that must arrive before playback starts.\n",
        );
    }

    /// Decodes one slice to raw mono f32.
    fn pcm_slice(ffmpeg: &Path, file: &Path, start: f64, len: f64) -> Option<Vec<u8>> {
        let mut command = Command::new(ffmpeg);
        crate::sidecar::quiet(&mut command);
        let out = command
            .args(["-hide_banner", "-loglevel", "error"])
            .args(["-ss", &format!("{start:.2}")])
            .arg("-i")
            .arg(file)
            .args(["-t", &format!("{len:.2}")])
            .args(["-vn", "-f", "f32le", "-acodec", "pcm_f32le"])
            .args(["-ar", "48000", "-ac", "2"])
            .arg("-")
            .output()
            .ok()?;
        out.status.success().then_some(out.stdout)
    }

    /// Integrated loudness of several slices, measured as one signal.
    ///
    /// The slices are concatenated and measured together rather than averaged
    /// separately. That matters: LUFS is logarithmic and gated, so the mean of
    /// two measurements is not the measurement of the two. Joining the audio
    /// first is what makes this the same computation the whole-track number is.
    fn lufs_of_slices(ffmpeg: &Path, file: &Path, slices: &[(f64, f64)]) -> Option<f64> {
        use std::io::Write;

        let mut joined = Vec::new();
        for (start, len) in slices {
            joined.extend_from_slice(&pcm_slice(ffmpeg, file, *start, *len)?);
        }
        if joined.is_empty() {
            return None;
        }

        let mut command = Command::new(ffmpeg);
        crate::sidecar::quiet(&mut command);
        let mut child = command
            .args(["-hide_banner", "-nostats"])
            .args(["-f", "f32le", "-ar", "48000", "-ac", "2"])
            .args(["-i", "-"])
            .args(["-af", "ebur128=peak=true:framelog=quiet"])
            .args(["-f", "null", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;

        child.stdin.take()?.write_all(&joined).ok()?;
        let out = child.wait_with_output().ok()?;

        let stderr = String::from_utf8_lossy(&out.stderr);
        let summary = stderr.split("Summary:").nth(1)?;
        for line in summary.lines() {
            if let Some(rest) = line.trim().strip_prefix("I:") {
                let v: f64 = rest.split_whitespace().next()?.parse().ok()?;
                return v.is_finite().then_some(v);
            }
        }
        None
    }

    /// A sampling plan, as fractions of the track.
    ///
    /// Proportional rather than absolute, so one rule covers a 90-second
    /// interlude and a nine-minute epic without a table of special cases. The
    /// duration is free -- it is in the container header -- so the app knows
    /// where to seek before it fetches anything.
    struct Plan {
        name: &'static str,
        /// `(start fraction, length fraction)` pairs.
        slices: &'static [(f64, f64)],
    }

    const PLANS: [Plan; 7] = [
        // The best single window from the fixed-offset run, for comparison.
        Plan { name: "one 60%", slices: &[(0.20, 0.60)] },
        Plan { name: "one 50%", slices: &[(0.25, 0.50)] },
        Plan { name: "2 x 20%", slices: &[(0.20, 0.20), (0.60, 0.20)] },
        Plan { name: "3 x 15%", slices: &[(0.15, 0.15), (0.42, 0.15), (0.70, 0.15)] },
        Plan { name: "3 x 10%", slices: &[(0.20, 0.10), (0.45, 0.10), (0.70, 0.10)] },
        Plan { name: "4 x 10%", slices: &[(0.10, 0.10), (0.35, 0.10), (0.60, 0.10), (0.82, 0.10)] },
        Plan { name: "5 x 8%", slices: &[(0.08, 0.08), (0.28, 0.08), (0.48, 0.08), (0.66, 0.08), (0.84, 0.08)] },
    ];

    /// Is there a plan that is always within 1 LU?
    ///
    /// The fixed-offset run topped out at 88% for the 3-5 minute bucket, which
    /// is where most of a library lives. A single window can only ever see one
    /// passage; if that passage is unrepresentative there is nothing the width
    /// can do about it. Spreading the same total audio across the track should
    /// fix that for the same number of bytes -- the question is how few slices
    /// it takes.
    #[test]
    #[ignore = "measurement"]
    fn is_there_a_plan_that_is_always_close() {
        let Some(ffmpeg) = crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg) else {
            eprintln!("SKIP: no staged ffmpeg");
            return;
        };
        let Ok(entries) = std::fs::read_dir(cache_dir()) else {
            eprintln!("SKIP: no audio cache");
            return;
        };

        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "mka"))
            .collect();
        files.sort();

        let mut errors: Vec<Vec<f64>> = vec![Vec::new(); PLANS.len()];
        let mut worst_track: Vec<(f64, String)> = vec![(0.0, String::new()); PLANS.len()];

        for file in &files {
            let Some(truth) = lufs(&ffmpeg, file, 0, None) else {
                continue;
            };
            let Some(duration) = duration_secs(&ffmpeg, file) else {
                continue;
            };

            let name: String = file
                .file_stem()
                .map(|s| s.to_string_lossy().chars().take(22).collect())
                .unwrap_or_default();

            for (i, plan) in PLANS.iter().enumerate() {
                let slices: Vec<(f64, f64)> = plan
                    .slices
                    .iter()
                    .map(|(s, l)| (s * duration, l * duration))
                    .collect();
                if let Some(v) = lufs_of_slices(&ffmpeg, file, &slices) {
                    let err = v - truth;
                    errors[i].push(err);
                    if err.abs() > worst_track[i].0 {
                        worst_track[i] = (err.abs(), name.clone());
                    }
                }
            }
        }

        eprintln!(
            "\n{:<10} {:>8} {:>8} {:>8} {:>8} {:>8}   {:<24}",
            "plan", "mean", "typical", "worst", "within1", "fetch", "worst on",
        );
        for (i, plan) in PLANS.iter().enumerate() {
            let e = &errors[i];
            if e.is_empty() {
                continue;
            }
            let n = e.len() as f64;
            let mean = e.iter().sum::<f64>() / n;
            let typical = e.iter().map(|x| x.abs()).sum::<f64>() / n;
            let worst = e.iter().cloned().fold(0.0f64, |m, x| m.max(x.abs()));
            let within1 = 100.0 * e.iter().filter(|x| x.abs() <= 1.0).count() as f64 / n;
            let fetch: f64 = plan.slices.iter().map(|(_, l)| l).sum::<f64>() * 100.0;

            eprintln!(
                "{:<10} {mean:>8.2} {typical:>8.2} {worst:>8.2} {within1:>7.0}% {fetch:>7.0}%   {:<24}",
                plan.name, worst_track[i].1,
            );
        }

        eprintln!(
            "\nfetch: how much of the track has to arrive, as a percentage.\n\
             within1: how often the correction lands inside 1 LU of the truth.\n\
             worst on: the track that defeated the plan, so it can be listened to.\n",
        );
    }

    /// What the trick is actually worth, over the network, on a cold stream.
    ///
    /// Everything above measures accuracy against local files. This measures
    /// the thing the accuracy is being traded for: how long a listener waits
    /// before the first note, on a track nobody has played before.
    ///
    /// Three routes, timed end to end from a resolved URL:
    ///
    /// - **uncorrected** -- what happens today with levelling off. Decode
    ///   enough to start and go. The floor; nothing can beat it.
    /// - **whole track** -- what "wait to measure" costs today: the entire
    ///   stream has to arrive before its integrated loudness exists.
    /// - **sampled** -- the proposal: fetch only the slices the plan asks for,
    ///   by range request, measure those, then start.
    ///
    /// The sampled route is only worth building if it lands much closer to the
    /// floor than to the whole-track figure.
    #[test]
    #[ignore = "hits the network"]
    fn what_a_cold_stream_costs_with_and_without_sampling() {
        use std::time::Instant;

        let Some(ffmpeg) = crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg) else {
            eprintln!("SKIP: no staged ffmpeg");
            return;
        };
        let Some(yt_dlp) = crate::sidecar::staged_for_tests(crate::sidecar::Tool::YtDlp) else {
            eprintln!("SKIP: no staged yt-dlp");
            return;
        };

        // Three real tracks of different lengths, so the shape of the answer
        // against duration is visible rather than assumed.
        let pages = [
            "https://www.youtube.com/watch?v=HJRz4pROLxE",
            "https://www.youtube.com/watch?v=MlcJQYON2Go",
            "https://www.youtube.com/watch?v=N5Q8EeyOiKY",
        ];

        eprintln!(
            "\n{:<14} {:>9} {:>12} {:>12} {:>12}",
            "track", "duration", "start only", "whole track", "sampled",
        );

        for page in pages {
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            let resolved = runtime.block_on(crate::youtube::resolve_stream_url(
                &yt_dlp,
                page,
                crate::stream_urls::Encoding::Preferred,
            ));
            let Ok(resolved) = resolved else {
                eprintln!("{:<14} could not resolve", &page[32..]);
                continue;
            };
            let url = resolved.url.clone();

            let Some(duration) = url_duration(&ffmpeg, &url) else {
                eprintln!("{:<14} no duration", &page[32..]);
                continue;
            };

            // 1. Enough to start playing, and no more. The prefill the engine
            //    already waits for before the first sample reaches the device.
            let started = Instant::now();
            let _ = pcm_from_url(&ffmpeg, &url, 0.0, 0.5);
            let start_only = started.elapsed().as_secs_f64();

            // 2. The whole stream, which is what an integrated measurement of
            //    the real thing costs.
            let started = Instant::now();
            let whole = lufs_from_url(&ffmpeg, &url, None);
            let whole_secs = started.elapsed().as_secs_f64();

            // 3. The plan: four slices at a tenth of the track each.
            let started = Instant::now();
            let slices: Vec<(f64, f64)> = [(0.10, 0.10), (0.35, 0.10), (0.60, 0.10), (0.82, 0.10)]
                .iter()
                .map(|(s, l)| (s * duration, l * duration))
                .collect();
            let mut joined = Vec::new();
            for (s, l) in &slices {
                if let Some(bytes) = pcm_from_url(&ffmpeg, &url, *s, *l) {
                    joined.extend_from_slice(&bytes);
                }
            }
            let sampled = lufs_of_pcm(&ffmpeg, &joined);
            let sampled_secs = started.elapsed().as_secs_f64();

            eprintln!(
                "{:<14} {duration:>8.0}s {start_only:>11.2}s {whole_secs:>11.2}s {sampled_secs:>11.2}s",
                &page[32..],
            );
            if let (Some(w), Some(s)) = (whole, sampled) {
                eprintln!(
                    "{:<14} {:>9} loudness {w:.1} LUFS vs sampled {s:.1} -- error {:.2} LU",
                    "", "", s - w,
                );
            }
        }

        eprintln!(
            "\nstart only: the floor -- what playing with no correction costs.\n\
             whole track: what an honest integrated measurement costs today.\n\
             sampled: the proposal. Worth building if it is near the floor.\n",
        );
    }

    fn url_duration(ffmpeg: &Path, url: &str) -> Option<f64> {
        let mut command = Command::new(ffmpeg);
        crate::sidecar::quiet(&mut command);
        let out = command
            .args(["-hide_banner", "-nostats"])
            .args(["-i", url])
            .args(["-f", "null", "-t", "0.01", "-"])
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

    /// One slice of a remote stream, by range request.
    ///
    /// `-ss` before `-i` is what makes this a range request rather than a
    /// download-and-discard: ffmpeg asks the server for the byte offset the
    /// container index points at.
    fn pcm_from_url(ffmpeg: &Path, url: &str, start: f64, len: f64) -> Option<Vec<u8>> {
        let mut command = Command::new(ffmpeg);
        crate::sidecar::quiet(&mut command);
        let out = command
            .args(["-hide_banner", "-loglevel", "error"])
            .args(["-ss", &format!("{start:.2}")])
            .args(["-i", url])
            .args(["-t", &format!("{len:.2}")])
            .args(["-vn", "-f", "f32le", "-acodec", "pcm_f32le"])
            .args(["-ar", "48000", "-ac", "1"])
            .arg("-")
            .output()
            .ok()?;
        out.status.success().then_some(out.stdout)
    }

    fn lufs_from_url(ffmpeg: &Path, url: &str, len: Option<f64>) -> Option<f64> {
        let mut command = Command::new(ffmpeg);
        crate::sidecar::quiet(&mut command);
        command.args(["-hide_banner", "-nostats"]).args(["-i", url]);
        if let Some(len) = len {
            command.args(["-t", &format!("{len:.2}")]);
        }
        let out = command
            .args(["-af", "ebur128=peak=true:framelog=quiet"])
            .args(["-f", "null", "-"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .output()
            .ok()?;
        parse_i(&String::from_utf8_lossy(&out.stderr))
    }

    fn lufs_of_pcm(ffmpeg: &Path, pcm: &[u8]) -> Option<f64> {
        use std::io::Write;
        if pcm.is_empty() {
            return None;
        }
        let mut command = Command::new(ffmpeg);
        crate::sidecar::quiet(&mut command);
        let mut child = command
            .args(["-hide_banner", "-nostats"])
            .args(["-f", "f32le", "-ar", "48000", "-ac", "1"])
            .args(["-i", "-"])
            .args(["-af", "ebur128=peak=true:framelog=quiet"])
            .args(["-f", "null", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        child.stdin.take()?.write_all(pcm).ok()?;
        let out = child.wait_with_output().ok()?;
        parse_i(&String::from_utf8_lossy(&out.stderr))
    }

    fn parse_i(stderr: &str) -> Option<f64> {
        let summary = stderr.split("Summary:").nth(1)?;
        for line in summary.lines() {
            if let Some(rest) = line.trim().strip_prefix("I:") {
                let v: f64 = rest.split_whitespace().next()?.parse().ok()?;
                return v.is_finite().then_some(v);
            }
        }
        None
    }

    /// Where the seconds actually go.
    ///
    /// The sampling idea rests on an assumption nobody checked: that the cost
    /// of measuring a cold stream is *the bytes*, so fetching 40% of them costs
    /// 40% as much. The timings did not behave that way -- one track took 3.05s
    /// sampled against 3.08s whole, no saving at all -- which means the bytes
    /// are not what dominates.
    ///
    /// This takes the same work apart:
    ///
    /// - **measure local** -- decode plus ebur128 on a file already on disk.
    ///   No network at all, so this is the pure computation.
    /// - **download only** -- the whole stream pulled with `-c copy`, so it is
    ///   demuxed but never decoded and never measured. Pure transfer.
    /// - **one slice** -- a single range request for a tenth of the track,
    ///   decoded and returned. One lot of connection and container setup.
    ///
    /// If `measure local` is small and `download only` is most of the whole
    /// figure, the cost is transfer and sampling should help. If `one slice`
    /// is most of the whole figure by itself, the cost is per-request setup and
    /// sampling cannot help however few bytes it asks for.
    #[test]
    #[ignore = "hits the network"]
    fn where_the_time_actually_goes() {
        use std::time::Instant;

        let Some(ffmpeg) = crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg) else {
            eprintln!("SKIP: no staged ffmpeg");
            return;
        };
        let Some(yt_dlp) = crate::sidecar::staged_for_tests(crate::sidecar::Tool::YtDlp) else {
            eprintln!("SKIP: no staged yt-dlp");
            return;
        };

        // A local file first, to price the computation with no network in it.
        if let Ok(entries) = std::fs::read_dir(cache_dir()) {
            let mut files: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "mka"))
                .collect();
            files.sort();

            eprintln!("\n─── decode + ebur128, local file, no network ───");
            for file in files.iter().take(3) {
                let Some(duration) = duration_secs(&ffmpeg, file) else {
                    continue;
                };
                let started = Instant::now();
                let _ = lufs(&ffmpeg, file, 0, None);
                let elapsed = started.elapsed().as_secs_f64();
                let size = std::fs::metadata(file).map(|m| m.len()).unwrap_or(0);
                eprintln!(
                    "  {duration:>5.0}s track, {:>5.1} MB:  {elapsed:>6.2}s  ({:.0}x realtime)",
                    size as f64 / 1e6,
                    duration / elapsed.max(1e-6),
                );
            }
        }

        eprintln!("\n─── the same work over the network ───");
        let pages = [
            "https://www.youtube.com/watch?v=HJRz4pROLxE",
            "https://www.youtube.com/watch?v=N5Q8EeyOiKY",
        ];

        for page in pages {
            let runtime = tokio::runtime::Runtime::new().expect("runtime");
            let Ok(resolved) = runtime.block_on(crate::youtube::resolve_stream_url(
                &yt_dlp,
                page,
                crate::stream_urls::Encoding::Preferred,
            )) else {
                eprintln!("  {} -- could not resolve", &page[32..]);
                continue;
            };
            let url = resolved.url;
            let Some(duration) = url_duration(&ffmpeg, &url) else {
                continue;
            };

            // Transfer alone: demuxed, never decoded, never measured.
            let started = Instant::now();
            let mut copy = Command::new(&ffmpeg);
            crate::sidecar::quiet(&mut copy);
            let _ = copy
                .args(["-hide_banner", "-loglevel", "error"])
                .args(["-i", &url])
                .args(["-c", "copy", "-f", "null", "-"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .stdin(Stdio::null())
                .status();
            let download_only = started.elapsed().as_secs_f64();

            // Transfer plus decode plus measure.
            let started = Instant::now();
            let _ = lufs_from_url(&ffmpeg, &url, None);
            let whole = started.elapsed().as_secs_f64();

            // One range request for a tenth of it.
            let started = Instant::now();
            let _ = pcm_from_url(&ffmpeg, &url, duration * 0.35, duration * 0.10);
            let one_slice = started.elapsed().as_secs_f64();

            eprintln!(
                "\n  {} ({duration:.0}s)",
                &page[32..],
            );
            eprintln!("    download only (no decode, no measure): {download_only:>6.2}s");
            eprintln!("    download + decode + measure:           {whole:>6.2}s");
            eprintln!("    one 10% slice by range request:        {one_slice:>6.2}s");
            eprintln!(
                "    -> four such slices would cost about   {:>6.2}s",
                one_slice * 4.0,
            );
        }

        eprintln!(
            "\nIf one slice already costs a large share of the whole, the cost is\n\
             per-request setup rather than bytes, and asking for fewer bytes in\n\
             more requests cannot win.\n",
        );
    }

    /// How good is the estimate from the audio that has already played?
    ///
    /// This is the measurement the design actually needs, and none of the
    /// earlier ones answer it. Playback already streams -- ffmpeg reads the URL
    /// and the first samples reach the device in about 0.2 s -- and it already
    /// writes a cache copy from the same read, for no extra network. So the
    /// audio is passing through the app anyway; nothing needs fetching to
    /// measure it.
    ///
    /// What that costs instead is *time*. An estimate taken N seconds in is the
    /// loudness of the first N seconds, and the first N seconds of a track are
    /// systematically quieter than the whole -- 5 LU quiet at ten seconds.
    /// The question is how far in that bias decays to something worth acting
    /// on.
    #[test]
    #[ignore = "measurement"]
    fn how_soon_does_a_playing_track_know_its_own_loudness() {
        let Some(ffmpeg) = crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg) else {
            eprintln!("SKIP: no staged ffmpeg");
            return;
        };
        let Ok(entries) = std::fs::read_dir(cache_dir()) else {
            eprintln!("SKIP: no audio cache");
            return;
        };

        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "mka"))
            .collect();
        files.sort();

        const HEARD: [u32; 7] = [15, 30, 45, 60, 90, 120, 150];
        let mut errors: Vec<Vec<f64>> = vec![Vec::new(); HEARD.len()];

        for file in &files {
            let Some(truth) = lufs(&ffmpeg, file, 0, None) else {
                continue;
            };
            let Some(duration) = duration_secs(&ffmpeg, file) else {
                continue;
            };

            for (i, secs) in HEARD.iter().enumerate() {
                // Past the end is not an estimate, it is the answer.
                if (*secs as f64) >= duration {
                    continue;
                }
                if let Some(v) = lufs(&ffmpeg, file, 0, Some(*secs)) {
                    errors[i].push(v - truth);
                }
            }
        }

        eprintln!(
            "\n{:<10} {:>8} {:>9} {:>8} {:>9} {:>9}",
            "heard", "mean", "typical", "worst", "within1", "within2",
        );
        for (i, secs) in HEARD.iter().enumerate() {
            let e = &errors[i];
            if e.is_empty() {
                continue;
            }
            let n = e.len() as f64;
            let mean = e.iter().sum::<f64>() / n;
            let typical = e.iter().map(|x| x.abs()).sum::<f64>() / n;
            let worst = e.iter().cloned().fold(0.0f64, |m, x| m.max(x.abs()));
            let within1 = 100.0 * e.iter().filter(|x| x.abs() <= 1.0).count() as f64 / n;
            let within2 = 100.0 * e.iter().filter(|x| x.abs() <= 2.0).count() as f64 / n;
            eprintln!(
                "{:<10} {mean:>8.2} {typical:>9.2} {worst:>8.2} {within1:>8.0}% {within2:>8.0}%   ({} tracks)",
                format!("{secs}s"),
                e.len(),
            );
        }

        eprintln!(
            "\nheard: how much of the track has played when the estimate is taken.\n\
             Nothing is fetched for any of these -- it is the audio the listener\n\
             is already hearing. The only cost is waiting.\n",
        );
    }

    /// Heard audio plus a few fetched slices: how soon does that reach 1 LU?
    ///
    /// The two halves fail in opposite ways and cost opposite things.
    ///
    /// In-line measurement is free -- the audio is already passing through for
    /// playback -- but it only ever knows the *beginning* of a track, and
    /// beginnings are systematically quiet. It needs 150 seconds to be reliably
    /// within 1 LU, which most of a listen has gone by.
    ///
    /// Slices see the whole shape immediately but each costs a range request,
    /// measured at about 0.35 s of mostly fixed setup.
    ///
    /// Together they should need far less of either: the heard part covers the
    /// opening for nothing, so the slices only have to cover what has not
    /// played yet. Slices are placed strictly after the heard region so no
    /// audio is counted twice.
    #[test]
    #[ignore = "measurement"]
    fn heard_audio_plus_a_few_slices() {
        let Some(ffmpeg) = crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg) else {
            eprintln!("SKIP: no staged ffmpeg");
            return;
        };
        let Ok(entries) = std::fs::read_dir(cache_dir()) else {
            eprintln!("SKIP: no audio cache");
            return;
        };

        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "mka"))
            .collect();
        files.sort();

        /// `(heard seconds, how many slices, each as a fraction of the track)`
        const PLANS: [(u32, usize, f64); 8] = [
            (15, 1, 0.10),
            (15, 2, 0.10),
            (15, 3, 0.10),
            (30, 1, 0.10),
            (30, 2, 0.10),
            (30, 3, 0.10),
            (30, 2, 0.15),
            (45, 2, 0.10),
        ];

        let mut errors: Vec<Vec<f64>> = vec![Vec::new(); PLANS.len()];

        for file in &files {
            let Some(truth) = lufs(&ffmpeg, file, 0, None) else {
                continue;
            };
            let Some(duration) = duration_secs(&ffmpeg, file) else {
                continue;
            };

            for (i, (heard, count, width)) in PLANS.iter().enumerate() {
                let heard_secs = *heard as f64;
                if heard_secs >= duration {
                    continue;
                }

                let mut slices = vec![(0.0, heard_secs)];

                // Spread the slices over what has not played, stopping short of
                // the very end where fades live.
                let from = heard_secs / duration;
                let to = 0.95 - width;
                if to > from {
                    for k in 0..*count {
                        let t = if *count == 1 {
                            0.5
                        } else {
                            k as f64 / (*count - 1) as f64
                        };
                        let start = (from + (to - from) * t) * duration;
                        slices.push((start, width * duration));
                    }
                }

                if let Some(v) = lufs_of_slices(&ffmpeg, file, &slices) {
                    errors[i].push(v - truth);
                }
            }
        }

        eprintln!(
            "\n{:<18} {:>8} {:>9} {:>8} {:>9} {:>8}",
            "plan", "mean", "typical", "worst", "within1", "fetch",
        );
        for (i, (heard, count, width)) in PLANS.iter().enumerate() {
            let e = &errors[i];
            if e.is_empty() {
                continue;
            }
            let n = e.len() as f64;
            let mean = e.iter().sum::<f64>() / n;
            let typical = e.iter().map(|x| x.abs()).sum::<f64>() / n;
            let worst = e.iter().cloned().fold(0.0f64, |m, x| m.max(x.abs()));
            let within1 = 100.0 * e.iter().filter(|x| x.abs() <= 1.0).count() as f64 / n;

            eprintln!(
                "{:<18} {mean:>8.2} {typical:>9.2} {worst:>8.2} {within1:>8.0}% {:>7.0}%",
                format!("{heard}s + {count}x{:.0}%", width * 100.0),
                *count as f64 * width * 100.0,
            );
        }

        eprintln!(
            "\nfetch: how much of the track is pulled over the network. The heard\n\
             part costs nothing -- it is the audio already playing.\n",
        );
    }

    /// The same hybrid, but not counting the intro.
    ///
    /// The first attempt was beaten by pure slices, and the reason is in the
    /// bias table: the free audio is the *opening*, which measures 3.9 LU quiet
    /// at fifteen seconds and is the least representative part of any track.
    /// Concatenating it with well-chosen slices drags the whole estimate down.
    ///
    /// Free is not the same as useful. This keeps the heard audio but starts it
    /// after the intro, so what is counted is playing audio that is actually
    /// representative -- and the slices still cover what has not played.
    #[test]
    #[ignore = "measurement"]
    fn heard_audio_after_the_intro_plus_slices() {
        let Some(ffmpeg) = crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg) else {
            eprintln!("SKIP: no staged ffmpeg");
            return;
        };
        let Ok(entries) = std::fs::read_dir(cache_dir()) else {
            eprintln!("SKIP: no audio cache");
            return;
        };

        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "mka"))
            .collect();
        files.sort();

        /// `(skip, heard until, slice count, slice width)`
        const PLANS: [(u32, u32, usize, f64); 8] = [
            (20, 45, 2, 0.10),
            (20, 45, 3, 0.10),
            (20, 60, 2, 0.10),
            (20, 60, 3, 0.10),
            (20, 90, 2, 0.10),
            (20, 90, 3, 0.10),
            (15, 60, 3, 0.10),
            (20, 60, 4, 0.10),
        ];

        let mut errors: Vec<Vec<f64>> = vec![Vec::new(); PLANS.len()];

        for file in &files {
            let Some(truth) = lufs(&ffmpeg, file, 0, None) else {
                continue;
            };
            let Some(duration) = duration_secs(&ffmpeg, file) else {
                continue;
            };

            for (i, (skip, until, count, width)) in PLANS.iter().enumerate() {
                let (skip, until) = (*skip as f64, *until as f64);
                if until >= duration {
                    continue;
                }

                let mut slices = vec![(skip, until - skip)];

                let from = until / duration;
                let to = 0.95 - width;
                if to > from {
                    for k in 0..*count {
                        let t = if *count == 1 {
                            0.5
                        } else {
                            k as f64 / (*count - 1) as f64
                        };
                        slices.push((((from + (to - from) * t) * duration), width * duration));
                    }
                }

                if let Some(v) = lufs_of_slices(&ffmpeg, file, &slices) {
                    errors[i].push(v - truth);
                }
            }
        }

        eprintln!(
            "\n{:<22} {:>8} {:>9} {:>8} {:>9} {:>8}",
            "plan", "mean", "typical", "worst", "within1", "fetch",
        );
        for (i, (skip, until, count, width)) in PLANS.iter().enumerate() {
            let e = &errors[i];
            if e.is_empty() {
                continue;
            }
            let n = e.len() as f64;
            let mean = e.iter().sum::<f64>() / n;
            let typical = e.iter().map(|x| x.abs()).sum::<f64>() / n;
            let worst = e.iter().cloned().fold(0.0f64, |m, x| m.max(x.abs()));
            let within1 = 100.0 * e.iter().filter(|x| x.abs() <= 1.0).count() as f64 / n;

            eprintln!(
                "{:<22} {mean:>8.2} {typical:>9.2} {worst:>8.2} {within1:>8.0}% {:>7.0}%   ({} tracks)",
                format!("{skip}-{until}s + {count}x{:.0}%", width * 100.0),
                *count as f64 * width * 100.0,
                e.len(),
            );
        }

        eprintln!(
            "\nfetch: pulled over the network. The heard part is free -- it is the\n\
             audio already playing -- but only the part after the intro is used.\n",
        );
    }
}
