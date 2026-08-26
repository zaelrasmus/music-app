use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::stream::DeviceSinkBuilder;
use rodio::{Player, Source};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::equalizer::{EqSettings, Equalised};
use crate::playable::PlayableSource;
use crate::transcode::{FfmpegInput, FfmpegSource};

/// How long a decoder must be starved before it is reported as stalled.
///
/// Long enough to ride out the ordinary hiccup the buffer exists to absorb --
/// reporting every momentary underrun would make the UI flicker for something
/// nobody hears.
const STALL_REPORT_AFTER: Duration = Duration::from_secs(1);

/// How often the thread wakes to notice a track finished on its own, and so
/// the upper bound on how late `Finished` can be.
///
/// Short, because with the next track prepared in advance this *is* the gap
/// between tracks -- everything else in the handover is already done by the
/// time the first one ends.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Progress is reported every this many polls.
///
/// Decoupled from the poll rate on purpose: noticing the end quickly is worth
/// waking often, but telling the UI twenty times a second would re-render it
/// for movement no one can see.
const PROGRESS_EVERY: u32 = 4;

/// Ceiling on how long a caller waits for the thread to answer.
///
/// Must exceed `transcode::PREFILL_TIMEOUT`: opening a stream legitimately
/// blocks the thread for that long, and a shorter ceiling here would report
/// "the audio thread did not respond" for a track that then starts playing
/// perfectly well.
const REPLY_TIMEOUT: Duration = Duration::from_secs(20);

// Enforced rather than trusted to a comment: shortening one of these without
// the other reintroduces exactly that bug.
const _: () = assert!(
    REPLY_TIMEOUT.as_secs() > crate::transcode::PREFILL_TIMEOUT.as_secs(),
    "a caller must not give up before a stream is allowed to start"
);

/// Returned when a load lost the race to a newer one.
///
/// A sentinel rather than a variant because it crosses the same
/// `Result<(), String>` channel every other engine failure uses, and it is the
/// one "failure" the coordinator must not show anyone or skip a track over.
pub const SUPERSEDED: &str = "__superseded__";

/// What the engine reports upwards. It never decides anything -- the
/// coordinator owns every policy question.
#[derive(Debug)]
pub enum EngineEvent {
    /// The track for `epoch` played to its end. Not sent on an explicit stop.
    Finished { epoch: u64 },
    /// Where the current track is up to. Emitted on the existing poll tick
    /// while actually playing -- never while paused or stopped, so the UI
    /// stops updating on its own without needing to be told.
    Progress { epoch: u64, position: Duration },
    /// The decoder has run dry without ending, or has recovered.
    ///
    /// Repeated roughly once a second while it lasts, so the coordinator can
    /// time it without running a clock of its own.
    Stalled { epoch: u64, stalled: bool },
}

enum Command {
    Play {
        source: PlayableSource,
        /// A decoder already built and buffering.
        ///
        /// Building one spawns ffmpeg and waits for it to produce half a
        /// second of audio -- seconds of work that must not happen on this
        /// thread while a track is playing. When the coordinator has prepared
        /// the next track ahead of time it arrives here ready to append, which
        /// is what makes the handover between tracks quick.
        decoded: Option<BuiltSource>,
        /// Where in the track to begin.
        ///
        /// Carried into the decode rather than applied as a seek afterwards:
        /// for a stream, seeking means restarting ffmpeg, so resuming at a
        /// position would otherwise start the process twice.
        start_at: Duration,
        /// Echoed back in `Finished`, so the coordinator can discard a report
        /// about a track it has already moved on from.
        ///
        /// Also orders the plays themselves: loads now run concurrently, so
        /// two can be in flight at once and the *older* one must not win by
        /// finishing last.
        epoch: u64,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Pause,
    Resume,
    Stop {
        /// Bumped past every load in flight, so none of them can start audio
        /// after the user has already stopped.
        epoch: u64,
    },
    /// Linear amplitude, already curved by the caller.
    SetVolume(f32),
    Position {
        reply: oneshot::Sender<Duration>,
    },
    Seek {
        position: Duration,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

/// Handle to the audio thread.
///
/// The thread owns the output device for the life of the process:
/// `MixerDeviceSink` holds a `cpal::Stream`, which is `Send + Sync` on
/// Windows/WASAPI but has no such impl on macOS/CoreAudio, so putting it in
/// shared state would compile here and fail there. Dropping it also silently
/// stops all audio, so one long-lived owner is the only arrangement that
/// behaves.
pub struct AudioEngine {
    tx: Mutex<Sender<Command>>,
    /// Live equaliser settings, shared with whatever is playing.
    ///
    /// Not routed through the command channel: every field is an atomic, so
    /// the UI writes and the audio thread reads with nothing in between. A
    /// slider drag therefore costs no message, no lock and no decode restart.
    eq: Arc<EqSettings>,
    /// The rate the output device actually runs at.
    ///
    /// Published by the audio thread because only it may touch the device,
    /// and read from elsewhere because the *decoder* has to match it: rodio
    /// resamples anything that does not, with a converter its own docs call
    /// "simple linear interpolation" that measures at -33 dB of added
    /// distortion. Handing it a matching rate makes it a pass-through.
    ///
    /// Starts at the fallback and is corrected within milliseconds of
    /// startup. A decode that somehow beat the device open would be
    /// resampled as before -- the old behaviour, not a new failure.
    output_rate: Arc<AtomicU32>,
}

impl AudioEngine {
    /// What ffmpeg should be told to produce.
    pub fn output_rate(&self) -> u32 {
        self.output_rate.load(Ordering::Acquire)
    }

    /// The live equaliser settings.
    ///
    /// Handed out rather than wrapped in commands because writing to it is
    /// already safe from any thread, and because a slider drag would otherwise
    /// put a message on the audio thread's channel for every pixel.
    pub fn equaliser(&self) -> &Arc<EqSettings> {
        &self.eq
    }


    fn send(&self, command: Command) -> Result<(), String> {
        self.tx
            .lock()
            .map_err(|_| "Audio channel is poisoned.".to_string())?
            .send(command)
            .map_err(|_| "The audio thread is not running.".to_string())
    }

    /// Async rather than blocking: opening a stream legitimately takes
    /// seconds, and a blocking wait here would park a runtime worker for the
    /// whole of it.
    pub async fn play(
        &self,
        source: PlayableSource,
        decoded: Option<BuiltSource>,
        start_at: Duration,
        epoch: u64,
    ) -> Result<(), String> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Play {
            source,
            decoded,
            start_at,
            epoch,
            reply,
        })?;

        await_reply(response, "The audio thread did not respond.").await
    }

    pub fn pause(&self) -> Result<(), String> {
        self.send(Command::Pause)
    }

    pub fn resume(&self) -> Result<(), String> {
        self.send(Command::Resume)
    }

    pub fn stop(&self, epoch: u64) -> Result<(), String> {
        self.send(Command::Stop { epoch })
    }

    pub fn set_volume(&self, linear: f32) -> Result<(), String> {
        self.send(Command::SetVolume(linear))
    }

    /// Jumps to `position` in the current track.
    ///
    /// `try_seek` blocks until the audio callback acknowledges, which is why
    /// it has to happen on the audio thread rather than in a command handler.
    /// It is also genuinely fallible -- some sources cannot seek at all -- so
    /// the error is propagated rather than swallowed.
    pub async fn seek(&self, position: Duration) -> Result<(), String> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Seek { position, reply })?;

        await_reply(response, "The audio thread did not respond to the seek.").await
    }

    /// Position within the current track. Zero when nothing is playing.
    pub async fn position(&self) -> Duration {
        let (reply, response) = oneshot::channel();
        if self.send(Command::Position { reply }).is_err() {
            return Duration::ZERO;
        }
        tokio::time::timeout(REPLY_TIMEOUT, response)
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(Duration::ZERO)
    }
}

/// Waits for the audio thread, turning both a timeout and a dropped sender
/// into the same plain message.
async fn await_reply(
    response: oneshot::Receiver<Result<(), String>>,
    on_silence: &str,
) -> Result<(), String> {
    match tokio::time::timeout(REPLY_TIMEOUT, response).await {
        Ok(Ok(result)) => result,
        _ => Err(on_silence.to_string()),
    }
}

pub fn spawn(events: UnboundedSender<EngineEvent>, ffmpeg: Option<PathBuf>) -> AudioEngine {
    let (tx, rx) = mpsc::channel();
    let output_rate = Arc::new(AtomicU32::new(crate::transcode::DEFAULT_OUTPUT_RATE));
    let published = Arc::clone(&output_rate);

    let eq = Arc::new(EqSettings::default());
    let audio_eq = Arc::clone(&eq);

    std::thread::Builder::new()
        .name("audio".to_string())
        .spawn(move || run(rx, events, ffmpeg, published, audio_eq))
        .expect("audio thread should spawn");

    AudioEngine {
        tx: Mutex::new(tx),
        output_rate,
        eq,
    }
}

/// The live audio settings a decode is built and played against.
///
/// One struct rather than three parameters threaded side by side. They are
/// always passed together, always cloned together, and every new audio setting
/// would otherwise add another argument to the same two functions -- `seek`
/// had reached eight before this existed.
struct Chain {
    /// Read per frame by `Levelled`, so the slider moves what is playing.
    volume: Arc<AtomicU32>,
    /// Read per frame by `Equalised`, for the same reason.
    eq: Arc<EqSettings>,
}

