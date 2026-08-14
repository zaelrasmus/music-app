use std::fs::File;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::Duration;

use rodio::stream::DeviceSinkBuilder;
use rodio::{Decoder, Player};
use tokio::sync::mpsc::UnboundedSender;

use crate::playable::PlayableSource;

/// How often the thread wakes to notice a track finished on its own. Also the
/// upper bound on how late `Finished` can be.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Ceiling on how long a caller waits for the thread to answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

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
}

enum Command {
    Play {
        source: PlayableSource,
        /// Echoed back in `Finished`, so the coordinator can discard a report
        /// about a track it has already moved on from.
        epoch: u64,
        reply: Sender<Result<(), String>>,
    },
    Pause,
    Resume,
    Stop,
    /// Linear amplitude, already curved by the caller.
    SetVolume(f32),
    Position {
        reply: Sender<Duration>,
    },
    Seek {
        position: Duration,
        reply: Sender<Result<(), String>>,
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

    pub fn play(&self, source: PlayableSource, epoch: u64) -> Result<(), String> {
        let (reply, response) = mpsc::channel();
        self.send(Command::Play {
            source,
            epoch,
            reply,
        })?;

        response
            .recv_timeout(REPLY_TIMEOUT)
            .map_err(|_| "The audio thread did not respond.".to_string())?
    }

    pub fn pause(&self) -> Result<(), String> {
        self.send(Command::Pause)
    }

    pub fn resume(&self) -> Result<(), String> {
        self.send(Command::Resume)
    }

    pub fn stop(&self) -> Result<(), String> {
        self.send(Command::Stop)
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
    pub fn seek(&self, position: Duration) -> Result<(), String> {
        let (reply, response) = mpsc::channel();
        self.send(Command::Seek { position, reply })?;

        response
            .recv_timeout(REPLY_TIMEOUT)
            .map_err(|_| "The audio thread did not respond to the seek.".to_string())?
    }

    /// Position within the current track. Zero when nothing is playing.
    pub fn position(&self) -> Duration {
        let (reply, response) = mpsc::channel();
        if self.send(Command::Position { reply }).is_err() {
            return Duration::ZERO;
        }
        response.recv_timeout(REPLY_TIMEOUT).unwrap_or(Duration::ZERO)
    }
}

pub fn spawn(events: UnboundedSender<EngineEvent>) -> AudioEngine {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("audio".to_string())
        .spawn(move || run(rx, events))
        .expect("audio thread should spawn");

    AudioEngine { tx: Mutex::new(tx) }
}

fn run(rx: Receiver<Command>, events: UnboundedSender<EngineEvent>) {
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

    loop {
        match rx.recv_timeout(POLL_INTERVAL) {
            Ok(Command::Play {
                source,
                epoch: new_epoch,
                reply,
            }) => {
                let result = match &device {
                    Ok(sink) => start(sink, &source, volume),
                    Err(e) => Err(e.clone()),
                };

                match result {
                    Ok(new_player) => {
                        // Dropping the previous player marks it stopped, which
                        // is what makes "replace what is playing" work --
                        // `append` alone would queue the new track behind it.
                        player = Some(new_player);
                        epoch = Some(new_epoch);
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

            Ok(Command::Stop) => {
                // Clear the epoch before dropping so end-detection below cannot
                // mistake a deliberate stop for a track finishing.
                epoch = None;
                player = None;
            }

            Ok(Command::SetVolume(linear)) => {
                volume = linear;
                if let Some(p) = &player {
                    p.set_volume(linear);
                }
            }

            Ok(Command::Position { reply }) => {
                let position = player.as_ref().map_or(Duration::ZERO, |p| p.get_pos());
                let _ = reply.send(position);
            }

            Ok(Command::Seek { position, reply }) => {
                let result = match &player {
                    Some(p) => p
                        .try_seek(position)
                        .map_err(|e| format!("Could not seek in this track: {e}")),
                    None => Err("Nothing is playing.".to_string()),
                };
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
                        let _ = events.send(EngineEvent::Progress {
                            epoch: current,
                            position: p.get_pos(),
                        });
                    }
                }
            }

            // The handle was dropped: the app is shutting down.
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn start(
    sink: &rodio::MixerDeviceSink,
    source: &PlayableSource,
    volume: f32,
) -> Result<Player, String> {
    let PlayableSource::LocalFile(path) = source;

    let file = File::open(path).map_err(|e| format!("Could not open the file: {e}"))?;
    let decoder =
        Decoder::try_from(file).map_err(|e| format!("Could not decode the audio: {e}"))?;

    let player = Player::connect_new(sink.mixer());
    player.set_volume(volume);
    player.append(decoder);

    Ok(player)
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

    #[test]
    fn progress_events_advance_while_a_track_plays() {
        let dir = std::env::temp_dir().join("music-app-engine-progress");
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        write_wav(&wav, 3);

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let engine = spawn(tx);

        let played = engine.play(PlayableSource::LocalFile(wav), 1);
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
