use std::time::Duration;

use serde::Serialize;
use sqlx::{Row, SqlitePool};
use tauri::{AppHandle, Emitter, Runtime, State};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::engine::{self, AudioEngine, EngineEvent};
use crate::playable::{get_playable_source, PlayableTrack};
use crate::queue::{Queue, RepeatMode};

pub const PLAYER_STATE_EVENT: &str = "player-state";
pub const PLAYER_ERROR_EVENT: &str = "player-error";
pub const PLAYER_PROGRESS_EVENT: &str = "player-progress";

/// Below this, Previous goes to the previous track; above it, Previous
/// restarts the current one. Matches what every other player does.
const RESTART_THRESHOLD: Duration = Duration::from_secs(3);

/// Slider 0.0 maps to silence; everything above maps into this dB range.
///
/// `set_volume` multiplies raw samples, but loudness is perceived roughly
/// logarithmically -- a linear slider puts almost all audible change in the
/// bottom fifth. Mapping through decibels is what makes the slider feel even.
/// At the midpoint this gives -20 dB, about 10% amplitude.
const MIN_DB: f32 = -40.0;

fn slider_to_linear(slider: f32, muted: bool) -> f32 {
    if muted {
        return 0.0;
    }
    let slider = slider.clamp(0.0, 1.0);
    if slider <= 0.0 {
        // -40 dB is quiet, not silent; the bottom of the slider must be silent.
        return 0.0;
    }
    rodio::math::db_to_linear(MIN_DB * (1.0 - slider))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackState {
    /// Resolving a source. Only YouTube streams stay here long enough to see.
    Loading,
    Playing,
    Paused,
    Stopped,
}

/// One consolidated snapshot. A single event keeps the frontend from having to
/// stitch several partial updates into a consistent view.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerStatus {
    pub state: PlaybackState,
    pub track_id: Option<i64>,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    /// The slider position (0..=1), not the amplitude sent to rodio.
    pub volume: f32,
    pub muted: bool,
    pub queue_length: usize,
    pub queue_position: usize,
}

/// Sent frequently while playing, so it is deliberately small and separate
/// from `PlayerStatus` — the UI should not re-render everything five times a
/// second just to move a progress bar.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerProgress {
    /// Which track the position belongs to, so a tick that arrives just after
    /// a track change cannot be applied to the wrong one.
    pub track_id: Option<i64>,
    pub position_secs: f64,
}

#[derive(Debug)]
pub enum PlayerCommand {
    PlayQueue {
        track_ids: Vec<i64>,
        start_index: usize,
    },
    TogglePlayPause,
    Next,
    Previous,
    Stop,
    SetVolume(f32),
    SetMuted(bool),
    SetRepeat(RepeatMode),
    SetShuffle(bool),
    Seek(f64),
}

/// Where the coordinator reports to.
///
/// The coordinator does not depend on Tauri directly, so its wiring can be
/// tested without a window -- which matters because the progress and
/// end-of-track paths are driven by the engine, never by a command, and so
/// cannot be exercised through the command API at all.
pub trait PlayerEvents: Send + Sync + 'static {
    fn state(&self, status: PlayerStatus);
    fn progress(&self, progress: PlayerProgress);
    fn error(&self, message: String);
}

impl<R: Runtime> PlayerEvents for AppHandle<R> {
    fn state(&self, status: PlayerStatus) {
        let _ = self.emit(PLAYER_STATE_EVENT, status);
    }

    fn progress(&self, progress: PlayerProgress) {
        let _ = self.emit(PLAYER_PROGRESS_EVENT, progress);
    }

    fn error(&self, message: String) {
        let _ = self.emit(PLAYER_ERROR_EVENT, message);
    }
}

/// Managed Tauri state. Holds only the channel to the coordinator.
pub struct PlayerHandle {
    tx: UnboundedSender<PlayerCommand>,
}

impl PlayerHandle {
    pub fn send(&self, command: PlayerCommand) -> Result<(), String> {
        self.tx
            .send(command)
            .map_err(|_| "The player is not running.".to_string())
    }
}

/// Owns every playback decision: what is queued, what plays next, repeat,
/// shuffle, volume.
///
/// Being the *sole* owner is the point. The engine reports "finished" and the
/// frontend sends commands, but only this task mutates the queue, so a track
/// ending at the same moment the user presses Next cannot advance twice.
struct Coordinator<E: PlayerEvents> {
    queue: Queue,
    state: PlaybackState,
    volume: f32,
    muted: bool,
    /// Incremented on every start. The engine echoes it back so a `Finished`
    /// for a track we already moved past can be discarded.
    epoch: u64,
    engine: AudioEngine,
    pool: SqlitePool,
    /// Handed in at startup: the coordinator has no `AppHandle` to resolve it
    /// from, and doing so per play would repeat the lookup needlessly.
    yt_dlp: Option<std::path::PathBuf>,
    events: E,
}