fn run(
    rx: Receiver<Command>,
    events: UnboundedSender<EngineEvent>,
    ffmpeg: Option<PathBuf>,
    output_rate: Arc<AtomicU32>,
    eq: Arc<EqSettings>,
) {
    // Opened once. If there is no device we keep the error and fail every play
    // with it, rather than killing the thread and making every later command
    // report "not running".
    let device = match DeviceSinkBuilder::open_default_sink() {
        Ok(mut sink) => {
            sink.log_on_drop(false);
            // Told to everyone who builds a decoder, before anything can.
            output_rate.store(sink.config().sample_rate().get(), Ordering::Release);
            Ok(sink)
        }
        Err(e) => Err(format!("No audio output device: {e}")),
    };

    let mut player: Option<Player> = None;
    let mut epoch: Option<u64> = None;
    // One cell, shared with whatever is playing. Volume used to live on
    // rodio's `Player`, which is rebuilt per track and so had to be re-applied
    // on every start; sharing it means a track picks up the current volume by
    // construction, and moving the slider is heard immediately.

    let chain = Chain {
        volume: Arc::new(AtomicU32::new(1.0f32.to_bits())),
        eq,
    };
    // What is loaded, kept so a seek can rebuild the decode from a new offset.
    let mut loaded: Option<Loaded> = None;
    // The newest epoch this thread has acted on.
    //
    // Loads run concurrently now, so a slow one can finish after a newer one
    // has already started playing. Without this the older audio would simply
    // overwrite the newer -- the user presses Next, waits, and then hears the
    // track they skipped.
    let mut highest_epoch: u64 = 0;
    let mut ticks: u32 = 0;
    // When the current decoder began starving, and whether that has been
    // reported yet.
    let mut starving_since: Option<std::time::Instant> = None;
    let mut reported_stall = false;

    loop {
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(Command::Play {
                source,
                decoded,
                start_at,
                epoch: new_epoch,
                reply,
            }) => {
                if new_epoch < highest_epoch {
                    // Superseded while it was still resolving. Reported as an
                    // error the coordinator recognises and discards, rather
                    // than silently succeeding and starting the wrong audio.
                    let _ = reply.send(Err(SUPERSEDED.to_string()));
                    continue;
                }
                highest_epoch = new_epoch;

                let mut starved = None;
                let result = match &device {
                    Ok(sink) => match decoded {
                        // Prepared ahead of time: nothing left to do but hand
                        // it to the mixer.
                        Some(ready) => {
                            starved = ready.starved;
                            Ok(start(sink, ready.decoded, &chain))
                        }
                        None => build_source(
                            &source,
                            ffmpeg.as_deref(),
                            output_rate.load(Ordering::Acquire),
                            start_at,
                        )
                        .map(|built| {
                            starved = built.starved;
                            start(sink, built.decoded, &chain)
                        }),
                    },
                    Err(e) => Err(e.clone()),
                };

                match result {
                    Ok(new_player) => {
                        // Dropping the previous player marks it stopped, which
                        // is what makes "replace what is playing" work --
                        // `append` alone would queue the new track behind it.
                        player = Some(new_player);
                        epoch = Some(new_epoch);
                        loaded = Some(Loaded {
                            source,
                            starved,
                            // The decode began here, so this is what the
                            // player's own position has to be added to.
                            offset: start_at,
                        });
                        let _ = reply.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }

            Ok(Command::Pause) => {
                if let Some(p) = &player {
                    p.pause();
                }
            }

            Ok(Command::Resume) => {
                if let Some(p) = &player {
                    p.play();
                }
            }

            Ok(Command::Stop { epoch: new_epoch }) => {
                // Clear the epoch before dropping so end-detection below cannot
                // mistake a deliberate stop for a track finishing.
                epoch = None;
                player = None;
                loaded = None;
                // Anything still resolving is now stale.
                highest_epoch = highest_epoch.max(new_epoch);
            }

            Ok(Command::SetVolume(linear)) => {
                // Nothing else to do: the source reads this cell per sample, so
                // the track already playing follows the slider on its own.
                chain.volume.store(linear.to_bits(), Ordering::Relaxed);
            }

            Ok(Command::Position { reply }) => {
                let _ = reply.send(position_of(&player, &loaded));
            }

            Ok(Command::Seek { position, reply }) => {
                let result = seek(
                    device.as_ref(),
                    &mut player,
                    &mut loaded,
                    position,
                    &chain,
                    ffmpeg.as_deref(),
                );
                let _ = reply.send(result);
            }

            Err(RecvTimeoutError::Timeout) => {
                // `append` bumps the queue count synchronously, so an empty
                // player here means the track genuinely reached its end.
                if let (Some(p), Some(current)) = (&player, epoch) {
                    if p.empty() {
                        player = None;
                        epoch = None;
                        let _ = events.send(EngineEvent::Finished { epoch: current });
                    } else if !p.is_paused() {
                        ticks = ticks.wrapping_add(1);

                        let starving = loaded
                            .as_ref()
                            .and_then(|l| l.starved.as_ref())
                            .is_some_and(|flag| flag.load(Ordering::Relaxed));

                        if starving {
                            let since = *starving_since.get_or_insert_with(std::time::Instant::now);

                            // Repeated while it lasts, so the coordinator can
                            // decide when enough is enough without a clock.
                            if since.elapsed() >= STALL_REPORT_AFTER
                                && (!reported_stall || ticks % PROGRESS_EVERY == 0)
                            {
                                reported_stall = true;
                                let _ = events.send(EngineEvent::Stalled {
                                    epoch: current,
                                    stalled: true,
                                });
                            }
                        } else {
                            if reported_stall {
                                let _ = events.send(EngineEvent::Stalled {
                                    epoch: current,
                                    stalled: false,
                                });
                            }
                            starving_since = None;
                            reported_stall = false;
                        }

                        // Silence is not progress: leaving the bar moving
                        // while nothing is arriving is exactly the lie this
                        // whole path exists to stop telling.
                        if ticks % PROGRESS_EVERY == 0 && !starving {
                            let _ = events.send(EngineEvent::Progress {
                                epoch: current,
                                position: position_of(&player, &loaded),
                            });
                        }
                    }
                }
            }

            // The handle was dropped: the app is shutting down.
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// What the thread is currently playing, retained so a seek can rebuild it.
struct Loaded {
    source: PlayableSource,
    /// Set while this decoder is starving. `None` when it cannot.
    starved: Option<Arc<AtomicBool>>,
    /// Where the current decode was started from.
    ///
    /// A restarted ffmpeg reports its own position from zero, so this is what
    /// keeps the position the *track's*, not the process's.
    offset: Duration,
}

/// Position within the track, as opposed to within the current decode.
fn position_of(player: &Option<Player>, loaded: &Option<Loaded>) -> Duration {
    let within = player.as_ref().map_or(Duration::ZERO, |p| p.get_pos());
    let offset = loaded.as_ref().map_or(Duration::ZERO, |l| l.offset);
    offset + within
}


/// Jumps to `position`, restarting the decoder when the source cannot seek.
///
/// Nothing seeks in place any more: `FfmpegSource` reads a pipe, and rodio
/// would drive `try_seek` from the audio callback thread, where spawning a
/// process is not allowed. So every seek restarts the decode here instead, on
/// the engine thread, against the source already in hand.
///
/// For a local file that costs a process: measured at **52 ms mean, 88 ms
/// worst** to first audio, and flat with position -- `-ss` goes *before* `-i`,
/// so ffmpeg uses the container index rather than decoding and discarding up
/// to the target. Seeking to 3:26 costs the same as seeking to 0:05. That is
/// the price of one decoder instead of two; symphonia repositioned an open
/// decoder in under a millisecond.
///
/// For a stream it costs the same restart it always did -- reusing the
/// resolved URL rather than re-resolving through yt-dlp, which would add
/// seconds instead of about 0.4s for YouTube and 2s for SoundCloud's HLS.
fn seek(
    device: Result<&rodio::MixerDeviceSink, &String>,
    player: &mut Option<Player>,
    loaded: &mut Option<Loaded>,
    position: Duration,
    chain: &Chain,
    ffmpeg: Option<&Path>,
) -> Result<(), String> {
    if player.is_none() {
        return Err("Nothing is playing.".to_string());
    }

    // Native seek first: a local file rewinds in place with no process work,
    // and this is the overwhelmingly common case.
    if let Some(p) = player.as_ref() {
        if p.try_seek(position).is_ok() {
            if let Some(l) = loaded.as_mut() {
                l.offset = Duration::ZERO;
            }
            return Ok(());
        }
    }

    // Not seekable in place, so rebuild the decode from the new offset.
    let Some(l) = loaded.as_mut() else {
        return Err("Nothing is playing.".to_string());
    };
    let sink = device.map_err(|e| e.clone())?;

    // Pause rather than drop: if the rebuild fails, the track is still loaded
    // and can simply resume. Dropping first would turn a failed seek into
    // silence with nothing to recover.
    let was_paused = player.as_ref().is_some_and(|p| p.is_paused());
    if let Some(p) = player.as_ref() {
        p.pause();
    }

    let rebuilt = build_source(&l.source, ffmpeg, sink.config().sample_rate().get(), position)
        .map(|built| {
        let starved = built.starved;
        (start(sink, built.decoded, chain), starved)
    });

    match rebuilt {
        Ok((replacement, starved)) => {
            // Seeking while paused must not start playback.
            if was_paused {
                replacement.pause();
            }
            // Assigning drops the old player, so the two are never audible at
            // once -- the old one has been silent since the pause above.
            *player = Some(replacement);
            l.offset = position;
            l.starved = starved;
            Ok(())
        }
        Err(e) => {
            if !was_paused {
                if let Some(p) = player.as_ref() {
                    p.play();
                }
            }
            Err(e)
        }
    }
}

/// A decoder, plus what the engine needs to know about it once it has been
/// boxed into a trait object.
///
/// The boxing is what makes preparing a track ahead of time possible, but it
/// also erases everything specific to the decoder -- including whether it can
/// tell us it is starving. This carries that back out.
pub struct BuiltSource {
    pub decoded: Box<dyn Source + Send>,
    /// `None` for a source that cannot meaningfully starve.
    ///
    /// Nothing produces `None` today -- every source is an `FfmpegSource`
    /// reading a pipe, and a pipe can always run dry. Kept as an `Option`
    /// because "this source cannot starve" is a real thing for a decoder to
    /// say, and collapsing it now would have to be undone by the next one.
    pub starved: Option<Arc<AtomicBool>>,
}

/// The highest sample this stage will ever emit.
///
/// Full scale, because the device hard-clips anything above it and there is no
/// reason to give away level that costs nothing to keep.
const CEILING: f32 = 1.0;

/// How far ahead the limiter looks before deciding on a gain.
///
/// This is the whole reason it can be transparent. Knowing a peak is coming
/// lets the gain be walked down *before* it arrives, so the waveform is scaled
/// rather than reshaped. Without it the only options are to reshape the sample
/// in place -- which is what the old `tanh` waveshaper did, and what measured
/// 0.88% THD at 0 dBFS rising to 13.8% at +3.17 dBFS -- or to let the device
/// chop it off.
///
/// 1.5 ms is short enough that nobody notices the delay and long enough to
/// cover a transient's rise. It costs 72 frames of latency at 48 kHz.
const LOOKAHEAD: Duration = Duration::from_micros(1500);

/// How quickly the gain returns to unity once the loud passage is over.
///
/// Slow enough not to be heard as pumping on the tail of a note, fast enough
/// that one transient does not duck the whole bar after it.
const RELEASE: Duration = Duration::from_millis(80);

/// A stereo-linked look-ahead peak limiter.
///
/// It replaced a memoryless waveshaper, and the distinction is the point: any
/// nonlinearity applied sample-by-sample generates harmonics, however smooth
/// its curve is. Smoothness only sets how fast the harmonic series rolls off;
/// it does not stop the series existing. Measured on this library, the
/// waveshaper's error was steeply treble-tilted -- around -23 dB relative to
/// the music at 10-16 kHz, and on a 16 kHz-brickwalled m4a it left the top
/// band *louder* than the source, entirely manufactured. That is what "harsh"
/// sounds like.
///
/// A gain envelope has no such problem. The samples are only ever multiplied
/// by a number that changes slowly, so nothing is created that was not
/// already there; what is left is level modulation, which the ear reads as
/// loudness moving rather than as grit.
///
/// Stereo-linked -- one gain for the whole frame -- because giving each
/// channel its own would move the stereo image every time one side peaked.
struct Limiter {
    channels: usize,
    /// Frames of lookahead. The delay line holds this many frames plus the one
    /// being emitted.
    look: usize,
    /// Delayed samples, interleaved.
    delayed: std::collections::VecDeque<rodio::Sample>,
    /// Peak of each frame in `delayed`, so the window maximum does not have to
    /// re-scan the samples themselves.
    peaks: std::collections::VecDeque<f32>,
    /// Current gain reduction, smoothed.
    gain: f32,
    attack: f32,
    release: f32,
    /// Frames ready to hand out.
    out: std::collections::VecDeque<rodio::Sample>,
    ended: bool,
}

impl Limiter {
    fn new(channels: usize, sample_rate: u32) -> Self {
        let rate = sample_rate.max(1) as f32;
        let look = ((rate * LOOKAHEAD.as_secs_f32()) as usize).max(1);
        Self {
            channels: channels.max(1),
            look,
            delayed: std::collections::VecDeque::with_capacity((look + 1) * channels.max(1)),
            peaks: std::collections::VecDeque::with_capacity(look + 1),
            gain: 1.0,
            // Reaching the target within the lookahead is what keeps the
            // guarantee below from ever having to bite hard.
            attack: (-4.0 / look as f32).exp(),
            release: (-1.0 / (rate * RELEASE.as_secs_f32())).exp(),
            out: std::collections::VecDeque::with_capacity(channels.max(1) * 2),
            ended: false,
        }
    }

    fn reset(&mut self) {
        self.delayed.clear();
        self.peaks.clear();
        self.out.clear();
        self.gain = 1.0;
        self.ended = false;
    }

    fn push_frame(&mut self, frame: &[rodio::Sample]) {
        let mut peak = 0.0f32;
        for s in frame {
            peak = peak.max(s.abs());
            self.delayed.push_back(*s);
        }
        self.peaks.push_back(peak);
    }

    /// Moves the oldest frame out, scaled by the gain the lookahead chose.
    fn emit_frame(&mut self) {
        // The window is [this frame, this frame + lookahead], which is exactly
        // what `peaks` holds. A linear scan is cheaper than it looks: `look` is
        // 72 at 48 kHz, so this is about 3.5 million comparisons a second,
        // against a budget of billions.
        let window_max = self.peaks.iter().fold(0.0f32, |a, b| a.max(*b));
        let target = if window_max > CEILING {
            CEILING / window_max
        } else {
            1.0
        };

        let coefficient = if target < self.gain {
            self.attack
        } else {
            self.release
        };
        self.gain = target + (self.gain - target) * coefficient;

        // The guarantee. Smoothing alone gets close but is asymptotic, so on a
        // steep enough transient the envelope could still be a fraction above
        // where it needs to be -- and a fraction above full scale is exactly
        // the corner this exists to avoid. Clamping against the frame actually
        // being emitted makes the ceiling absolute rather than approximate.
        let here = self.peaks.pop_front().unwrap_or(0.0);
        let applied = if here > 0.0 {
            self.gain.min(CEILING / here)
        } else {
            self.gain
        };

        for _ in 0..self.channels {
            let sample = self.delayed.pop_front().unwrap_or(0.0);
            self.out.push_back(sample * applied);
        }
    }
}

/// Applies the volume, then limits what would clip, in that order.
///
/// The gain lives here rather than on rodio's `Player` because limiting has
/// to happen *after* it: rodio multiplies on the way out of this source, so
/// a limiter inside would be guarding a number that is about to change. The
/// player's own volume is left at unity and this owns the whole job.
///
/// The gain is shared rather than copied so that moving the slider changes
/// what is already playing. Read once per frame -- not per sample, which used
/// to let the two channels of one frame be scaled by different numbers if the
/// slider moved between them.
struct Levelled<S> {
    inner: S,
    gain: Arc<AtomicU32>,
    limiter: Limiter,
}

impl<S> Levelled<S>
where
    S: Source,
{
    fn new(inner: S, gain: Arc<AtomicU32>) -> Self {
        let limiter = Limiter::new(inner.channels().get() as usize, inner.sample_rate().get());
        Self {
            inner,
            gain,
            limiter,
        }
    }

    /// Pulls one whole frame, already scaled by the volume.
    ///
    /// A short read at the end of a track is padded rather than dropped: the
    /// stream is interleaved, so half a frame would put every sample after it
    /// in the wrong channel.
    fn pull_frame(&mut self) -> Option<Vec<rodio::Sample>> {
        let gain = f32::from_bits(self.gain.load(Ordering::Relaxed));
        let channels = self.limiter.channels;
        let mut frame = Vec::with_capacity(channels);
        for index in 0..channels {
            match self.inner.next() {
                Some(sample) => frame.push(sample * gain),
                None if index == 0 => return None,
                None => frame.push(0.0),
            }
        }
        Some(frame)
    }
}

impl<S> Iterator for Levelled<S>
where
    S: Source,
{
    type Item = rodio::Sample;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(sample) = self.limiter.out.pop_front() {
                return Some(sample);
            }

            // Fill the lookahead before anything may leave.
            while !self.limiter.ended && self.limiter.peaks.len() <= self.limiter.look {
                match self.pull_frame() {
                    Some(frame) => self.limiter.push_frame(&frame),
                    None => self.limiter.ended = true,
                }
            }

            if self.limiter.peaks.is_empty() {
                return None;
            }
            self.limiter.emit_frame();
        }
    }
}

impl<S> Source for Levelled<S>
where
    S: Source,
{
    fn current_span_len(&self) -> Option<usize> {
        // Not forwarded: the delay line means what leaves here no longer lines
        // up with the input's span boundaries, and a wrong answer is worse than
        // none. rodio treats `None` as "ask again", which is correct.
        None
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), rodio::source::SeekError> {
        let sought = self.inner.try_seek(position);
        if sought.is_ok() {
            // Whatever is in the delay line belongs to where we just left.
            self.limiter.reset();
        }
        sought
    }
}

fn start(
    sink: &rodio::MixerDeviceSink,
    decoded: Box<dyn Source + Send>,
    chain: &Chain,
) -> Player {
    // Appended *before* the queue reaches the mixer, which is not fussiness.
    //
    // `Mixer::add` wraps whatever it is handed in a `UniformSourceIterator`,
    // and that reads the source's channel count and sample rate **once, at
    // that moment**. `Player::connect_new` adds the queue while it is still
    // empty, and an empty queue reports its placeholder `Empty` source: 48 kHz,
    // but **one channel**. So the mixer builds a mono-to-stereo converter and
    // then runs real stereo audio through it for the whole track.
    //
    // Measured on a 48 kHz file against the same audio tapped before the mixer:
    // wrapping first costs a 256-sample delay, a flat -60 dB error across every
    // audible band, and +43 dB in the 22-24 kHz band. Appending first is
    // bit-exact -- -223 dB, the measurement floor.
    //
    // `connect_new` is exactly `new()` followed by `mixer.add()`, so doing the
    // halves in the other order costs nothing.
    let (player, queue) = Player::new();
    // Left at unity deliberately: `Levelled` owns the gain, because the
    // limiter has to see the sample the device will actually receive.
    player.append(Levelled::new(
        Equalised::new(decoded, Arc::clone(&chain.eq)),
        Arc::clone(&chain.volume),
    ));
    sink.mixer().add(queue);
    player
}

/// Builds the decoder for a source, ready to be appended.
///
/// Deliberately separate from [`start`], and public, because this is the
/// expensive half: for anything ffmpeg handles it spawns a process and waits
/// for the first half second of audio. Splitting it lets the coordinator do
/// that work early, on another thread, for a track that has not started yet.
/// Builds a decoder for one source.
///
/// `output_rate` is the *device's* rate, not a preference. rodio resamples any
/// source whose rate differs from the mixer's, using a converter its own
/// documentation calls "simple linear interpolation" -- measured on this
/// library at 44.1 -> 48 kHz, that is error only 33 dB below the music. Asking
/// ffmpeg for the device rate makes rodio's converter a pass-through and hands
/// the job to a resampler that does it properly.
///
/// Every source goes through ffmpeg, so this holds unconditionally rather than
/// only for the sources rodio could not decode. That is the point of having
/// one decoder: the rate handed to the mixer is no longer a property of which
/// path a file happened to take.
pub fn build_source(
    source: &PlayableSource,
    ffmpeg: Option<&Path>,
    output_rate: u32,
    start_at: Duration,
) -> Result<BuiltSource, String> {
    match source {
        PlayableSource::LocalFile(path) => {
            // One decoder, for every format and every rate.
            //
            // rodio used to decode this natively, which meant its resampler ran
            // whenever a file's rate differed from the device's -- linear
            // interpolation, measured at error only 12 dB below the music
            // across 10-16 kHz with the top octave 21 to 35 dB too loud. The
            // old code dodged that by using ffmpeg *only* on a rate mismatch,
            // which left two decode paths that seeked differently, disagreed
            // about Opus, and could not both carry an audio filter.
            //
            // ffmpeg resamples correctly and always decodes straight to the
            // device rate, so the mismatch it was working around cannot occur.
            let ffmpeg = ffmpeg.ok_or(crate::sidecar::NO_DECODER)?;
            let source =
                FfmpegSource::open_at(ffmpeg, output_rate, FfmpegInput::File(path), start_at, None)?;
            Ok(BuiltSource {
                starved: Some(source.starvation_flag()),
                decoded: Box::new(source),
            })
        }

        PlayableSource::Cached(path) => {
            let ffmpeg = ffmpeg.ok_or(crate::sidecar::NO_DECODER)?;
            // Disposable: a copy this app made of a stream it already
            // played. If ffmpeg complains while reading it back, it is
            // thrown away rather than quietly ending the song early on
            // every future play.
            let source = FfmpegSource::open_at(
                ffmpeg,
                output_rate,
                FfmpegInput::Disposable(path),
                start_at,
                None,
            )?;
            Ok(BuiltSource {
                starved: Some(source.starvation_flag()),
                decoded: Box::new(source),
            })
        }

        PlayableSource::Stream { url, cache } => {
            let ffmpeg = ffmpeg.ok_or(crate::sidecar::NO_DECODER)?;
            let source = FfmpegSource::open_at(
                ffmpeg,
                output_rate,
                FfmpegInput::Url(url),
                start_at,
                // A seeked decode begins partway in, so what it would write is
                // not the whole song. Only a fresh start can fill the cache.
                if start_at.is_zero() { cache.clone() } else { None },
            )?;
            Ok(BuiltSource {
                starved: Some(source.starvation_flag()),
                decoded: Box::new(source),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// ~2 seconds of silence at 44100Hz mono 16-bit.
    fn write_wav(path: &std::path::Path, seconds: u32) {
        let samples = 44100 * seconds;
        let data_len = samples * 2;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&44100u32.to_le_bytes());
        b.extend_from_slice(&88200u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        b.extend_from_slice(&vec![0u8; data_len as usize]);
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(&b).unwrap();
    }


    #[tokio::test]
    async fn progress_events_advance_while_a_track_plays() {
        let dir = std::env::temp_dir().join("music-app-engine-progress");
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        write_wav(&wav, 3);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // A real sidecar, because ffmpeg is now the only decoder. This used to
        // pass `None` -- a plain WAV needed no sidecar when rodio decoded it --
        // and leaving it that way would not have failed: the play returns an
        // error, the block below prints SKIP, and the test passes having
        // asserted nothing. A test that quietly stops testing is worse than
        // one that breaks.
        let ffmpeg = crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg);
        assert!(
            ffmpeg.is_some(),
            "no staged ffmpeg -- see src-tauri/binaries/README.md",
        );
        let engine = spawn(tx, ffmpeg);

        let played = engine
            .play(PlayableSource::LocalFile(wav), None, Duration::ZERO, 1)
            .await;
        if let Err(e) = &played {
            // Only a missing audio device is a legitimate skip now.
            eprintln!("SKIP: {e}");
            return;
        }

        std::thread::sleep(Duration::from_millis(1200));

        let mut positions = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let EngineEvent::Progress { position, .. } = event {
                positions.push(position);
            }
        }

        eprintln!("got {} progress events: {:?}", positions.len(), positions);
        assert!(!positions.is_empty(), "no progress events were emitted");
        assert!(
            positions.iter().any(|p| *p > Duration::ZERO),
            "progress events all reported zero: {positions:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod limiter_tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Runs a stereo signal through the real stage -- volume, then limiter --
    /// exactly as `start()` assembles it.
    fn through_stage(interleaved: Vec<f32>, gain: f32) -> Vec<f32> {
        let buffer = rodio::buffer::SamplesBuffer::new(
            rodio::ChannelCount::new(2).unwrap(),
            rodio::SampleRate::new(RATE).unwrap(),
            interleaved,
        );
        Levelled::new(buffer, Arc::new(AtomicU32::new(gain.to_bits()))).collect()
    }

    /// The full chain as `start()` assembles it, equaliser included.
    fn through_full_chain(interleaved: Vec<f32>, gain: f32, eq: Arc<EqSettings>) -> Vec<f32> {
        let buffer = rodio::buffer::SamplesBuffer::new(
            rodio::ChannelCount::new(2).unwrap(),
            rodio::SampleRate::new(RATE).unwrap(),
            interleaved,
        );
        Levelled::new(
            Equalised::new(buffer, eq),
            Arc::new(AtomicU32::new(gain.to_bits())),
        )
        .collect()
    }

    /// Adding the equaliser must not cost the bit-exactness below it.
    ///
    /// `Equalised` sits between the decoder and the volume stage, so it is now
    /// in the path of *every* sample -- including for the great majority of
    /// listeners who will never open the panel. If merely having it in the
    /// chain altered the audio, that would be a regression shipped to everyone
    /// to serve a feature almost nobody switched on.
    #[test]
    fn an_untouched_equaliser_keeps_the_chain_bit_exact() {
        let input = stereo_tone(1000.0, 0.5, 40_000);
        let eq = Arc::new(EqSettings::default());

        assert_eq!(
            through_full_chain(input.clone(), 1.0, eq),
            input,
            "the equaliser altered audio nobody asked it to touch",
        );
    }

    /// A boost is heard, and the limiter still holds the ceiling.
    ///
    /// These two have to be checked together. An equaliser that boosts into a
    /// signal already near full scale is the ordinary way to produce clipping,
    /// and the reason `Equalised` sits *before* the limiter rather than after
    /// it. Order the two the other way and this test is what fails.
    #[test]
    fn a_boost_is_audible_but_still_cannot_pass_the_ceiling() {
        // Already loud, so a boost has nowhere to go but through the ceiling.
        let input = stereo_tone(1000.0, 0.9, 40_000);

        let eq = Arc::new(EqSettings::default());
        eq.set_enabled(true);
        let mut gains = [0.0f32; crate::equalizer::BAND_COUNT];
        gains[5] = 12.0; // 1 kHz, where the tone is
        eq.set_gains(&gains);

        let out = through_full_chain(input.clone(), 1.0, eq);

        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            peak <= CEILING + 1e-6,
            "a boosted band drove the output to {peak}, past the ceiling",
        );

        // And it did something: the second half, past the limiter's attack and
        // the filter's settling, should be louder than it went in.
        let half = out.len() / 2;
        let rms = |v: &[f32]| {
            (v.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / v.len() as f64).sqrt()
        };
        assert!(
            rms(&out[half..]) > rms(&input[half..]) * 1.05,
            "the boost was inaudible: {:.4} in, {:.4} out",
            rms(&input[half..]),
            rms(&out[half..]),
        );
    }

    fn stereo_tone(freq: f64, peak: f32, frames: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let s = peak * (2.0 * std::f64::consts::PI * freq * i as f64 / RATE as f64).sin() as f32;
            v.push(s);
            v.push(s);
        }
        v
    }

    /// The property the whole design rests on, and the one the old waveshaper
    /// could not have: material that never threatens the ceiling is not merely
    /// *nearly* untouched, it is untouched. No knee, no shaping, no gain
    /// movement -- the same bits back.
    #[test]
    fn audio_below_the_ceiling_is_not_touched() {
        let input = stereo_tone(1000.0, 0.8, 4_000);
        let output = through_stage(input.clone(), 1.0);

        assert_eq!(output.len(), input.len(), "samples were lost or invented");
        for (i, (got, want)) in output.iter().zip(&input).enumerate() {
            assert_eq!(
                got, want,
                "sample {i} came back as {got} instead of {want}, so the stage is \
                 colouring material that never needed limiting",
            );
        }
    }

    /// The reason it exists. A lossy decode to f32 is not clamped, so
    /// brick-walled masters reconstruct above full scale -- measured here, 26 of
    /// 28 sampled tracks peak above 0 dBFS and the worst reaches +4.99 dBFS. The
    /// device hard-clips anything above 1.0, and a corner in a waveform is
    /// broadband distortion on the loudest moment of the track.
    #[test]
    fn nothing_ever_leaves_above_full_scale() {
        // +3.5, +6 and +12 dBFS, plus a gain that pushes an ordinary track there.
        for (peak, gain) in [(1.5f32, 1.0f32), (2.0, 1.0), (4.0, 1.0), (0.9, 4.0)] {
            let output = through_stage(stereo_tone(440.0, peak, 20_000), gain);
            let worst = output.iter().fold(0.0f32, |a, b| a.max(b.abs()));
            assert!(
                worst <= CEILING + 1e-6,
                "a {peak} peak at gain {gain} produced {worst}, which the device will clip",
            );
        }
    }

    /// One gain for the whole frame, or the stereo image moves every time one
    /// side peaks harder than the other -- which is audible as the picture
    /// wandering, not as level.
    #[test]
    fn both_channels_are_scaled_by_the_same_number() {
        let frames = 20_000;
        let mut input = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let s = (2.0 * std::f64::consts::PI * 440.0 * i as f64 / RATE as f64).sin() as f32;
            input.push(1.6 * s); // left, well over the ceiling
            input.push(0.32 * s); // right, a fifth of it and never near the ceiling
        }
        let output = through_stage(input, 1.0);

        // Sampled away from the zero crossings, where the ratio is meaningless.
        let mut checked = 0;
        for frame in output.chunks_exact(2).skip(frames / 2) {
            if frame[0].abs() < 0.2 {
                continue;
            }
            let ratio = frame[1] / frame[0];
            assert!(
                (ratio - 0.2).abs() < 1e-3,
                "the channels came out at a ratio of {ratio} instead of 0.2, \
                 so the image shifted",
            );
            checked += 1;
        }
        assert!(checked > 100, "only {checked} frames were loud enough to check");
    }

    /// A limiter that quietly drops or duplicates frames would swap the stereo
    /// channels for the rest of the track, which is the bug this codebase has
    /// already been bitten by once in the ffmpeg reader.
    #[test]
    fn every_sample_that_goes_in_comes_out() {
        for frames in [1usize, 7, 71, 72, 73, 5_000] {
            let input = stereo_tone(1000.0, 1.4, frames);
            let output = through_stage(input.clone(), 1.0);
            assert_eq!(
                output.len(),
                input.len(),
                "{frames} frames in produced {} samples out",
                output.len(),
            );
        }
    }

    /// What reaches the mixer must be what the limiter produced, bit for bit.
    ///
    /// `Mixer::add` wraps its input in a `UniformSourceIterator`, which reads
    /// the channel count and sample rate **once, at that moment**. Add an empty
    /// queue and it reads the placeholder `Empty` source, which reports **one
    /// channel** -- so the mixer builds a mono-to-stereo converter and runs real
    /// stereo audio through it for the rest of the track. Measured, that costs a
    /// 256-sample delay, a flat -60 dB error across every audible band, and
    /// +43 dB in the 22-24 kHz band.
    ///
    /// Appending before adding is the only thing that prevents it, and nothing
    /// about `start()` makes that ordering look load-bearing. Hence this test.
    #[test]
    fn the_mixer_receives_the_samples_unaltered() {
        let input = stereo_tone(1000.0, 0.5, 40_000);
        let buffer = rodio::buffer::SamplesBuffer::new(
            rodio::ChannelCount::new(2).unwrap(),
            rodio::SampleRate::new(RATE).unwrap(),
            input.clone(),
        );
        let levelled = Levelled::new(buffer, Arc::new(AtomicU32::new(1.0f32.to_bits())));

        let (mixer, mut out) = rodio::mixer::mixer(
            rodio::ChannelCount::new(2).unwrap(),
            rodio::SampleRate::new(RATE).unwrap(),
        );
        // The order under test.
        let (player, queue) = rodio::Player::new();
        player.append(levelled);
        mixer.add(queue);

        let mut got = Vec::with_capacity(input.len());
        while got.len() < input.len() {
            match out.next() {
                Some(s) => got.push(s),
                None => break,
            }
        }

        assert_eq!(got.len(), input.len(), "the mixer dropped or invented samples");
        for (i, (produced, expected)) in got.iter().zip(&input).enumerate() {
            assert_eq!(
                produced, expected,
                "sample {i} reached the mixer as {produced} instead of {expected} -- \
                 something between the limiter and the device is altering the audio",
            );
        }
    }

    /// Gain reduction must be smooth. A limiter that jumps its gain between
    /// adjacent frames is a step in the waveform, which is a click -- the same
    /// broadband distortion the waveshaper made, arrived at by another route.
    #[test]
    fn the_gain_never_jumps_between_frames() {
        // A quiet passage, then a loud one: the worst case for an envelope.
        let mut input = stereo_tone(440.0, 0.2, 10_000);
        input.extend(stereo_tone(440.0, 3.0, 10_000));
        let output = through_stage(input.clone(), 1.0);

        // The bound is the design's own, not a taste: a one-pole envelope moves
        // at most `1 - coefficient` of the distance to its target in a frame,
        // and the attack is the faster of the two. Anything larger means the
        // smoothing was bypassed -- an instantaneous attack would step by the
        // whole excursion at once, which is a click.
        let look = (RATE as f32 * LOOKAHEAD.as_secs_f32()) as usize;
        let bound = 1.0 - (-4.0f32 / look as f32).exp();

        let mut previous: Option<f32> = None;
        let mut worst = 0.0f32;
        for (frame_in, frame_out) in input.chunks_exact(2).zip(output.chunks_exact(2)) {
            if frame_in[0].abs() < 0.05 {
                continue;
            }
            let gain = frame_out[0] / frame_in[0];
            if let Some(last) = previous {
                worst = worst.max((gain - last).abs());
            }
            previous = Some(gain);
        }

        eprintln!("worst per-frame gain step: {worst:.5} (bound {bound:.5})");
        assert!(
            worst <= bound + 1e-4,
            "the gain stepped by {worst} in one frame against a bound of {bound}, \
             so the envelope is not being smoothed",
        );
    }
}

/// What the stage does to a *signal*, as opposed to what its transfer function
/// looks like.
///
/// The module this replaced checked the shape of a `tanh` waveshaper --
/// continuous, odd, monotonic, no corner -- and every one of those tests passed
/// while the same curve added 13.8% total harmonic distortion to a track
/// peaking at +3.18 dBFS. A smooth curve is still a *nonlinear* curve, and
/// every nonlinearity generates harmonics; smoothness only sets how fast the
/// series rolls off. Measuring the output is the only way that is visible.
#[cfg(test)]
mod signal_tests {
    use super::*;


    /// Changing the equaliser mid-song must not click.
    ///
    /// This is the case the steady-tone tests cannot see. They measure a filter
    /// that has settled; a preset change happens *while* audio is flowing, and
    /// the two ways to get it wrong both sound like a click:
    ///
    /// - Clearing the filter state along with the coefficients. The registers
    ///   hold the tail of the signal, and zeroing them puts a step through
    ///   every band at once.
    /// - Applying the bands one at a time, so the curve sweeps rather than
    ///   changes.
    ///
    /// A click is a discontinuity: a sample-to-sample jump far larger than the
    /// signal itself can produce. At 440 Hz and 48 kHz a sine moves at most
    /// about 0.058 of its amplitude per sample, so anything approaching the
    /// amplitude is a step, not music.
    #[test]
    fn changing_the_equaliser_mid_song_does_not_click() {
        const PEAK: f32 = 0.5;
        // The largest step a 440 Hz sine at this peak can legitimately make in
        // one sample, with generous headroom for the filters phase response.
        const MAX_STEP: f32 = PEAK * 0.25;

        for (name, gains) in PRESETS {
            let eq = Arc::new(EqSettings::default());
            eq.set_enabled(true);

            let input = stereo_tone(440.0, PEAK, 40_000);
            let buffer = rodio::buffer::SamplesBuffer::new(
                rodio::ChannelCount::new(2).unwrap(),
                rodio::SampleRate::new(RATE).unwrap(),
                input,
            );
            let mut source = Equalised::new(buffer, Arc::clone(&eq));

            // Play a while flat, switch to the preset, keep playing.
            let mut out = Vec::new();
            for _ in 0..20_000 {
                out.push(source.next().unwrap());
            }
            eq.set_gains(&gains);
            for _ in 0..40_000 {
                match source.next() {
                    Some(s) => out.push(s),
                    None => break,
                }
            }

            // Left channel only: the step test is per channel, and an
            // interleaved stream alternates between two of them.
            let left: Vec<f32> = out.iter().step_by(2).copied().collect();
            let (worst, at) = left
                .windows(2)
                .enumerate()
                .map(|(i, w)| ((w[1] - w[0]).abs(), i))
                .fold((0.0f32, 0), |acc, x| if x.0 > acc.0 { x } else { acc });

            assert!(
                worst < MAX_STEP,
                "switching to {name:?} mid-song produced a {worst:.4} jump between \
                 samples {at} and {}, which is a click",
                at + 1,
            );
            assert!(
                left.iter().all(|s| s.is_finite()),
                "switching to {name:?} mid-song produced values that are not numbers",
            );
        }
    }

    /// Switching between two presets, not just away from flat.
    ///
    /// The harder case: both curves are non-trivial, so the coefficients move
    /// on every band at once and the filter state was built by a different
    /// filter from the one about to use it.
    #[test]
    fn switching_between_presets_does_not_click() {
        const PEAK: f32 = 0.5;
        const MAX_STEP: f32 = PEAK * 0.25;

        for pair in PRESETS.windows(2) {
            let (from_name, from) = pair[0];
            let (to_name, to) = pair[1];

            let eq = Arc::new(EqSettings::default());
            eq.set_enabled(true);
            eq.set_gains(&from);

            let buffer = rodio::buffer::SamplesBuffer::new(
                rodio::ChannelCount::new(2).unwrap(),
                rodio::SampleRate::new(RATE).unwrap(),
                stereo_tone(440.0, PEAK, 40_000),
            );
            let mut source = Equalised::new(buffer, Arc::clone(&eq));

            let mut out = Vec::new();
            for _ in 0..20_000 {
                out.push(source.next().unwrap());
            }
            eq.set_gains(&to);
            for _ in 0..40_000 {
                match source.next() {
                    Some(s) => out.push(s),
                    None => break,
                }
            }

            let left: Vec<f32> = out.iter().step_by(2).copied().collect();
            let worst = left
                .windows(2)
                .map(|w| (w[1] - w[0]).abs())
                .fold(0.0f32, f32::max);

            assert!(
                worst < MAX_STEP,
                "{from_name:?} -> {to_name:?} mid-song produced a {worst:.4} jump \
                 between samples, which is a click",
            );
        }
    }

    /// Switching the equaliser off mid-song must not click either.
    ///
    /// The bypass is a *hard* branch -- it returns the input sample untouched,
    /// which is what makes flat bit-exact. That is also exactly how a
    /// discontinuity gets in: the filtered signal and the raw one are at
    /// different amplitudes and phases, so jumping between them is a step.
    #[test]
    fn toggling_the_equaliser_mid_song_is_measured() {
        const PEAK: f32 = 0.5;

        let eq = Arc::new(EqSettings::default());
        eq.set_enabled(true);
        eq.set_gains(&[6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);

        let buffer = rodio::buffer::SamplesBuffer::new(
            rodio::ChannelCount::new(2).unwrap(),
            rodio::SampleRate::new(RATE).unwrap(),
            stereo_tone(440.0, PEAK, 40_000),
        );
        let mut source = Equalised::new(buffer, Arc::clone(&eq));

        let mut out = Vec::new();
        for _ in 0..20_000 {
            out.push(source.next().unwrap());
        }
        eq.set_enabled(false);
        for _ in 0..20_000 {
            match source.next() {
                Some(s) => out.push(s),
                None => break,
            }
        }

        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        let worst = left
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);

        // Reported rather than asserted at the same bar as the others: this
        // one is a deliberate hard switch, and the number is what decides
        // whether it needs a ramp.
        eprintln!("bypass switch: worst sample-to-sample step {worst:.4}");
        assert!(
            worst < PEAK,
            "switching the equaliser off jumped {worst:.4} between samples, \
             which is louder than the music -- that is an audible click",
        );
    }

    /// Every shipped preset, measured for harmonic distortion.
    ///
    /// A biquad is a *linear* filter: it changes the level of frequencies that
    /// are already there and can never invent new ones. So a sine through any
    /// equaliser setting must come back a sine. Energy at 2f, 3f and above is
    /// the signature of something nonlinear -- clipping, an unstable filter,
    /// state shared between channels -- and none of those are subtle once you
    /// know to look for them.
    ///
    /// Level is kept low on purpose. At 0.1 peak even the heaviest preset stays
    /// under the ceiling, so the limiter never engages and what is measured is
    /// the equaliser alone. Limiting *is* nonlinear, and letting it fire here
    /// would measure the wrong thing and pass for the wrong reason.
    fn eq_distortion(gains: &[f32; crate::equalizer::BAND_COUNT], freq: f64) -> f64 {
        let eq = Arc::new(EqSettings::default());
        eq.set_enabled(true);
        eq.set_gains(gains);

        let input = stereo_tone(freq, 0.1, SETTLE + WINDOW + 2000);
        let buffer = rodio::buffer::SamplesBuffer::new(
            rodio::ChannelCount::new(2).unwrap(),
            rodio::SampleRate::new(RATE).unwrap(),
            input,
        );
        let out: Vec<f32> = Equalised::new(buffer, eq).collect();
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        distortion_db(&left[SETTLE..SETTLE + WINDOW], freq)
    }

    /// The presets exactly as the UI ships them.
    ///
    /// Copied rather than imported because they live in the frontend. A test
    /// that invented its own curves would prove nothing about what a listener
    /// The decisive question: does the equaliser invent frequencies?
    ///
    /// A biquad cannot, in theory. This measures whether ours does in practice,
    /// by looking specifically at the harmonics of the input tone rather than
    /// at total residual energy. That distinction was worth making: an earlier
    /// build measured -61 dB of *total* residual on an extreme setting, which
    /// looks alarming, while its harmonics sat at -130 dB. The residual was
    /// arithmetic noise from ten cascaded recursive sections in `f32` -- audible
    /// as nothing, but 60 dB above where it belonged. Moving the filter state to
    /// `f64` put it at -139 dB and the harmonics below -300 dB.
    ///
    /// So the bar here is deliberately brutal. Anything nonlinear -- clipping,
    /// an unstable band, state leaking between channels -- shows up as harmonic
    /// energy long before it is audible, and this is what would catch it.
    #[test]
    fn the_equaliser_never_invents_a_frequency() {
        const NO_HARMONICS_DB: f64 = -120.0;

        for (name, gains) in PRESETS.iter().chain(&[
            ("all up", [crate::equalizer::MAX_GAIN_DB; 10]),
            ("all down", [-crate::equalizer::MAX_GAIN_DB; 10]),
            (
                "alternating",
                [12.0, -12.0, 12.0, -12.0, 12.0, -12.0, 12.0, -12.0, 12.0, -12.0],
            ),
        ]) {
            for freq in [440.0f64, 1_000.0, 4_000.0] {
                let eq = Arc::new(EqSettings::default());
                eq.set_enabled(true);
                eq.set_gains(gains);

                let buffer = rodio::buffer::SamplesBuffer::new(
                    rodio::ChannelCount::new(2).unwrap(),
                    rodio::SampleRate::new(RATE).unwrap(),
                    stereo_tone(freq, 0.1, SETTLE + WINDOW + 2000),
                );
                let out: Vec<f32> = Equalised::new(buffer, eq).collect();
                let left: Vec<f32> = out.iter().step_by(2).copied().collect();
                let w = &left[SETTLE..SETTLE + WINDOW];

                let fundamental = tone_power(w, freq);
                // Only harmonics that fit below Nyquist; above it there is
                // nothing to measure and the correlation aliases.
                let harmonics: f64 = (2..=12)
                    .map(|n| freq * n as f64)
                    .filter(|f| *f < RATE as f64 / 2.0)
                    .map(|f| tone_power(w, f))
                    .sum();

                let ratio = 10.0 * (harmonics / fundamental).max(1e-30).log10();
                assert!(
                    ratio < NO_HARMONICS_DB,
                    "{name:?} at {freq} Hz generated harmonics {ratio:.1} dB below \
                     the tone -- a linear filter cannot do that, so something \
                     nonlinear is happening",
                );
            }
        }
    }

    /// can actually select.
    const PRESETS: [(&str, [f32; 10]); 7] = [
        ("Flat", [0.0; 10]),
        ("Bass boost", [6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
        ("Vocal", [-2.0, -2.0, -1.0, 1.0, 3.0, 3.0, 2.0, 1.0, 0.0, -1.0]),
        ("Treble", [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 2.0, 4.0, 5.0, 5.0]),
        ("Loudness", [5.0, 4.0, 2.0, 0.0, -1.0, -1.0, 0.0, 2.0, 4.0, 5.0]),
        ("Electronic", [4.0, 3.0, 1.0, 0.0, -1.0, 0.0, 1.0, 2.0, 3.0, 4.0]),
        ("Rock", [3.0, 2.0, 0.0, -1.0, -1.0, 1.0, 2.0, 3.0, 3.0, 2.0]),
    ];

    #[test]
    fn no_preset_adds_harmonic_distortion() {
        // Across the spectrum, not just mid-band: an unstable band shows up
        // only when there is signal in it.
        for freq in [100.0, 440.0, 1_000.0, 4_000.0, 10_000.0] {
            for (name, gains) in PRESETS {
                let d = eq_distortion(&gains, freq);
                assert!(
                    d < TRANSPARENT_DB,
                    "preset {name:?} at {freq} Hz produced {d:.1} dB of distortion \
                     ({:.4}%), which means it is not behaving as a linear filter",
                    100.0 * 10f64.powf(d / 20.0),
                );
            }
        }
    }

    /// The extremes of the control, which is what someone will actually drag to.
    #[test]
    fn even_the_most_extreme_setting_stays_linear() {
        for gains in [
            [crate::equalizer::MAX_GAIN_DB; 10],
            [-crate::equalizer::MAX_GAIN_DB; 10],
            // Alternating, the worst case for bands fighting each other.
            [12.0, -12.0, 12.0, -12.0, 12.0, -12.0, 12.0, -12.0, 12.0, -12.0],
        ] {
            for freq in [440.0, 1_000.0, 4_000.0] {
                let d = eq_distortion(&gains, freq);
                assert!(
                    d < TRANSPARENT_DB,
                    "{gains:?} at {freq} Hz produced {d:.1} dB of distortion",
                );
            }
        }
    }

    /// Nothing may come out that is not a number.
    ///
    /// A single NaN poisons the biquad state permanently: every sample after it
    /// is NaN, which reaches the device as full-scale noise rather than silence.
    #[test]
    fn no_preset_can_emit_a_value_that_is_not_a_number() {
        for (name, gains) in PRESETS {
            let eq = Arc::new(EqSettings::default());
            eq.set_enabled(true);
            eq.set_gains(&gains);

            let buffer = rodio::buffer::SamplesBuffer::new(
                rodio::ChannelCount::new(2).unwrap(),
                rodio::SampleRate::new(RATE).unwrap(),
                stereo_tone(1_000.0, 1.0, 20_000),
            );
            let out: Vec<f32> = Equalised::new(buffer, eq).collect();
            assert!(
                out.iter().all(|s| s.is_finite()),
                "preset {name:?} emitted a value that is not finite",
            );
        }
    }
    /// 48 kHz with a 4800-sample window puts bins exactly 10 Hz apart, so every
    /// frequency used below lands dead on a bin. That matters: an off-bin tone
    /// leaks across its neighbours, and the measurement would report the
    /// leakage as distortion -- failing these tests for a reason that is not
    /// real.
    const RATE: u32 = 48_000;
    const WINDOW: usize = 4_800;

    /// Long enough for the release to settle before anything is measured.
    const SETTLE: usize = RATE as usize / 2;

    fn through_stage(interleaved: Vec<f32>, gain: f32) -> Vec<f32> {
        let buffer = rodio::buffer::SamplesBuffer::new(
            rodio::ChannelCount::new(2).unwrap(),
            rodio::SampleRate::new(RATE).unwrap(),
            interleaved,
        );
        Levelled::new(buffer, Arc::new(AtomicU32::new(gain.to_bits()))).collect()
    }

    /// The steady-state left channel, one bin-exact window of it.
    fn settled_left(interleaved: Vec<f32>, gain: f32) -> Vec<f32> {
        let out = through_stage(interleaved, gain);
        let left: Vec<f32> = out.iter().step_by(2).copied().collect();
        left[SETTLE..SETTLE + WINDOW].to_vec()
    }

    /// Power of the single frequency component at `freq`, by direct correlation.
    fn tone_power(x: &[f32], freq: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq / RATE as f64;
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, s) in x.iter().enumerate() {
            let a = w * i as f64;
            re += *s as f64 * a.cos();
            im -= *s as f64 * a.sin();
        }
        let n = x.len() as f64;
        2.0 * (re * re + im * im) / (n * n)
    }

    fn total_power(x: &[f32]) -> f64 {
        x.iter().map(|s| *s as f64 * *s as f64).sum::<f64>() / x.len() as f64
    }

    fn stereo_tone(freq: f64, peak: f32, frames: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let s = peak * (2.0 * std::f64::consts::PI * freq * i as f64 / RATE as f64).sin() as f32;
            v.push(s);
            v.push(s);
        }
        v
    }

    /// Everything that is not the tone, relative to the tone, in dB.
    fn distortion_db(out: &[f32], freq: f64) -> f64 {
        let fundamental = tone_power(out, freq);
        let rest = (total_power(out) - fundamental).max(0.0);
        10.0 * (rest / fundamental).max(1e-30).log10()
    }

    /// The bar a gain stage has to clear before it can be called transparent.
    ///
    /// -60 dB is 0.1% THD+N. Chosen because it is about an order of magnitude
    /// below where broadband nonlinear distortion starts being reported as
    /// audible on music, so a stage that meets it is inaudible with margin
    /// rather than borderline.
    const TRANSPARENT_DB: f64 = -60.0;

    /// Below the ceiling the stage is a straight wire, and this is the control:
    /// if it ever fails, the measurement is broken rather than the code.
    #[test]
    fn a_tone_below_the_ceiling_comes_back_clean() {
        let settled = settled_left(stereo_tone(1000.0, 0.8, SETTLE + WINDOW + 1000), 1.0);
        let d = distortion_db(&settled, 1000.0);
        assert!(
            d < -120.0,
            "a tone below the ceiling measured {d:.1} dB of distortion, \
             so the measurement is wrong",
        );
    }

    /// The levels here are not hypothetical. Measured over this library with the
    /// bundled ffmpeg, 26 of 28 sampled tracks peak above 0 dBFS once decoded to
    /// f32, and the loudest reaches +4.99 dBFS. At unity gain -- the shipped
    /// default, `volume: 1` with `volumeCeilingDb: 0` -- every one of those
    /// peaks reaches this stage.
    ///
    /// The old waveshaper failed this at the very first level, measuring 0.88%
    /// at 0 dBFS and 13.8% at +3.17.
    #[test]
    fn a_tone_at_the_levels_this_library_reaches_gains_no_harmonics() {
        for peak in [1.0f32, 1.19, 1.44, 1.78] {
            let settled = settled_left(stereo_tone(1000.0, peak, SETTLE + WINDOW + 1000), 1.0);
            let d = distortion_db(&settled, 1000.0);
            assert!(
                d < TRANSPARENT_DB,
                "a 1 kHz tone peaking at {:.2} dBFS came back with {d:.1} dB of distortion, \
                 which is {:.3}% -- the stage is not transparent",
                20.0 * peak.log10(),
                100.0 * 10f64.powf(d / 20.0),
            );
        }
    }

    /// The test that names the complaint.
    ///
    /// A lossy master arrives band-limited: an m4a from this library is
    /// brickwalled just under 16 kHz, and above that there is nothing at all. A
    /// gain stage cannot put anything there. A nonlinear one fills it with
    /// harmonics -- measured on `4GET. - dethkitty.m4a`, the 16-22 kHz band left
    /// the old waveshaper 16.9 dB *louder* than it arrived, entirely
    /// manufactured.
    ///
    /// That is where "harsh" lives: the harmonics landing below Nyquist pile up
    /// in the presence region, and the ones above it alias back down into it.
    #[test]
    fn nothing_is_created_above_the_bandwidth_of_the_source() {
        // Three tones, none above 5 kHz, so every component above 12 kHz in the
        // output was invented by the stage rather than passed through it.
        let frames = SETTLE + WINDOW + 1000;
        let mut mixed = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let t = i as f64 / RATE as f64;
            let s = (2.0 * std::f64::consts::PI * 1000.0 * t).sin()
                + (2.0 * std::f64::consts::PI * 3000.0 * t).sin()
                + (2.0 * std::f64::consts::PI * 5000.0 * t).sin();
            // Scaled so the sum peaks at +2 dBFS, ordinary for this library.
            let s = (s / 3.0 * 1.259 * 3.0 / 2.7) as f32;
            mixed.push(s);
            mixed.push(s);
        }
        let peak = mixed.iter().fold(0.0f32, |a, b| a.max(b.abs()));
        let scaled: Vec<f32> = mixed.iter().map(|s| s * 1.259 / peak).collect();

        let arrived: Vec<f32> = scaled.iter().step_by(2).copied().collect();
        let arrived = &arrived[SETTLE..SETTLE + WINDOW];
        let settled = settled_left(scaled.clone(), 1.0);

        let in_band: f64 = [1000.0, 3000.0, 5000.0]
            .iter()
            .map(|f| tone_power(arrived, *f))
            .sum();
        // Every bin from 12 kHz to just under Nyquist.
        let created: f64 = (1200..2390)
            .map(|b| tone_power(&settled, b as f64 * 10.0))
            .sum();

        let ratio = 10.0 * (created / in_band).max(1e-30).log10();
        assert!(
            ratio < TRANSPARENT_DB,
            "the stage put {ratio:.1} dB of energy above 12 kHz into material that had none \
             there -- that is manufactured treble, and it is what 'harsh' means",
        );
    }

    /// rodio resamples any source whose rate differs from the mixer's, and its
    /// converter is linear interpolation. Measured against this library at
    /// 44.1 -> 48 kHz, that is error only 11.7 dB below the music across the
    /// 10-16 kHz band, and it leaves the 16-22 kHz band 28 dB too loud.
    ///
    /// `build_source` avoids this by asking ffmpeg for the device's own rate,
    /// which now covers everything: there is no second decode path left to be
    /// exempt from it. There used to be, and it was not an edge case -- 93 of
    /// 150 files sampled from this library are 44.1 kHz while all sixteen
    /// render endpoints on this machine run at 48 kHz.
    ///
    /// The invariant is structural and cheap to hold: whatever `build_source`
    /// returns is already at the device rate, so rodio's converter is never
    /// reached.
    #[test]
    fn a_decoder_is_never_handed_to_rodios_resampler() {
        let dir = std::env::temp_dir().join("music-app-engine-rate");
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("44100.wav");
        write_tone_wav(&wav, 44_100, 2);

        let ffmpeg = crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg);
        assert!(
            ffmpeg.is_some(),
            "the staged ffmpeg sidecar is missing, so this test would prove nothing",
        );

        let built = build_source(
            &PlayableSource::LocalFile(wav),
            ffmpeg.as_deref(),
            48_000,
            Duration::ZERO,
        )
        .expect("a plain 44.1 kHz WAV should decode");

        let got = built.decoded.sample_rate().get();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            got, 48_000,
            "a 44.1 kHz file reached the mixer at {got} Hz against a 48 kHz device, \
             so rodio's linear-interpolation converter runs on every sample of it",
        );
    }

    /// A 1 kHz tone at half scale, so the file is real audio rather than silence.
    fn write_tone_wav(path: &std::path::Path, rate: u32, seconds: u32) {
        let frames = rate * seconds;
        let data_len = frames * 2;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVE");
        b.extend_from_slice(b"fmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * 2).to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            let s = (0.5
                * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / rate as f64).sin()
                * i16::MAX as f64) as i16;
            b.extend_from_slice(&s.to_le_bytes());
        }
        let mut f = std::fs::File::create(path).unwrap();
        std::io::Write::write_all(&mut f, &b).unwrap();
    }
}

