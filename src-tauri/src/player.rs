use std::collections::HashMap;
use std::time::Duration;

use serde::Serialize;
use sqlx::{QueryBuilder, Row, SqlitePool};
use tauri::{AppHandle, Emitter, Runtime, State};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::engine::{self, AudioEngine, EngineEvent};
use crate::playable::{get_playable_source, PlayableTrack};
use crate::queue::{PlayerQueue, RepeatMode};

pub const PLAYER_STATE_EVENT: &str = "player-state";
pub const PLAYER_ERROR_EVENT: &str = "player-error";
pub const PLAYER_PROGRESS_EVENT: &str = "player-progress";
pub const PLAYER_QUEUE_EVENT: &str = "player-queue";

/// Below this, Previous goes to the previous track; above it, Previous
/// restarts the current one. Matches what every other player does.
const RESTART_THRESHOLD: Duration = Duration::from_secs(3);

/// How many upcoming context tracks the preview carries.
///
/// Capped because the context is often the whole library: shipping thousands
/// of rows over IPC on every queue change to render a list nobody scrolls to
/// the bottom of would be pure waste.
const PREVIEW_LIMIT: usize = 50;

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
    /// Deliberately the *context*, not the total. The manual queue is not
    /// positional -- "3 of 12" would be a lie the moment anything is queued.
    pub context_length: usize,
    pub context_position: usize,
    /// Enough for a badge on the queue button without subscribing to the
    /// heavier queue event.
    pub manual_length: usize,
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

/// A row in the queue panel, hydrated from the database.
///
/// The panel cannot look these up itself: a queued YouTube result may have
/// been saved seconds ago and is not in whatever list the frontend has loaded.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    /// Present only for manual entries -- they are the only removable,
    /// reorderable rows, and this is what addresses them.
    pub entry_id: Option<u64>,
    pub track_id: i64,
    pub title: String,
    pub artist: Option<String>,
    pub duration_secs: Option<i64>,
    /// "present" / "missing" / "saved" / "downloaded", as in the library.
    pub state: String,
    pub source: String,
}

/// The whole "Up Next" panel in one payload.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueState {
    pub current: Option<QueueEntry>,
    /// Tracks the user explicitly queued, in play order.
    pub manual: Vec<QueueEntry>,
    /// Preview of the context continuation, capped at `PREVIEW_LIMIT`.
    pub up_next: Vec<QueueEntry>,
    pub context_name: Option<String>,
    /// Context tracks beyond the preview, so the UI can say "and N more".
    pub context_remaining: usize,
}

#[derive(Debug)]
pub enum PlayerCommand {
    PlayQueue {
        track_ids: Vec<i64>,
        start_index: usize,
        /// Shown as "Next from …". `None` leaves the heading generic.
        context_name: Option<String>,
    },
    /// Queue to the front — plays after the current track.
    PlayNext(i64),
    /// Queue to the back.
    AddToQueue(i64),
    RemoveFromQueue(u64),
    ReorderQueue {
        entry_id: u64,
        to_index: usize,
    },
    ClearQueue,
    /// Asks for a `player-queue` event, for a panel that just mounted.
    RequestQueueState,
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

impl PlayerCommand {
    /// Whether handling this can change what the queue panel shows.
    ///
    /// Volume and seek arrive per pixel of slider travel, and rebuilding the
    /// queue payload costs a database round trip, so they are excluded rather
    /// than emitting on everything.
    fn affects_queue(&self) -> bool {
        !matches!(
            self,
            PlayerCommand::SetVolume(_) | PlayerCommand::SetMuted(_) | PlayerCommand::Seek(_)
        )
    }
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
    fn queue(&self, queue: QueueState);
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

    fn queue(&self, queue: QueueState) {
        let _ = self.emit(PLAYER_QUEUE_EVENT, queue);
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
    queue: PlayerQueue,
    state: PlaybackState,
    /// The track the engine actually holds decoded, which is not always
    /// `queue.current()` -- during resolution the queue has already moved on.
    /// Only equality with this makes a rewind safe instead of a reload.
    loaded: Option<i64>,
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
            queue: PlayerQueue::default(),
            state: PlaybackState::Stopped,
            loaded: None,
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
        let affects_queue = command.affects_queue();

        match command {
            PlayerCommand::PlayQueue {
                track_ids,
                start_index,
                context_name,
            } => {
                // Deliberately does not clear the manual queue: tracks the
                // user interposed are a separate intention from "play this
                // playlist".
                let target = self.queue.set_context(track_ids, start_index, context_name);
                self.start(target).await;
            }

            PlayerCommand::PlayNext(track_id) => {
                self.queue.enqueue_next(track_id);
                self.start_if_idle().await;
            }

            PlayerCommand::AddToQueue(track_id) => {
                self.queue.enqueue_last(track_id);
                self.start_if_idle().await;
            }

            PlayerCommand::RemoveFromQueue(entry_id) => {
                self.queue.remove_manual(entry_id);
            }

            PlayerCommand::ReorderQueue { entry_id, to_index } => {
                self.queue.reorder_manual(entry_id, to_index);
            }

            PlayerCommand::ClearQueue => self.queue.clear_manual(),

            PlayerCommand::RequestQueueState => {}

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
                    let target = self.queue.current();
                    self.start(target).await;
                }
            },

