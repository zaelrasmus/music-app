use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::stream::DeviceSinkBuilder;
use rodio::{Decoder, Player, Source};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

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
}

impl AudioEngine {
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
    std::thread::Builder::new()
        .name("audio".to_string())
        .spawn(move || run(rx, events, ffmpeg))
        .expect("audio thread should spawn");

    AudioEngine { tx: Mutex::new(tx) }
}

fn run(rx: Receiver<Command>, events: UnboundedSender<EngineEvent>, ffmpeg: Option<PathBuf>) {
    // Opened once. If there is no device we keep the error and fail every play
    // with it, rather than killing the thread and making every later command
    // report "not running".
    let device = match DeviceSinkBuilder::open_default_sink() {
        Ok(mut sink) => {
            sink.log_on_drop(false);
            Ok(sink)
        }
        Err(e) => Err(format!("No audio output device: {e}")),
    };

    let mut player: Option<Player> = None;
    let mut epoch: Option<u64> = None;
    // Volume lives on `Player`, and a fresh `Player` is built per track, so it
    // must be remembered here and re-applied on every start. Otherwise it
    // silently resets to full on each queue advance.
    let mut volume = 1.0f32;
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
                            Ok(start(sink, ready.decoded, volume))
                        }
                        None => build_source(&source, ffmpeg.as_deref(), start_at).map(|built| {
                            starved = built.starved;
                            start(sink, built.decoded, volume)
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
                volume = linear;
                if let Some(p) = &player {
                    p.set_volume(linear);
                }
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
                    volume,
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
/// Local files seek natively and instantly. Everything ffmpeg decodes -- Opus
/// files, and every remote stream -- cannot: `FfmpegSource` reads a pipe, and
/// rodio would drive `try_seek` from the audio callback thread where spawning
/// a process is not allowed. So the decode is restarted here instead, on the
/// engine thread, against the URL already in hand. Re-resolving through yt-dlp
/// would add seconds; reusing the resolved URL costs about 0.4s for YouTube
/// and 2s for SoundCloud's HLS.
fn seek(
    device: Result<&rodio::MixerDeviceSink, &String>,
    player: &mut Option<Player>,
    loaded: &mut Option<Loaded>,
    position: Duration,
    volume: f32,
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

    let rebuilt = build_source(&l.source, ffmpeg, position).map(|built| {
        let starved = built.starved;
        (start(sink, built.decoded, volume), starved)
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
    /// `None` for a source that cannot meaningfully starve, such as a file
    /// rodio decodes itself.
    pub starved: Option<Arc<AtomicBool>>,
}

/// Connects a ready decoder to the output. Cheap -- no process, no waiting.
fn start(sink: &rodio::MixerDeviceSink, decoded: Box<dyn Source + Send>, volume: f32) -> Player {
    let player = Player::connect_new(sink.mixer());
    player.set_volume(volume);
    player.append(decoded);
    player
}

/// Builds the decoder for a source, ready to be appended.
///
/// Deliberately separate from [`start`], and public, because this is the
/// expensive half: for anything ffmpeg handles it spawns a process and waits
/// for the first half second of audio. Splitting it lets the coordinator do
/// that work early, on another thread, for a track that has not started yet.
pub fn build_source(
    source: &PlayableSource,
    ffmpeg: Option<&Path>,
    start_at: Duration,
) -> Result<BuiltSource, String> {
    match source {
        PlayableSource::LocalFile(path) => {
            // Native first, seeking it in place when a start was asked for: a
            // file seeks cheaply and rodio can do it, so resuming partway
            // through one should not need ffmpeg at all.
            match native_decoder(path) {
                Ok(mut decoder) => {
                    if start_at.is_zero() || decoder.try_seek(start_at).is_ok() {
                        // A file rodio reads directly cannot starve on the
                        // network, which is the only starvation worth naming.
                        return Ok(BuiltSource {
                            decoded: Box::new(decoder),
                            starved: None,
                        });
                    }
                    // Decoded but would not seek. ffmpeg can start anywhere.
                }
                // The seam's guess was wrong, or the file is something neither
                // of us anticipated. ffmpeg understands far more formats than
                // rodio, so it is worth one attempt before giving up -- but
                // with no ffmpeg the native failure is the real answer.
                Err(native_error) if ffmpeg.is_none() => return Err(native_error),
                Err(_) => {}
            }

            let ffmpeg = ffmpeg.ok_or(
                "This file needs ffmpeg to play, and ffmpeg was not found. \
                 See src-tauri/binaries/README.md.",
            )?;
            let source = FfmpegSource::open_at(ffmpeg, FfmpegInput::File(path), start_at, None)?;
            Ok(BuiltSource {
                starved: Some(source.starvation_flag()),
                decoded: Box::new(source),
            })
        }

        PlayableSource::Transcoded(path) => {
            let ffmpeg = ffmpeg.ok_or(
                "This file needs ffmpeg to play, and ffmpeg was not found. \
                 See src-tauri/binaries/README.md.",
            )?;
            let source = FfmpegSource::open_at(ffmpeg, FfmpegInput::File(path), start_at, None)?;
            Ok(BuiltSource {
                starved: Some(source.starvation_flag()),
                decoded: Box::new(source),
            })
        }

        PlayableSource::Stream { url, cache } => {
            let ffmpeg = ffmpeg.ok_or(
                "Streaming needs ffmpeg, and ffmpeg was not found. \
                 See src-tauri/binaries/README.md.",
            )?;
            let source = FfmpegSource::open_at(
                ffmpeg,
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

fn native_decoder(path: &Path) -> Result<Decoder<std::io::BufReader<File>>, String> {
    let file = File::open(path).map_err(|e| format!("Could not open the file: {e}"))?;
    Decoder::try_from(file).map_err(|e| format!("Could not decode the audio: {e}"))
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
        let mut f = File::create(path).unwrap();
        f.write_all(&b).unwrap();
    }


    #[tokio::test]
    async fn progress_events_advance_while_a_track_plays() {
        let dir = std::env::temp_dir().join("music-app-engine-progress");
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        write_wav(&wav, 3);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // No ffmpeg needed: this test plays a plain WAV, which rodio decodes.
        let engine = spawn(tx, None);

        let played = engine
            .play(PlayableSource::LocalFile(wav), None, Duration::ZERO, 1)
            .await;
        if let Err(e) = &played {
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
