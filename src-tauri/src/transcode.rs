use std::io::Read;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rodio::{ChannelCount, Sample, SampleRate, Source};
use rtrb::{Consumer, Producer, PushError, RingBuffer};

use crate::audio_cache::PendingCache;

/// What we ask ffmpeg to produce. Matching rodio's native sample type means the
/// bytes off the pipe are already the samples we hand to the mixer -- no
/// conversion, no allocation.
const OUTPUT_RATE: u32 = 44_100;
const OUTPUT_CHANNELS: u16 = 2;

const SAMPLES_PER_SECOND: usize = OUTPUT_RATE as usize * OUTPUT_CHANNELS as usize;

/// Slack between ffmpeg and the speakers, for a local file.
///
/// Small on purpose: a disk read that stalls for five seconds has bigger
/// problems than audio, so more headroom would just be idle memory.
const BUFFER_SECONDS_FILE: usize = 5;

/// The same, for a network stream.
///
/// Much larger because the failure it guards against is real and common:
/// congestion, a wifi handover, a provider throttling mid-track. Anything
/// longer than the buffer becomes audible silence. Thirty seconds costs about
/// 10 MB of memory and makes ordinary hiccups invisible.
const BUFFER_SECONDS_NETWORK: usize = 30;

/// Filled before playback starts so the opening moments cannot underrun.
const PREFILL_SAMPLES: usize = SAMPLES_PER_SECOND / 2;

/// How long to wait for ffmpeg to produce that first half second.
pub(crate) const PREFILL_TIMEOUT: Duration = Duration::from_secs(15);

/// Enough of ffmpeg's stderr to explain a failure, and no more.
const MAX_STDERR: usize = 4096;

/// Audio decoded by ffmpeg, presented to rodio as an ordinary [`Source`].
///
/// This exists because rodio's own `Decoder` requires `Read + Seek`, and a pipe
/// is not seekable -- so ffmpeg's output can never be handed to it. Instead
/// ffmpeg does the demuxing and decoding, emits raw `f32` samples, and this
/// type is the `Source` rodio actually plays. That covers every format rodio
/// lacks a codec for (Opus above all) with one mechanism.
pub struct FfmpegSource {
    consumer: Consumer<Sample>,
    /// Set by the reader thread once ffmpeg's output ends.
    finished: Arc<AtomicBool>,
    /// Set while the buffer has run dry but the source has *not* ended --
    /// which for a network stream means the connection has stopped keeping
    /// up.
    ///
    /// Distinct from `finished` on purpose: silence because nothing is
    /// arriving and silence because the song is over need completely
    /// different responses, and they are indistinguishable from the samples
    /// alone.
    starved: Arc<AtomicBool>,
    child: Child,
    /// A cache copy being written alongside playback, committed on `Drop`
    /// only if ffmpeg got all the way to the end.
    pending_cache: Option<PendingCache>,
}

/// What ffmpeg should read.
///
/// The distinction matters because a network source needs reconnect options
/// that are meaningless -- and rejected -- for a file.
#[derive(Debug, Clone, Copy)]
pub enum FfmpegInput<'a> {
    File(&'a Path),
    Url(&'a str),
}

impl FfmpegInput<'_> {
    /// How much decoded audio to hold ahead of the speakers.
    ///
    /// A network stream needs far more slack than a file: the buffer is
    /// exactly what a stall has to outlast before it becomes silence.
    fn buffer_samples(self) -> usize {
        let seconds = match self {
            FfmpegInput::File(_) => BUFFER_SECONDS_FILE,
            FfmpegInput::Url(_) => BUFFER_SECONDS_NETWORK,
        };
        SAMPLES_PER_SECOND * seconds
    }
}