            PlayerCommand::Next => {
                // The same call the natural end uses, so pressing Next and
                // letting a track run out cannot disagree about what is next.
                match self.queue.on_next() {
                    Some(track_id) => self.start(Some(track_id)).await,
                    None => self.halt(),
                }
            }

            PlayerCommand::Previous => {
                // Past the threshold, Previous restarts the current track
                // instead of leaving it.
                let restart = self.state != PlaybackState::Stopped
                    && self.engine.position() > RESTART_THRESHOLD;

                let target = if restart {
                    self.queue.current()
                } else {
                    self.queue.on_previous()
                };

                self.rewind_or_start(target).await;
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

            // Both change the preview, so both re-emit the queue.
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
        if affects_queue {
            self.emit_queue().await;
        }
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

                // The engine dropped its player, so nothing is decoded any
                // more; a rewind is not available even for repeat-one.
                self.loaded = None;

                match self.queue.on_finished() {
                    Some(track_id) => self.start(Some(track_id)).await,
                    None => self.halt(),
                }

                self.emit_state();
                self.emit_queue().await;
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

    /// Queueing something while nothing is playing should start playing it.
    ///
    /// Without this, "add to queue" on a fresh launch appears to do nothing:
    /// there is no context to advance into, so nothing would ever pick the
    /// entry up.
    async fn start_if_idle(&mut self) {
        if self.state != PlaybackState::Stopped {
            return;
        }
        if let Some(track_id) = self.queue.on_next() {
            self.start(Some(track_id)).await;
        }
    }

    /// Restarts `target` if it is already decoded, otherwise loads it.
    ///
    /// Reloading to go back to the start of a track costs a full resolve --
    /// for a YouTube track that is another yt-dlp round trip, seconds of
    /// silence to rewind something already in hand.
    async fn rewind_or_start(&mut self, target: Option<i64>) {
        let already_loaded =
            target.is_some() && target == self.loaded && self.state != PlaybackState::Stopped;

        if already_loaded {
            match self.engine.seek(Duration::ZERO) {
                Ok(()) => {
                    self.emit_progress(Duration::ZERO);
                    return;
                }
                // Streams cannot seek at all -- `FfmpegSource` has no
                // `try_seek` -- so for them reloading *is* the rewind. Falling
                // through is the whole point of not reporting this error.
                Err(_) => {}
            }
        }

        self.start(target).await;
    }

    /// Plays `first`, skipping over tracks that will not resolve.
    ///
    /// A queue can contain tracks whose files vanished since the last scan.
    /// Stopping dead on the first would be worse than skipping, but skipping
    /// must be bounded or a queue of entirely missing tracks would spin
    /// forever -- with repeat on, the context never runs out.
    async fn start(&mut self, first: Option<i64>) {
        // Bounded far below the queue length on purpose. A local file fails
        // instantly, but resolving a YouTube stream takes ~7 seconds, so
        // walking a long queue of dead links would freeze playback for
        // minutes with no explanation.
        const MAX_ATTEMPTS: usize = 3;

        let mut candidate = first;
        let mut last_error = None;

        for _ in 0..MAX_ATTEMPTS {
            let Some(track_id) = candidate else {
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
                            self.loaded = Some(track_id);
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

            // Skipping a dead track goes through the same priority rule as a
            // natural end, so an unplayable context track cannot swallow the
            // tracks the user queued behind it.
            candidate = self.queue.on_next();
        }

        self.halt();

        if let Some(error) = last_error {
            self.events.error(error);
        }
    }

    async fn resolve(&self, track_id: i64) -> Result<crate::playable::PlayableSource, String> {
        let row =
            sqlx::query("SELECT source, state, local_path, remote_url FROM tracks WHERE id = ?")
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
                remote_url: row.get("remote_url"),
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
        self.loaded = None;
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
            track_id: self.queue.current(),
            repeat: self.queue.repeat(),
            shuffle: self.queue.is_shuffled(),
            volume: self.volume,
            muted: self.muted,
            context_length: self.queue.context_len(),
            context_position: self.queue.context_position(),
            manual_length: self.queue.manual_len(),
        });
    }

    async fn emit_queue(&self) {
        self.events.queue(self.queue_state().await);
    }

