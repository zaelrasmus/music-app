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
}