impl FfmpegSource {
    /// Same, but beginning `start` into the audio.
    ///
    /// This is how seeking works for anything ffmpeg decodes. rodio drives
    /// `try_seek` from the audio callback thread, where spawning a process
    /// would stall playback, so the engine restarts the decode here instead
    /// and tracks the offset itself.
    pub fn open_at(
        ffmpeg: &Path,
        input: FfmpegInput<'_>,
        start: Duration,
        cache: Option<PendingCache>,
    ) -> Result<Self, String> {
        // Its own temp file. The reservation is per *track*, but this is one
        // decode of it, and a seek can leave two overlapping briefly -- the
        // outgoing one deleting its temp file on the way out would otherwise
        // take the incoming one's with it.
        let cache = cache.map(|pending| pending.for_decode());

        let mut command = Command::new(ffmpeg);
        command.arg("-hide_banner").args(["-loglevel", "error"]);

        if let FfmpegInput::Url(_) = input {
            // A dropped connection mid-song should be retried, not fatal.
            // These are HTTP-protocol options and must precede `-i`.
            command
                .args(["-reconnect", "1"])
                .args(["-reconnect_streamed", "1"])
                .args(["-reconnect_delay_max", "5"]);
        }

        if !start.is_zero() {
            // Before `-i`, which makes this an *input* seek: ffmpeg jumps
            // there via a range request or the container index. After `-i` it
            // would instead decode and discard everything up to that point --
            // correct, but many seconds of work for a four-minute seek.
            command.args(["-ss", &format!("{:.3}", start.as_secs_f64())]);
        }

        command.arg("-i");
        match input {
            FfmpegInput::File(path) => command.arg(path),
            FfmpegInput::Url(url) => command.arg(url),
        };

        command
            // Drop any cover art, then emit bare interleaved f32.
            .args(["-vn", "-f", "f32le", "-acodec", "pcm_f32le"])
            .args(["-ar", &OUTPUT_RATE.to_string()])
            .args(["-ac", &OUTPUT_CHANNELS.to_string()])
            .arg("-");

        // A second output from the same decode. ffmpeg reads the input once
        // and writes both, so the cache copy costs no extra network traffic --
        // it is the bytes already arriving, stream-copied rather than
        // re-encoded, so what lands is exactly what the provider sent.
        if let Some(pending) = &cache {
            command
                .args(["-vn", "-c", "copy", "-f", "matroska"])
                .arg(&pending.partial);
        }

        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Could not start ffmpeg: {e}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or("ffmpeg produced no output stream.")?;
        let stderr = child.stderr.take();

        let (producer, consumer) = RingBuffer::<Sample>::new(input.buffer_samples());
        let finished = Arc::new(AtomicBool::new(false));
        let starved = Arc::new(AtomicBool::new(false));

        spawn_reader(stdout, producer, finished.clone());
        let errors = stderr.map(spawn_stderr_drain);

        let source = Self {
            consumer,
            finished,
            starved,
            child,
            pending_cache: cache,
        };

        source.wait_for_prefill()?;

        // Nothing arrived and ffmpeg is done: it rejected the input. Its stderr
        // is the only thing that explains why.
        if source.consumer.slots() == 0 && source.finished.load(Ordering::Acquire) {
            let detail = errors
                .and_then(|e| e.lock().ok().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "ffmpeg produced no audio.".to_string());

            return Err(explain_ffmpeg(&detail));
        }

        Ok(source)
    }

    /// Whether the decoder is currently starved of input.
    ///
    /// Handed to the engine so it can tell a stalled connection from a track
    /// that simply ended, without reaching into the source rodio owns.
    pub fn starvation_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.starved)
    }

    fn wait_for_prefill(&self) -> Result<(), String> {
        let deadline = Instant::now() + PREFILL_TIMEOUT;

        while self.consumer.slots() < PREFILL_SAMPLES {
            if self.finished.load(Ordering::Acquire) {
                // Ended early -- either a decode failure or a very short file.
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err("ffmpeg did not produce audio in time.".to_string());
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        Ok(())
    }
}

/// Moves ffmpeg's bytes into the ring buffer.
///
/// Runs on its own thread precisely so that [`Iterator::next`] -- which rodio
/// polls from the audio callback -- never has to touch a pipe.
fn spawn_reader(mut stdout: ChildStdout, mut producer: Producer<Sample>, finished: Arc<AtomicBool>) {
    let _ = std::thread::Builder::new()
        .name("ffmpeg-reader".to_string())
        .spawn(move || {
            let mut buffer = [0u8; 8192];
            // A read can split an f32 across two chunks; hold the remainder.
            let mut partial: Vec<u8> = Vec::with_capacity(4);

            loop {
                let read = match stdout.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };

                partial.extend_from_slice(&buffer[..read]);
                let whole = partial.len() - (partial.len() % 4);

                for frame in partial[..whole].chunks_exact(4) {
                    let sample =
                        f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as Sample;

                    if push_blocking(&mut producer, sample).is_err() {
                        // The source was dropped; stop feeding a dead buffer.
                        finished.store(true, Ordering::Release);
                        return;
                    }
                }

                partial.drain(..whole);
            }

            finished.store(true, Ordering::Release);
        });
}

