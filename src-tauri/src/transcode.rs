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
/// The rate to decode at when the output device has not said what it wants.
///
/// Only a fallback. What matters is that ffmpeg's output rate *matches the
/// device*: rodio resamples anything that does not, and its converter is
/// documented as "simple linear interpolation ... may introduce audible
/// distortions". Measured against this library at 44.1 -> 48 kHz, that is
/// error at -33 dB relative to the music, which is about two percent
/// distortion on every track. ffmpeg's own resampler does the same job
/// properly, so the fix is to hand rodio a rate it does not have to touch.
pub const DEFAULT_OUTPUT_RATE: u32 = 44_100;
const OUTPUT_CHANNELS: u16 = 2;

/// Nominal, for sizing buffers only.
///
/// The real rate is whatever the device asked for, so a 48 kHz stream drains
/// this 9% faster than the name suggests -- a 0.46 second prefill rather than
/// 0.5. Not worth threading a rate through buffer arithmetic to correct.
const SAMPLES_PER_SECOND: usize = DEFAULT_OUTPUT_RATE as usize * OUTPUT_CHANNELS as usize;

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
    /// Whatever ffmpeg has written to stderr, still filling.
    ///
    /// Read at `Drop` rather than only on a failed start: ffmpeg can
    /// complain *and* keep producing audio, and that combination is
    /// exactly what leaves a damaged copy on disk.
    errors: Option<Arc<Mutex<String>>>,
    /// The thread filling `errors`.
    ///
    /// Joined before that text is read. ffmpeg exiting does not mean the
    /// last of its output has been collected yet, and reading a moment too
    /// early would make 'did ffmpeg complain' a race -- which, on the one
    /// run where it complained, is the answer that matters.
    stderr_drain: Option<std::thread::JoinHandle<()>>,
    /// The rate ffmpeg was told to produce, which is the device's own.
    ///
    /// Carried rather than assumed, because `Source::sample_rate` has to
    /// report the same number: rodio compares it against the mixer's and
    /// resamples if they differ, which is exactly what this avoids.
    output_rate: u32,
    /// Samples of silence emitted since real audio last flowed.
    ///
    /// Counted because the stream is interleaved: silence has to be inserted
    /// in whole frames or everything after it lands in the wrong channel.
    inserted: usize,
    /// A sample popped but not yet emitted, held while a frame is completed.
    held: Option<Sample>,
    /// A copy of ours being read, deleted if ffmpeg complains about it.
    ///
    /// Only ever set for [`FfmpegInput::Disposable`], so only ever a file
    /// in this app's own audio cache.
    disposable: Option<std::path::PathBuf>,
}