    /// Builds the panel payload, hydrating titles in one query.
    async fn queue_state(&self) -> QueueState {
        let manual: Vec<(Option<u64>, i64)> = self
            .queue
            .manual()
            .map(|entry| (Some(entry.entry_id), entry.track_id))
            .collect();
        let up_next = self.queue.context_upcoming(PREVIEW_LIMIT);
        let current = self.queue.current();

        let mut ids: Vec<i64> = manual.iter().map(|(_, id)| *id).collect();
        ids.extend(up_next.iter().copied());
        ids.extend(current);
        ids.sort_unstable();
        ids.dedup();

        let details = self.load_details(&ids).await;

        QueueState {
            current: current.map(|id| entry_for(&details, None, id)),
            manual: manual
                .into_iter()
                .map(|(entry_id, id)| entry_for(&details, entry_id, id))
                .collect(),
            up_next: up_next
                .iter()
                .map(|&id| entry_for(&details, None, id))
                .collect(),
            context_name: self.queue.context_name().map(str::to_owned),
            context_remaining: self
                .queue
                .context_upcoming_total()
                .saturating_sub(up_next.len()),
        }
    }

    async fn load_details(&self, ids: &[i64]) -> HashMap<i64, TrackDetail> {
        if ids.is_empty() {
            return HashMap::new();
        }

        let mut builder = QueryBuilder::new(
            "SELECT id, source, title, artist, duration_secs, state FROM tracks WHERE id IN (",
        );
        let mut separated = builder.separated(", ");
        for id in ids {
            separated.push_bind(*id);
        }
        builder.push(")");

        let rows = match builder.build().fetch_all(&self.pool).await {
            Ok(rows) => rows,
            // The panel is cosmetic; a failed lookup should not take playback
            // down with it. Every row falls back to its placeholder.
            Err(_) => return HashMap::new(),
        };

        rows.into_iter()
            .map(|row| {
                let id: i64 = row.get("id");
                (
                    id,
                    TrackDetail {
                        source: row.get("source"),
                        title: row.get("title"),
                        artist: row.get("artist"),
                        duration_secs: row.get("duration_secs"),
                        state: row.get("state"),
                    },
                )
            })
            .collect()
    }
}

struct TrackDetail {
    source: String,
    title: String,
    artist: Option<String>,
    duration_secs: Option<i64>,
    state: String,
}

/// Turns an id into a row, keeping deleted tracks visible.
///
/// A queued track whose row has since been deleted still occupies a slot in
/// the queue, and silently dropping it would leave the panel disagreeing with
/// what actually plays.
fn entry_for(details: &HashMap<i64, TrackDetail>, entry_id: Option<u64>, track_id: i64) -> QueueEntry {
    match details.get(&track_id) {
        Some(detail) => QueueEntry {
            entry_id,
            track_id,
            title: detail.title.clone(),
            artist: detail.artist.clone(),
            duration_secs: detail.duration_secs,
            state: detail.state.clone(),
            source: detail.source.clone(),
        },
        None => QueueEntry {
            entry_id,
            track_id,
            title: "Unavailable".to_string(),
            artist: None,
            duration_secs: None,
            state: "missing".to_string(),
            source: "local".to_string(),
        },
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
    context_name: Option<String>,
    player: State<'_, PlayerHandle>,
) -> Result<(), String> {
    player.send(PlayerCommand::PlayQueue {
        track_ids,
        start_index,
        context_name,
    })
}

#[tauri::command]
pub async fn play_next(track_id: i64, player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::PlayNext(track_id))
}

#[tauri::command]
pub async fn add_to_queue(track_id: i64, player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::AddToQueue(track_id))
}

#[tauri::command]
pub async fn remove_from_queue(entry_id: u64, player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::RemoveFromQueue(entry_id))
}

#[tauri::command]
pub async fn reorder_queue(
    entry_id: u64,
    to_index: usize,
    player: State<'_, PlayerHandle>,
) -> Result<(), String> {
    player.send(PlayerCommand::ReorderQueue { entry_id, to_index })
}

#[tauri::command]
pub async fn clear_queue(player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::ClearQueue)
}

/// Asks the coordinator to re-emit `player-queue`.
///
/// Deliberately not a return value: the panel already listens for the event,
/// and having two ways to learn the same thing is how they drift apart.
#[tauri::command]
pub async fn request_queue_state(player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::RequestQueueState)
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

    #[test]
    fn the_high_frequency_commands_do_not_rebuild_the_queue_payload() {
        // Volume arrives per pixel of slider travel; rebuilding the panel
        // payload costs a database round trip each time.
        assert!(!PlayerCommand::SetVolume(0.5).affects_queue());
        assert!(!PlayerCommand::SetMuted(true).affects_queue());
        assert!(!PlayerCommand::Seek(12.0).affects_queue());
    }

    #[test]
    fn shuffle_and_repeat_do_rebuild_it() {
        // Both change what the "Next from …" preview should show.
        assert!(PlayerCommand::SetShuffle(true).affects_queue());
        assert!(PlayerCommand::SetRepeat(RepeatMode::All).affects_queue());
        assert!(PlayerCommand::AddToQueue(1).affects_queue());
    }
}