/// Starts the engine thread and the coordinator task.
pub fn spawn<E: PlayerEvents>(
    events: E,
    pool: SqlitePool,
    ffmpeg: Option<std::path::PathBuf>,
    yt_dlp: Option<std::path::PathBuf>,
) -> PlayerHandle {
    let (engine_tx, engine_rx) = mpsc::unbounded_channel();
    let engine = engine::spawn(engine_tx, ffmpeg);

    let (command_tx, command_rx) = mpsc::unbounded_channel();

    tauri::async_runtime::spawn(async move {
        Coordinator {
            queue: Queue::default(),
            state: PlaybackState::Stopped,
            volume: 1.0,
            muted: false,
            epoch: 0,
            engine,
            pool,
            yt_dlp,
            events,
        }
        .run(command_rx, engine_rx)
        .await;
    });

    PlayerHandle { tx: command_tx }
}

impl<E: PlayerEvents> Coordinator<E> {
    async fn run(
        mut self,
        mut commands: UnboundedReceiver<PlayerCommand>,
        mut engine_events: UnboundedReceiver<EngineEvent>,
    ) {
        loop {
            tokio::select! {
                Some(command) = commands.recv() => self.handle_command(command).await,
                Some(event) = engine_events.recv() => self.handle_engine_event(event).await,
                else => break,
            }
        }
    }

    async fn handle_command(&mut self, command: PlayerCommand) {
        match command {
            PlayerCommand::PlayQueue {
                track_ids,
                start_index,
            } => {
                self.queue.set(track_ids, start_index);
                self.start_current().await;
            }

            PlayerCommand::TogglePlayPause => match self.state {
                PlaybackState::Playing => {
                    if self.report(self.engine.pause()) {
                        self.state = PlaybackState::Paused;
                    }
                }
                PlaybackState::Paused => {
                    if self.report(self.engine.resume()) {
                        self.state = PlaybackState::Playing;
                    }
                }
                // Nothing loaded, or still resolving: start from the cursor.
                PlaybackState::Stopped | PlaybackState::Loading => {
                    self.start_current().await
                }
            },

            PlayerCommand::Next => {
                if self.queue.next_manual().is_some() {
                    self.start_current().await;
                } else {
                    self.halt();
                }
            }

            PlayerCommand::Previous => {
                // Past the threshold, Previous restarts the current track
                // instead of leaving it.
                let restart = self.state != PlaybackState::Stopped
                    && self.engine.position() > RESTART_THRESHOLD;

                if !restart {
                    self.queue.previous_manual();
                }
                self.start_current().await;
            }

            PlayerCommand::Stop => self.halt(),

            PlayerCommand::SetVolume(volume) => {
                self.volume = volume.clamp(0.0, 1.0);
                // Changing the volume while muted is an implicit unmute --
                // otherwise the slider appears dead.
                if self.muted && self.volume > 0.0 {
                    self.muted = false;
                }
                self.apply_volume();
            }

            PlayerCommand::SetMuted(muted) => {
                self.muted = muted;
                self.apply_volume();
            }

            PlayerCommand::SetRepeat(mode) => self.queue.set_repeat(mode),

            PlayerCommand::SetShuffle(on) => self.queue.set_shuffle(on),

            PlayerCommand::Seek(seconds) => {
                if self.state != PlaybackState::Stopped {
                    let position = Duration::from_secs_f64(seconds.max(0.0));
                    match self.engine.seek(position) {
                        // Echo the new position straight away rather than
                        // leaving the bar stale until the next tick.
                        Ok(()) => self.emit_progress(position),
                        Err(e) => {
                            // Seeking is genuinely unsupported for some
                            // sources; say so and carry on playing.
                            self.events.error(e);
                        }
                    }
                }
            }
        }

        self.emit_state();
    }

    async fn handle_engine_event(&mut self, event: EngineEvent) {
        match event {
            EngineEvent::Finished { epoch } => {
                // A report about a track we already moved past -- the user
                // pressed Next just as it ended. Ignoring it is what prevents
                // a double advance.
                if epoch != self.epoch {
                    return;
                }

                if self.queue.advance_natural().is_some() {
                    self.start_current().await;
                } else {
                    self.halt();
                }

                self.emit_state();
            }

            EngineEvent::Progress { epoch, position } => {
                // Progress deliberately does not emit full state: at five ticks
                // a second that would churn the whole UI.
                if epoch == self.epoch {
                    self.emit_progress(position);
                }
            }
        }
    }