/// What ffmpeg should read.
///
/// The distinction matters because a network source needs reconnect options
/// that are meaningless -- and rejected -- for a file.
#[derive(Debug, Clone, Copy)]
pub enum FfmpegInput<'a> {
    File(&'a Path),
    /// A file the *app* wrote: a cache copy of a stream it already played.
    ///
    /// Decoded exactly like [`Self::File`]. The difference is that it is
    /// worth nothing and may be deleted -- which is why the distinction is
    /// in the type rather than in a flag someone could pass by accident.
    /// Nothing the user owns is ever named this way.
    Disposable(&'a Path),
    Url(&'a str),
}

impl FfmpegInput<'_> {
    /// How much decoded audio to hold ahead of the speakers.
    ///
    /// A network stream needs far more slack than a file: the buffer is
    /// exactly what a stall has to outlast before it becomes silence.
    fn buffer_samples(self) -> usize {
        let seconds = match self {
            FfmpegInput::File(_) | FfmpegInput::Disposable(_) => BUFFER_SECONDS_FILE,
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
        output_rate: u32,
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
        crate::sidecar::quiet(&mut command);
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
            FfmpegInput::File(path) | FfmpegInput::Disposable(path) => command.arg(path),
            FfmpegInput::Url(url) => command.arg(url),
        };

        command
            // Drop any cover art, then emit bare interleaved f32.
            .args(["-vn", "-f", "f32le", "-acodec", "pcm_f32le"])
            .args(["-ar", &output_rate.to_string()])
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
        let (errors, stderr_drain) = match stderr.map(spawn_stderr_drain) {
            Some((collected, handle)) => (Some(collected), handle),
            None => (None, None),
        };

        let source = Self {
            consumer,
            finished,
            starved,
            child,
            pending_cache: cache,
            // Kept rather than dropped after the checks below: whether ffmpeg
            // complained is the difference between a cache copy worth keeping
            // and one that will break the track later.
            errors: errors.clone(),
            stderr_drain,
            output_rate,
            inserted: 0,
            held: None,
            disposable: match input {
                FfmpegInput::Disposable(path) => Some(path.to_path_buf()),
                _ => None,
            },
        };

        source.wait_for_prefill()?;

        // Nothing arrived and ffmpeg is done: it rejected the input. Its stderr
        // is the only thing that explains why.
        if source.consumer.slots() == 0 && source.finished.load(Ordering::Acquire) {
            let detail = errors
                .and_then(|e| e.lock().ok().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "ffmpeg produced no audio.".to_string());

            let message = explain_ffmpeg(&detail);

            // The failure yt-dlp is structurally unable to report. It resolved
            // a URL and exited happy; the refusal happened afterwards, here,
            // to a different process. Playback is where an aged-out build is
            // first visible, so it is where the app has to notice.
            if is_transient(&message) {
                crate::updater::nudge(crate::updater::Trigger::Suspected);
            }

            return Err(message);
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

/// How much more silence is owed before real audio may resume.
///
/// The stream ffmpeg produces is interleaved: sample 0 is the left channel,
/// 1 the right, 2 the left again. An underrun inserts silence to keep the
/// track playing, and the number inserted is however long the stall lasted --
/// so half the time it is odd, and every sample afterwards is delivered to
/// the wrong channel for the rest of the track.
///
/// Rounding the insertion up to a whole frame costs at most one sample of
/// silence, 23 microseconds, and is the difference between a stall being
/// inaudible and permanently swapping the stereo image.
fn pad_to_frame(inserted: usize, channels: u16) -> usize {
    let channels = usize::from(channels);
    match inserted % channels {
        0 => 0,
        ragged => channels - ragged,
    }
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
fn spawn_stderr_drain(
    mut stderr: ChildStderr,
) -> (Arc<Mutex<String>>, Option<std::thread::JoinHandle<()>>) {
    let collected = Arc::new(Mutex::new(String::new()));
    let sink = collected.clone();

    let handle = std::thread::Builder::new()
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

    (collected, handle.ok())
}

impl Iterator for FfmpegSource {
    type Item = Sample;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        // Held back while the frame a stall interrupted was completed.
        if let Some(sample) = self.held.take() {
            return Some(sample);
        }

        match self.consumer.pop() {
            Ok(sample) => {
                // A relaxed load per sample is a plain memory read; the store
                // only happens on the edge out of starvation.
                if self.starved.load(Ordering::Relaxed) {
                    self.starved.store(false, Ordering::Relaxed);
                }

                // Audio is back, but a stall may have left the frame half
                // written. Finish it before letting this sample through, or
                // it plays out of the wrong speaker -- and so does every
                // sample after it.
                if pad_to_frame(self.inserted, OUTPUT_CHANNELS) > 0 {
                    self.inserted = 0;
                    self.held = Some(sample);
                    return Some(0.0);
                }

                self.inserted = 0;
                Some(sample)
            }
            Err(_) if self.finished.load(Ordering::Acquire) => None,
            // Underrun: ffmpeg has not kept up. A moment of silence beats
            // ending the track, which is what returning None would do -- but
            // the engine is told, so a long one can be reported as what it is
            // rather than played as silence forever.
            Err(_) => {
                self.starved.store(true, Ordering::Relaxed);
                self.inserted += 1;
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
        // Falls back rather than panicking: a zero here would abort the
        // audio thread, and playing at the wrong rate is recoverable.
        SampleRate::new(self.output_rate)
            .or_else(|| SampleRate::new(DEFAULT_OUTPUT_RATE))
            .expect("the fallback rate is a non-zero constant")
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

        // ffmpeg has exited, so its stderr pipe is closed and this returns
        // at once -- but it is what makes the check below a fact.
        if let Some(drain) = self.stderr_drain.take() {
            let _ = drain.join();
        }

        let complained = self
            .errors
            .as_ref()
            .and_then(|e| e.lock().ok().map(|s| !s.trim().is_empty()))
            .unwrap_or(false);
        // The same question the write side asks, asked about a copy being
        // *read*. A truncated one decodes happily until it reaches the
        // damage -- ffmpeg exits 0 and says only `File ended prematurely` --
        // so without this the song quietly ends early on every play, forever,
        // and nothing ever reconsiders the file.
        //
        // Deleting costs one re-fetch; the provider still has the track. It
        // is only ever reached for a copy this app wrote itself.
        if let Some(path) = self.disposable.take() {
            if complained {
                let _ = std::fs::remove_file(path);
            }
        }

        let Some(pending) = self.pending_cache.take() else {
            return;
        };


        if worth_caching(ended, status.is_some_and(|s| s.success()), complained) {
            pending.commit();
        } else {
            pending.discard();
        }
    }
}

/// Whether a decode produced a cache copy worth keeping.
///
/// All three conditions, because each covers a different way of ending up
/// with a file that decodes for two minutes and then turns to noise:
///
/// - `ended`: ffmpeg closed its own output. Anything else is a skip or a
///   seek, and the copy stops wherever the listener did.
/// - `exited_cleanly`: it was not killed, and did not fail.
/// - `complained`: **the one that was missing.** ffmpeg can lose part of an
///   HLS stream, say so on stderr, and still exit 0 having written most of
///   the track -- the copy is short and its last seconds are garbage. That
///   was ignored while audio was arriving, so the damaged copy was committed
///   and every later play of that track found it. At `-loglevel error`
///   ffmpeg is silent on a healthy run, so anything at all is a reason not
///   to keep what it produced.
///
/// A copy not kept costs one re-fetch. A bad one kept costs the track.
fn worth_caching(ended: bool, exited_cleanly: bool, complained: bool) -> bool {
    ended && exited_cleanly && !complained
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

    // A decoder that read the stream and refused it.
    //
    // Worth naming separately from the catch-all because it is the one failure
    // the app can *do* something about: the same audio in another encoding
    // usually plays, and `is_undecodable` is what routes it there.
    if lowered.contains("invalid data found")
        || lowered.contains("error while decoding")
        || lowered.contains("decoding for stream")
        || lowered.contains("exceeds limit")
        || lowered.contains("decoder not found")
    {
        return format!("{UNDECODABLE} ({})", codec_detail(stderr));
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

/// Said whenever a decoder read the audio and would not accept it.
pub const UNDECODABLE: &str = "That track's audio could not be decoded";

/// Whether the audio arrived and could not be turned into sound.
///
/// Distinct from every network case: the bytes are here, so resolving the same
/// link again is pointless and the useful retry is a *different encoding* of
/// the same track. The catch-all message counts too — an unrecognised ffmpeg
/// failure at this point is still ffmpeg failing to produce audio.
pub fn is_undecodable(message: &str) -> bool {
    message.starts_with(UNDECODABLE) || message.starts_with("Could not decode this audio")
}

/// Strips ffmpeg's `[codec @ 0x...]` prefix, keeping the codec name.
///
/// The pointer is a heap address inside a process that has already exited. It
/// is pure noise in a toast, and it is most of what makes ffmpeg's output look
/// like something went catastrophically wrong rather than "this file is odd".
fn codec_detail(stderr: &str) -> String {
    let first = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no detail");

    let cleaned = match first.strip_prefix('[').and_then(|rest| rest.split_once(']')) {
        Some((tag, rest)) => {
            let codec = tag.split_whitespace().next().unwrap_or(tag);
            format!("{codec}: {}", rest.trim())
        }
        None => first.to_string(),
    };

    cleaned.chars().take(120).collect()
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
    /// Builds a source with no ffmpeg behind it, so starvation can be staged.
    ///
    /// A real underrun needs a network stall at the exact moment the buffer
    /// empties, which is not something a test can arrange. Driving the ring
    /// buffer by hand is the only way to see what the iterator does when it
    /// runs dry -- and what it does when it runs dry is the whole question.
    fn staged_source(capacity: usize) -> (super::Producer<super::Sample>, super::FfmpegSource) {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;

        let (producer, consumer) = super::RingBuffer::<super::Sample>::new(capacity);

        // A process that has already exited. `FfmpegSource` only ever kills
        // and waits on this; it never reads from it.
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--staged-source-placeholder")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("a placeholder process");

        let source = super::FfmpegSource {
            consumer,
            finished: Arc::new(AtomicBool::new(false)),
            starved: Arc::new(AtomicBool::new(false)),
            child,
            pending_cache: None,
            errors: None,
            stderr_drain: None,
            output_rate: DEFAULT_OUTPUT_RATE,
            inserted: 0,
            held: None,
            disposable: None,
        };

        (producer, source)
    }

    #[test]
    fn silence_from_a_stall_never_swaps_the_channels() {
        let (mut producer, mut source) = staged_source(16);

        // One whole frame of real audio: left, then right.
        producer.push(1.0).unwrap();
        producer.push(2.0).unwrap();

        let mut heard = vec![source.next().unwrap(), source.next().unwrap()];

        // The stream stalls. One sample of silence goes out -- an odd number,
        // which is the case that used to break everything after it.
        heard.push(source.next().unwrap());

        // Audio returns. This sample belongs on the left, where it started.
        producer.push(3.0).unwrap();
        heard.push(source.next().unwrap());
        heard.push(source.next().unwrap());

        assert_eq!(heard, vec![1.0, 2.0, 0.0, 0.0, 3.0]);

        // The real test is the position, not the values: an interleaved stream
        // puts the left channel on even indices, and 3.0 began on the left.
        let resumed_at = heard.iter().position(|s| *s == 3.0).unwrap();
        assert_eq!(
            resumed_at % usize::from(super::OUTPUT_CHANNELS),
            0,
            "audio resumed mid-frame at index {resumed_at}, so it plays out of the wrong speaker",
        );
    }

    #[test]
    fn an_even_stall_is_left_exactly_as_it_is() {
        // Padding that is not needed would be a sample of silence added to
        // every underrun for no reason.
        assert_eq!(super::pad_to_frame(0, 2), 0);
        assert_eq!(super::pad_to_frame(2, 2), 0);
        assert_eq!(super::pad_to_frame(1, 2), 1);
        assert_eq!(super::pad_to_frame(3, 2), 1);
    }
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("music-app-transcode-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The whole point of the plumbing, tested against the real binary.
    ///
    /// rodio resamples any source whose rate differs from the mixer's, with a
    /// converter its own docs describe as simple linear interpolation. On this
    /// machine every output device runs at 48 kHz, so a decoder that still
    /// hands back 44.1 gets silently resampled -- measured at 33 dB below the
    /// music, roughly two percent distortion, on every track.
    ///
    /// Nothing about that failure is visible: no error, no glitch, just a
    /// quietly worse sound. So the number has to be asserted.
    #[test]
    fn the_decoder_produces_the_rate_it_was_asked_for() {
        let Some(ffmpeg) = crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg)
        else {
            eprintln!("skipped: no staged ffmpeg to run");
            return;
        };

        let dir = std::env::temp_dir().join("music-app-rate-probe");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let tone = dir.join("tone.wav");

        // Deliberately 44100, the rate the app used to hardcode: the test is
        // that the *device's* rate wins over both the source's and the old
        // constant.
        let mut generate = std::process::Command::new(&ffmpeg);
        crate::sidecar::quiet(&mut generate);
        let made = generate
            .args(["-hide_banner", "-nostats", "-y"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=2:sample_rate=44100"])
            .arg(&tone)
            .status()
            .expect("ffmpeg should run");
        assert!(made.success(), "could not generate the test tone");

        for asked in [48_000_u32, 44_100] {
            let source = FfmpegSource::open_at(
                &ffmpeg,
                asked,
                FfmpegInput::File(&tone),
                Duration::ZERO,
                None,
            )
            .expect("a plain wav should decode");

            assert_eq!(
                source.sample_rate().get(),
                asked,
                "decoder reported {} when the device asked for {asked}",
                source.sample_rate().get(),
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
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

    /// The bug this whole fallback exists for, as the tester saw it.
    /// A healthy run is the only one worth keeping.
    #[test]
    fn a_clean_complete_silent_run_is_cached() {
        assert!(worth_caching(true, true, false));
    }

    /// The case that poisoned a track for good: ffmpeg lost part of an HLS
    /// stream, said so, and still exited 0 having written most of it. The
    /// copy decoded for two minutes and then turned to noise, and every
    /// later play found it again.
    #[test]
    fn a_run_ffmpeg_complained_about_is_not_cached() {
        assert!(
            !worth_caching(true, true, true),
            "a copy ffmpeg complained about must not outlive the play",
        );
    }

    /// Skips and seeks stop the copy wherever the listener stopped.
    #[test]
    fn an_interrupted_run_is_not_cached() {
        assert!(!worth_caching(false, true, false), "killed part-way");
        assert!(!worth_caching(true, false, false), "exited badly");
        assert!(!worth_caching(false, false, true), "all three wrong");
    }

    #[test]
    fn a_decoder_refusing_a_stream_is_named_and_routed_to_a_retry() {
        let raw = "[aac @ 000001dda6bf5800] Number of bands (49) exceeds limit (32).";
        let message = explain_ffmpeg(raw);

        assert!(
            is_undecodable(&message),
            "a decoder failure must retry a different encoding: {message:?}",
        );
        assert!(
            !message.contains("000001dda6bf5800"),
            "the heap pointer is noise and must not reach a toast: {message:?}",
        );
        assert!(
            message.contains("aac"),
            "the codec is worth keeping: {message:?}",
        );
    }

    /// Network failures must *not* be routed to the encoding retry: the same
    /// encoding over a working connection is exactly what is wanted.
    #[test]
    fn a_network_failure_is_not_treated_as_undecodable() {
        for raw in [
            "HTTP error 403 Forbidden",
            "Server returned 404 Not Found",
            "Failed to resolve hostname",
        ] {
            let message = explain_ffmpeg(raw);
            assert!(
                !is_undecodable(&message),
                "{raw:?} became {message:?}, which would retry the wrong thing",
            );
        }
    }

    /// An unrecognised ffmpeg failure at this point is still ffmpeg failing
    /// to produce audio, so it earns the same retry.
    #[test]
    fn an_unrecognised_failure_still_earns_the_encoding_retry() {
        assert!(is_undecodable(&explain_ffmpeg("something nobody has seen")));
    }

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