/// Waits for room rather than dropping samples. Errors only if the consumer is
/// gone, which means playback stopped.
fn push_blocking(producer: &mut Producer<Sample>, sample: Sample) -> Result<(), ()> {
    let mut pending = sample;

    loop {
        match producer.push(pending) {
            Ok(()) => return Ok(()),
            Err(PushError::Full(returned)) => {
                if producer.is_abandoned() {
                    return Err(());
                }
                pending = returned;
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// Keeps ffmpeg's stderr drained so it can never fill its pipe and block.
fn spawn_stderr_drain(mut stderr: ChildStderr) -> Arc<Mutex<String>> {
    let collected = Arc::new(Mutex::new(String::new()));
    let sink = collected.clone();

    let _ = std::thread::Builder::new()
        .name("ffmpeg-stderr".to_string())
        .spawn(move || {
            let mut buffer = [0u8; 1024];
            while let Ok(read) = stderr.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                if let Ok(mut text) = sink.lock() {
                    if text.len() < MAX_STDERR {
                        text.push_str(&String::from_utf8_lossy(&buffer[..read]));
                    }
                }
            }
        });

    collected
}

impl Iterator for FfmpegSource {
    type Item = Sample;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self.consumer.pop() {
            Ok(sample) => {
                // A relaxed load per sample is a plain memory read; the store
                // only happens on the edge out of starvation.
                if self.starved.load(Ordering::Relaxed) {
                    self.starved.store(false, Ordering::Relaxed);
                }
                Some(sample)
            }
            Err(_) if self.finished.load(Ordering::Acquire) => None,
            // Underrun: ffmpeg has not kept up. A moment of silence beats
            // ending the track, which is what returning None would do -- but
            // the engine is told, so a long one can be reported as what it is
            // rather than played as silence forever.
            Err(_) => {
                self.starved.store(true, Ordering::Relaxed);
                Some(0.0)
            }
        }
    }
}

impl Source for FfmpegSource {
    fn current_span_len(&self) -> Option<usize> {
        // Rate and channel count are fixed by the ffmpeg arguments above, so
        // they never change mid-stream.
        None
    }

    fn channels(&self) -> ChannelCount {
        ChannelCount::new(OUTPUT_CHANNELS).expect("channel count is a non-zero constant")
    }

    fn sample_rate(&self) -> SampleRate {
        SampleRate::new(OUTPUT_RATE).expect("sample rate is a non-zero constant")
    }

    fn total_duration(&self) -> Option<Duration> {
        // Raw PCM carries no duration. The UI takes it from `tracks`, which
        // lofty already filled in during the scan.
        None
    }

    // `try_seek` keeps its default of `SeekError::NotSupported`. Seeking would
    // mean restarting ffmpeg with `-ss`, and rodio drives seeks from the audio
    // callback thread, where spawning a process would stall playback. Doing it
    // at the engine level instead is the follow-up.
}

impl Drop for FfmpegSource {
    fn drop(&mut self) {
        // The reader saw the output end, which only happens when ffmpeg closed
        // its stdout -- so it is exiting on its own and worth waiting for.
        // Otherwise this is a skip or a seek, and it has to be killed before it
        // lives on writing into a pipe nobody reads.
        let ended = self.finished.load(Ordering::Acquire);
        if !ended {
            let _ = self.child.kill();
        }
        let status = self.child.wait().ok();

        let Some(pending) = self.pending_cache.take() else {
            return;
        };

        // Only a clean, complete run produces a usable cache entry. Anything
        // else leaves a truncated song, which would be worse than no cache at
        // all -- it would play and then stop early for no visible reason.
        if ended && status.is_some_and(|s| s.success()) {
            pending.commit();
        } else {
            pending.discard();
        }
    }
}

/// Turns ffmpeg's stderr into something a person can act on.
///
/// Raw ffmpeg output is unusable in a toast: a failed stream dumps the entire
/// signed googlevideo URL, which is two kilobytes of query string. The network
/// cases in particular need naming, because "403" from YouTube almost always
/// means the bundled yt-dlp has aged out rather than anything being wrong with
/// the track.
pub fn explain_ffmpeg(stderr: &str) -> String {
    let lowered = stderr.to_lowercase();

    if lowered.contains("403") || lowered.contains("forbidden") {
        // Seen in practice even with a current yt-dlp: YouTube rate-limits
        // bursts of requests, and periodically changes what it will serve.
        // Waiting usually works; updating yt-dlp is the fix when it does not.
        return "YouTube refused this stream. Wait a moment and try again -- if \
                it keeps happening, yt-dlp may need updating."
            .to_string();
    }
    if lowered.contains("404") || lowered.contains("not found") {
        return "That stream is no longer available. Try playing it again to \
                fetch a fresh link."
            .to_string();
    }
    if lowered.contains("failed to resolve")
        || lowered.contains("network is unreachable")
        || lowered.contains("connection refused")
        || lowered.contains("timed out")
    {
        return "Could not reach the audio. Check your internet connection.".to_string();
    }
    if lowered.contains("no such file") {
        return "That file is missing from disk.".to_string();
    }

    // Something unanticipated: keep the first line only, and keep it short.
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("ffmpeg could not decode this audio.");

    let mut summary: String = first.chars().take(200).collect();
    if first.chars().count() > 200 {
        summary.push('…');
    }

    format!("Could not decode this audio: {summary}")
}

/// Whether a failure is worth retrying with a freshly resolved URL.
///
/// YouTube rejects a sizeable share of these fetches — measured at roughly one
/// in three on a warm IP — and the same request often succeeds moments later
/// with a new URL. That makes 403 a transient condition rather than a verdict.
pub fn is_transient(message: &str) -> bool {
    message.contains("YouTube refused")
}

/// Whether a file needs ffmpeg, based on what rodio can actually decode.
///
/// rodio's enabled codecs are mp3, flac, wav, AAC/ALAC in mp4, and Vorbis in
/// Ogg. Opus has no symphonia codec at all, so anything carrying it must go
/// through ffmpeg.
pub fn needs_transcode(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|e| e.to_str()) else {
        // No extension: let rodio try, and fall back if it cannot cope.
        return false;
    };

    match extension.to_lowercase().as_str() {
        // Always Opus, or a container rodio has no demuxer for.
        "opus" | "webm" | "weba" | "mka" | "mkv" => true,
        // Ogg carries Vorbis (native) or Opus (not) -- the extension alone
        // cannot say which, so look inside.
        "ogg" | "oga" => is_opus_stream(path),
        _ => false,
    }
}

/// Reads the head of an Ogg file to identify its codec.
///
/// An Ogg stream announces itself in the first page: an Opus stream's first
/// packet begins with the magic `OpusHead`, a Vorbis stream's with `\x01vorbis`.
/// Checking the bytes is both cheaper and more honest than trusting `.ogg`.
fn is_opus_stream(path: &Path) -> bool {
    use std::fs::File;

    let Ok(mut file) = File::open(path) else {
        return false;
    };

    // The identification header sits in the first page, well inside 4 KiB.
    let mut head = [0u8; 4096];
    let Ok(read) = file.read(&mut head) else {
        return false;
    };

    contains(&head[..read], b"OpusHead")
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("music-app-transcode-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn formats_rodio_handles_natively_are_left_alone() {
        assert!(!needs_transcode(Path::new("a.mp3")));
        assert!(!needs_transcode(Path::new("a.flac")));
        assert!(!needs_transcode(Path::new("a.wav")));
        assert!(!needs_transcode(Path::new("a.m4a")));
        assert!(!needs_transcode(Path::new("a.M4A")));
    }

    #[test]
    fn opus_and_matroska_always_need_ffmpeg() {
        assert!(needs_transcode(Path::new("a.opus")));
        assert!(needs_transcode(Path::new("a.OPUS")));
        assert!(needs_transcode(Path::new("a.webm")));
        assert!(needs_transcode(Path::new("a.mka")));
    }

    #[test]
    fn an_extensionless_file_is_left_for_rodio_to_attempt() {
        assert!(!needs_transcode(Path::new("mystery")));
    }

    /// The whole point of sniffing: two files with the same extension, only one
    /// of which rodio can decode.
    #[test]
    fn ogg_is_decided_by_its_contents_not_its_extension() {
        let dir = temp_dir("ogg-sniff");

        let opus = dir.join("opus.ogg");
        let mut page = vec![0u8; 28];
        page.splice(0..4, *b"OggS");
        page.extend_from_slice(b"OpusHead");
        page.extend_from_slice(&[0u8; 64]);
        std::fs::File::create(&opus).unwrap().write_all(&page).unwrap();

        let vorbis = dir.join("vorbis.ogg");
        let mut page = vec![0u8; 28];
        page.splice(0..4, *b"OggS");
        page.extend_from_slice(b"\x01vorbis");
        page.extend_from_slice(&[0u8; 64]);
        std::fs::File::create(&vorbis)
            .unwrap()
            .write_all(&page)
            .unwrap();

        assert!(needs_transcode(&opus), "Opus in Ogg needs ffmpeg");
        assert!(!needs_transcode(&vorbis), "Vorbis in Ogg is native");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_ogg_file_does_not_panic() {
        assert!(!needs_transcode(Path::new("nope.ogg")));
    }
}

#[cfg(test)]
mod ffmpeg_error_tests {
    use super::*;

    /// The failure that actually happens in practice, and the one where the
    /// raw text is least useful: a signed URL is ~2KB of query string.
    #[test]
    fn a_403_points_at_the_real_cause() {
        let stderr = "[in#0 @ 000001e3] Error opening input: Server returned 403 Forbidden \
                      (access denied)\r\nError opening input file https://rr2---sn-jvqxuxa.\
                      googlevideo.com/videoplayback?expire=1786700267&ei=i41&ip=2001";

        let message = explain_ffmpeg(stderr);

        assert!(message.contains("YouTube refused"), "got: {message}");
        assert!(
            !message.contains("googlevideo"),
            "the URL must not reach the user: {message}"
        );
    }

    #[test]
    fn network_failures_are_named() {
        assert!(explain_ffmpeg("Failed to resolve hostname").contains("internet connection"));
        assert!(explain_ffmpeg("Connection refused").contains("internet connection"));
    }

    #[test]
    fn a_missing_file_is_named() {
        assert!(explain_ffmpeg("x.opus: No such file or directory").contains("missing from disk"));
    }

    /// Anything unrecognised still has to be short enough for a toast.
    #[test]
    fn an_unknown_failure_is_truncated() {
        let stderr = "z".repeat(1000);
        let message = explain_ffmpeg(&stderr);

        assert!(message.len() < 260, "still too long: {} chars", message.len());
        assert!(message.ends_with('…'));
    }

    #[test]
    fn blank_stderr_still_says_something() {
        assert!(!explain_ffmpeg("").is_empty());
    }
}

#[cfg(test)]
mod transient_tests {
    use super::*;

    #[test]
    fn a_refused_fetch_is_worth_retrying() {
        assert!(is_transient(&explain_ffmpeg("Server returned 403 Forbidden")));
    }

    /// Retrying these would just waste the user's time.
    #[test]
    fn permanent_failures_are_not_retried() {
        assert!(!is_transient(&explain_ffmpeg("x.opus: No such file or directory")));
        assert!(!is_transient(&explain_ffmpeg("Invalid data found when processing input")));
    }
}