    /// Plays the track at the cursor, skipping over ones that will not resolve.
    ///
    /// A queue can contain tracks whose files vanished since the last scan.
    /// Stopping dead on the first would be worse than skipping, but skipping
    /// must be bounded or a queue of entirely missing tracks would spin
    /// forever -- with repeat on, `next_manual` never returns `None`.
    async fn start_current(&mut self) {
        // Bounded far below the queue length on purpose. A local file fails
        // instantly, but resolving a YouTube stream takes ~7 seconds, so
        // walking a long queue of dead links would freeze playback for
        // minutes with no explanation.
        const MAX_ATTEMPTS: usize = 3;

        let attempts_allowed = self.queue.len().clamp(1, MAX_ATTEMPTS);
        let mut last_error = None;

        for _ in 0..attempts_allowed {
            let Some(track_id) = self.queue.current() else {
                break;
            };

            // Resolution can take seconds; without this the UI would sit on
            // "stopped" and look broken while a stream is being fetched.
            self.state = PlaybackState::Loading;
            self.emit_state();

            match self.resolve(track_id).await {
                Ok(source) => {
                    self.epoch += 1;
                    match self.engine.play(source, self.epoch) {
                        Ok(()) => {
                            self.state = PlaybackState::Playing;
                            self.apply_volume();
                            // Reset the bar immediately; the first tick is up
                            // to a poll interval away.
                            self.emit_progress(Duration::ZERO);
                            return;
                        }
                        Err(e) => last_error = Some(e),
                    }
                }
                Err(e) => last_error = Some(e),
            }

            if self.queue.next_manual().is_none() {
                break;
            }
        }

        self.halt();

        if let Some(error) = last_error {
            self.events.error(error);
        }
    }

    async fn resolve(&self, track_id: i64) -> Result<crate::playable::PlayableSource, String> {
        let row =
            sqlx::query("SELECT source, state, local_path, yt_video_id FROM tracks WHERE id = ?")
                .bind(track_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| e.to_string())?
                .ok_or("That track no longer exists.")?;

        get_playable_source(
            &PlayableTrack {
                source: row.get("source"),
                state: row.get("state"),
                local_path: row.get("local_path"),
                yt_video_id: row.get("yt_video_id"),
            },
            self.yt_dlp.as_deref(),
        )
        .await
    }

    fn halt(&mut self) {
        // Bump the epoch so any in-flight `Finished` for the stopped track is
        // discarded rather than triggering an advance.
        self.epoch += 1;
        self.state = PlaybackState::Stopped;
        self.report(self.engine.stop());
    }

    fn apply_volume(&self) {
        let linear = slider_to_linear(self.volume, self.muted);
        self.report(self.engine.set_volume(linear));
    }

    /// Surfaces an engine failure to the UI. Returns whether it succeeded.
    fn report(&self, result: Result<(), String>) -> bool {
        match result {
            Ok(()) => true,
            Err(e) => {
                self.events.error(e);
                false
            }
        }
    }

    fn emit_progress(&self, position: Duration) {
        self.events.progress(PlayerProgress {
            track_id: self.queue.current(),
            position_secs: position.as_secs_f64(),
        });
    }

    fn emit_state(&self) {
        self.events.state(PlayerStatus {
            state: self.state,
            track_id: self.queue.current().filter(|_| !self.queue.is_empty()),
            repeat: self.queue.repeat(),
            shuffle: self.queue.is_shuffled(),
            volume: self.volume,
            muted: self.muted,
            queue_length: self.queue.len(),
            queue_position: self.queue.position(),
        });
    }
}

// --- commands ----------------------------------------------------------
//
// Thin: every one forwards to the coordinator. Failures come back as
// `player-error` events rather than command results, because the interesting
// ones (a track that will not resolve) happen after the command returns.

#[tauri::command]
pub async fn play_queue(
    track_ids: Vec<i64>,
    start_index: usize,
    player: State<'_, PlayerHandle>,
) -> Result<(), String> {
    player.send(PlayerCommand::PlayQueue {
        track_ids,
        start_index,
    })
}

#[tauri::command]
pub async fn toggle_play_pause(player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::TogglePlayPause)
}

#[tauri::command]
pub async fn next_track(player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::Next)
}

#[tauri::command]
pub async fn previous_track(player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::Previous)
}

#[tauri::command]
pub async fn stop(player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::Stop)
}

#[tauri::command]
pub async fn set_volume(volume: f32, player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::SetVolume(volume))
}

#[tauri::command]
pub async fn set_muted(muted: bool, player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::SetMuted(muted))
}

#[tauri::command]
pub async fn set_repeat(mode: RepeatMode, player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::SetRepeat(mode))
}

#[tauri::command]
pub async fn set_shuffle(shuffle: bool, player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::SetShuffle(shuffle))
}

#[tauri::command]
pub async fn seek(position_secs: f64, player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::Seek(position_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_top_of_the_slider_is_unity_gain() {
        assert!((slider_to_linear(1.0, false) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn the_bottom_of_the_slider_is_truly_silent() {
        // -40 dB is quiet but audible, so zero must be special-cased.
        assert_eq!(slider_to_linear(0.0, false), 0.0);
    }

    #[test]
    fn muting_silences_regardless_of_the_slider() {
        assert_eq!(slider_to_linear(1.0, true), 0.0);
    }

    #[test]
    fn the_curve_is_perceptual_not_linear() {
        let half = slider_to_linear(0.5, false);
        // A linear mapping would give 0.5 here, which sounds barely quieter.
        assert!(half < 0.2, "midpoint should be well below half amplitude");
        assert!(half > 0.05, "but still clearly audible");
    }

    #[test]
    fn the_curve_rises_monotonically() {
        let mut previous = -1.0;
        for step in 0..=20 {
            let value = slider_to_linear(step as f32 / 20.0, false);
            assert!(value > previous, "volume must never dip as the slider rises");
            previous = value;
        }
    }
}