/// Records what Windows actually sends to the speakers, rather than what this
/// crate believes it sent.
///
/// Every other measurement in this file reasons about the signal chain from the
/// inside. This one stands outside it: a WASAPI loopback capture on the default
/// render endpoint returns the audio engine's own output, after this app, after
/// the per-application volume, and after any effect running in the audio engine.
/// Nulling that against the source file is the only way to tell "this app
/// damages the audio" apart from "something after this app does".
///
/// Both tests are `#[ignore]`d: they need a real output device, they run in real
/// time, and one of them expects a human to start another player. Run them by
/// name.
///
/// ```text
/// MUSIC_APP_PROBE_FILE=... MUSIC_APP_PROBE_OUT=... \
///   cargo test --lib capture_this_apps_own_output -- --ignored --nocapture
/// ```
#[cfg(test)]
mod loopback_probe {
    use super::*;
    use rodio::cpal;
    use rodio::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::{Arc, Mutex};

    fn env_or(key: &str, fallback: &str) -> String {
        std::env::var(key).unwrap_or_else(|_| fallback.to_string())
    }

    /// The endpoint's own mix format, and a recorder already running on it.
    ///
    /// Building an *input* stream on an *output* device is what puts WASAPI into
    /// loopback mode -- cpal does this transparently, and it is the whole trick.
    fn start_capture() -> (Arc<Mutex<Vec<f32>>>, cpal::Stream, u32, u16) {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .expect("there is no default output device to record");
        let supported = device
            .default_output_config()
            .expect("the default output device reports no config");

        assert_eq!(
            supported.sample_format(),
            cpal::SampleFormat::F32,
            "this probe only decodes an f32 mix format",
        );
        let rate = supported.sample_rate();
        let channels = supported.channels();
        eprintln!(
            "capturing loopback from {:?} at {rate} Hz, {channels} ch",
            device.name().unwrap_or_else(|_| "<unnamed>".into()),
        );

        let captured = Arc::new(Mutex::new(Vec::<f32>::new()));
        let sink = Arc::clone(&captured);
        let stream = device
            .build_input_stream(
                &supported.into(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if let Ok(mut buf) = sink.lock() {
                        buf.extend_from_slice(data);
                    }
                },
                |e| eprintln!("capture error: {e}"),
                None,
            )
            .expect("could not open a loopback capture stream");
        stream.play().expect("could not start the capture");

        (captured, stream, rate, channels)
    }

    fn write_capture(captured: &Arc<Mutex<Vec<f32>>>, rate: u32, channels: u16, out: &str) {
        let buf = captured.lock().unwrap();
        let peak = buf.iter().fold(0.0f32, |a, b| a.max(b.abs()));
        let frames = buf.len() / channels.max(1) as usize;
        eprintln!(
            "captured {frames} frames ({:.2} s), peak {:.2} dBFS",
            frames as f32 / rate as f32,
            20.0 * peak.max(1e-12).log10(),
        );
        assert!(
            peak > 1e-5,
            "the capture is silent -- nothing was playing, or the default output \
             device is not the one being played to",
        );

        let mut bytes = Vec::with_capacity(buf.len() * 4);
        for s in buf.iter() {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(out, &bytes).expect("could not write the capture");
        eprintln!("wrote {out} ({rate} Hz, {channels} ch, f32le)");
    }

    /// Plays a file through this app's real engine and records the result.
    ///
    /// Deliberately the engine rather than the coordinator: `player.rs` only
    /// decides *what* to play and at what gain, and gain is set explicitly here
    /// so the capture is comparable against the source at unity.
    #[tokio::test]
    #[ignore = "needs a real output device and runs in real time"]
    async fn capture_this_apps_own_output() {
        let file = env_or("MUSIC_APP_PROBE_FILE", "");
        assert!(!file.is_empty(), "set MUSIC_APP_PROBE_FILE to the track to play");
        let out = env_or("MUSIC_APP_PROBE_OUT", "app-capture.f32");
        let secs: f32 = env_or("MUSIC_APP_PROBE_SECS", "20").parse().unwrap();
        let start_at: f64 = env_or("MUSIC_APP_PROBE_START", "0").parse().unwrap();

        let (captured, stream, rate, channels) = start_capture();

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let ffmpeg = crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg);
        assert!(ffmpeg.is_some(), "the staged ffmpeg sidecar is missing");
        let engine = spawn(tx, ffmpeg);
        // Linear amplitude, not a slider position: the point of the probe is to
        // drive the endpoint at a chosen level, and levels are what the chain
        // after this app turns out to be sensitive to.
        let gain: f32 = env_or("MUSIC_APP_PROBE_GAIN", "1.0").parse().unwrap();
        eprintln!("engine gain {gain} ({:.2} dB)", 20.0 * gain.log10());
        engine.set_volume(gain).expect("set volume");

        engine
            .play(
                PlayableSource::LocalFile(std::path::PathBuf::from(&file)),
                None,
                Duration::from_secs_f64(start_at),
                1,
            )
            .await
            .expect("the engine refused to play the file");

        tokio::time::sleep(Duration::from_secs_f32(secs)).await;
        drop(stream);
        write_capture(&captured, rate, channels, &out);
    }

    /// Records the endpoint while some *other* player has the track.
    ///
    /// The comparison this exists for: the same file, the same endpoint, the
    /// same capture path, a different player. Anything that differs between the
    /// two captures is the players differing -- nothing else in the chain moved.
    #[test]
    #[ignore = "needs a human to start playback in another player"]
    fn capture_whatever_is_playing() {
        let out = env_or("MUSIC_APP_PROBE_OUT", "other-capture.f32");
        let secs: f32 = env_or("MUSIC_APP_PROBE_SECS", "20").parse().unwrap();
        let wait: f32 = env_or("MUSIC_APP_PROBE_WAIT", "5").parse().unwrap();

        eprintln!("start playback in the other player now -- recording in {wait:.0} s");
        std::thread::sleep(Duration::from_secs_f32(wait));

        let (captured, stream, rate, channels) = start_capture();
        std::thread::sleep(Duration::from_secs_f32(secs));
        drop(stream);
        write_capture(&captured, rate, channels, &out);
    }
}

#[cfg(test)]
mod rate_check {
    use super::*;

    /// The rate invariant, on a real file of the reader's choosing.
    ///
    /// rodio resamples any source whose rate differs from the mixer's, with a
    /// converter its own docs call "simple linear interpolation" -- measured
    /// over this library at error only 12 dB below the music across 10-16 kHz.
    /// Everything must therefore reach the mixer already at the device rate.
    ///
    /// That used to be conditional, and this printed which way it went. It is
    /// now structural: ffmpeg decodes every source and is always told the
    /// device rate. So this asserts rather than reports -- if it ever fails,
    /// something has learned to hand the mixer a rate of its own again.
    #[test]
    #[ignore = "diagnostic, needs a real file via MUSIC_APP_PROBE_FILE"]
    fn what_rate_does_this_file_reach_the_mixer_at() {
        let file = std::env::var("MUSIC_APP_PROBE_FILE").expect("set MUSIC_APP_PROBE_FILE");
        let device: u32 = std::env::var("MUSIC_APP_PROBE_RATE")
            .unwrap_or_else(|_| "48000".into())
            .parse()
            .unwrap();
        let ffmpeg = crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg);
        assert!(ffmpeg.is_some(), "staged ffmpeg missing");

        let built = build_source(
            &PlayableSource::LocalFile(std::path::PathBuf::from(&file)),
            ffmpeg.as_deref(),
            device,
            Duration::ZERO,
        )
        .expect("build_source failed");

        let reached = built.decoded.sample_rate().get();
        eprintln!("device wants {device} Hz; build_source returned {reached} Hz");
        assert_eq!(
            reached, device,
            "the mixer was handed {reached} Hz for a {device} Hz device, so \
             rodio's linear-interpolation resampler is running",
        );
    }
}

#[cfg(test)]
mod mixer_tap {
    //! The app's digital output, with the operating system taken out of the
    //! picture entirely.
    //!
    //! A loopback capture measures this app *plus* Windows plus every effect
    //! registered on the endpoint, and cannot tell them apart. This assembles
    //! the identical rodio chain `start()` builds -- `Levelled` on a `Player` on
    //! a mixer at the device's rate -- and pulls the samples straight out of the
    //! mixer instead of handing them to cpal. Whatever this writes is exactly
    //! what the app asks Windows to play, and nothing else.
    use super::*;

    #[test]
    #[ignore = "diagnostic, writes the app's own mixer output to a file"]
    fn what_the_mixer_actually_produces() {
        let file = std::env::var("MUSIC_APP_PROBE_FILE").expect("set MUSIC_APP_PROBE_FILE");
        let out = std::env::var("MUSIC_APP_PROBE_OUT").expect("set MUSIC_APP_PROBE_OUT");
        let rate: u32 = std::env::var("MUSIC_APP_PROBE_RATE")
            .unwrap_or_else(|_| "48000".into())
            .parse()
            .unwrap();
        let secs: f32 = std::env::var("MUSIC_APP_PROBE_SECS")
            .unwrap_or_else(|_| "12".into())
            .parse()
            .unwrap();
        let gain: f32 = std::env::var("MUSIC_APP_PROBE_GAIN")
            .unwrap_or_else(|_| "1.0".into())
            .parse()
            .unwrap();
        let start_at: f64 = std::env::var("MUSIC_APP_PROBE_START")
            .unwrap_or_else(|_| "0".into())
            .parse()
            .unwrap();

        let ffmpeg = crate::sidecar::staged_for_tests(crate::sidecar::Tool::Ffmpeg);
        let built = build_source(
            &PlayableSource::LocalFile(std::path::PathBuf::from(&file)),
            ffmpeg.as_deref(),
            rate,
            Duration::from_secs_f64(start_at),
        )
        .expect("build_source failed");
        eprintln!("decoder hands over {} Hz", built.decoded.sample_rate().get());

        let wanted = (rate as f32 * secs) as usize * 2;
        let mut samples = Vec::with_capacity(wanted);
        let mut levelled = Levelled::new(
            built.decoded,
            Arc::new(AtomicU32::new(gain.to_bits())),
        );

        // Which slice of the chain to tap, so the cost of each stage can be
        // attributed instead of guessed at. `raw` is this app's own code;
        // everything past it is rodio's.
        let stage = std::env::var("MUSIC_APP_PROBE_STAGE").unwrap_or_else(|_| "mixer".into());
        eprintln!("tapping stage: {stage}");
        match stage.as_str() {
            // Decoder plus gain plus limiter. No rodio adapters at all.
            "raw" => {
                while samples.len() < wanted {
                    match levelled.next() {
                        Some(s) => samples.push(s),
                        None => break,
                    }
                }
            }
            // What `Mixer::add` wraps every source in, and nothing else. At a
            // matching rate and channel count this should be a pass-through.
            "uniform" => {
                let mut uniform = rodio::source::UniformSourceIterator::new(
                    levelled,
                    rodio::ChannelCount::new(2).unwrap(),
                    rodio::SampleRate::new(rate).unwrap(),
                );
                while samples.len() < wanted {
                    match uniform.next() {
                        Some(s) => samples.push(s),
                        None => break,
                    }
                }
            }
            // The adapter stack `Player::append` builds, plus the queue it
            // feeds -- but not the mixer. `Player::new` hands back the queue
            // output directly, which is what makes the split possible.
            "player" => {
                let (player, mut queue_out) = rodio::Player::new();
                player.append(levelled);
                while samples.len() < wanted {
                    match queue_out.next() {
                        Some(s) => samples.push(s),
                        None => break,
                    }
                }
            }
            // What `Mixer::add` actually wraps: the *queue*, not the source.
            // The distinction matters because `Player::connect_new` adds the
            // queue to the mixer while it is still empty, and an empty queue's
            // `current` is `Empty`, which reports 1 channel and a span of 0.
            "uniform_queue" => {
                let (player, queue_out) = rodio::Player::new();
                player.append(levelled);
                let mut uniform = rodio::source::UniformSourceIterator::new(
                    queue_out,
                    rodio::ChannelCount::new(2).unwrap(),
                    rodio::SampleRate::new(rate).unwrap(),
                );
                while samples.len() < wanted {
                    match uniform.next() {
                        Some(s) => samples.push(s),
                        None => break,
                    }
                }
            }
            // The same, but wrapped *before* anything is appended -- which is
            // the order `Player::connect_new` uses.
            "uniform_queue_empty" => {
                let (player, queue_out) = rodio::Player::new();
                let mut uniform = rodio::source::UniformSourceIterator::new(
                    queue_out,
                    rodio::ChannelCount::new(2).unwrap(),
                    rodio::SampleRate::new(rate).unwrap(),
                );
                player.append(levelled);
                while samples.len() < wanted {
                    match uniform.next() {
                        Some(s) => samples.push(s),
                        None => break,
                    }
                }
            }
            // Exactly what `start()` builds, against a plain mixer rather than
            // a device -- including the append-before-add order.
            _ => {
                let (mixer, mut mixer_out) = rodio::mixer::mixer(
                    rodio::ChannelCount::new(2).unwrap(),
                    rodio::SampleRate::new(rate).unwrap(),
                );
                let (player, queue) = rodio::Player::new();
                player.append(levelled);
                mixer.add(queue);
                while samples.len() < wanted {
                    match mixer_out.next() {
                        Some(s) => samples.push(s),
                        None => break,
                    }
                }
            }
        }

        let peak = samples.iter().fold(0.0f32, |a, b| a.max(b.abs()));
        eprintln!(
            "pulled {} frames, peak {:.2} dBFS",
            samples.len() / 2,
            20.0 * peak.max(1e-12).log10()
        );

        let mut bytes = Vec::with_capacity(samples.len() * 4);
        for s in &samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(&out, &bytes).expect("write");
        eprintln!("wrote {out} ({rate} Hz, 2 ch, f32le)");
    }
}
