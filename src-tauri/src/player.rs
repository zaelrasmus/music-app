use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use sqlx::{QueryBuilder, Row, SqlitePool};
use tauri::{AppHandle, Emitter, Runtime, State};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::engine::{self, AudioEngine, EngineEvent, SUPERSEDED};
use crate::audio_cache::AudioCache;
use crate::playable::{get_playable_source, PlayableSource, PlayableTrack};
use crate::providers::Provider;
use crate::queue::PlayerQueue;

/// Re-exported because it already forms part of this module public surface --
/// `PlayerCommand::SetRepeat` takes one and `PlayerStatus` returns one -- and
/// a type you can be handed but cannot name is no use to a caller.
pub use crate::queue::RepeatMode;
use crate::stream_urls::StreamUrlCache;

pub const PLAYER_STATE_EVENT: &str = "player-state";
pub const PLAYER_ERROR_EVENT: &str = "player-error";
pub const PLAYER_PROGRESS_EVENT: &str = "player-progress";
pub const PLAYER_QUEUE_EVENT: &str = "player-queue";

/// Below this, Previous goes to the previous track; above it, Previous
/// restarts the current one. Matches what every other player does.
const RESTART_THRESHOLD: Duration = Duration::from_secs(3);

/// How long a track must play before it counts as played.
///
/// Counting on *start* would fill the history with everything skipped past
/// while looking for something, which is the opposite of what a recently
/// played list is for. Thirty seconds is the convention, and short tracks are
/// covered by counting a natural end regardless.
const PLAY_COUNTS_AFTER: Duration = Duration::from_secs(30);

/// How much of a track must have played before an abandoned listen is worth
/// fetching a complete copy of.
///
/// Half is already a strong signal, and there is no over-triggering to guard
/// against: this is only ever evaluated at the moment a track is *left*, so a
/// listen that runs to the end never reaches it -- the decoder's own copy has
/// already been committed for free.
const OFFLINE_COPY_AFTER: f64 = 0.5;

/// Longest track worth fetching a second time.
///
/// The cost here is the *whole* track, not the remainder, so the guard is on
/// total length. An hour-long upload someone sampled is not worth fifty
/// megabytes of background traffic.
const OFFLINE_COPY_MAX_SECS: f64 = 20.0 * 60.0;

/// How long a stall may last before playback is abandoned.
///
/// Generous, because the buffer already absorbs anything short and a wifi
/// handover can take a while -- but bounded, because silence forever is not a
/// state anyone should have to sit in.
const STALL_GIVE_UP: Duration = Duration::from_secs(30);

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
    /// The stream has run dry without ending -- the connection has stopped
    /// keeping up. Distinct from loading, which is a track that has not
    /// started yet.
    pub stalled: bool,
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
    /// Puts a track back in the bar at the position it was left, without
    /// loading it. Sent once at startup.
    Restore {
        track_id: i64,
        position_secs: f64,
    },
    TogglePlayPause,
    Next,
    Previous,
    Stop,
    SetVolume(f32),
    SetMuted(bool),
    SetRepeat(RepeatMode),
    SetShuffle(bool),
    SetKeepAbandoned(bool),
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
    /// Shared: a load runs on its own task and reaches the engine from there.
    engine: Arc<AudioEngine>,
    /// Where those tasks report back.
    loads: UnboundedSender<LoadOutcome>,
    pool: SqlitePool,
    /// Handed in at startup: the coordinator has no `AppHandle` to resolve it
    /// from, and doing so per play would repeat the lookup needlessly.
    yt_dlp: Option<std::path::PathBuf>,
    /// Resolved stream URLs, reused for their stated lifetime.
    ///
    /// Shared because prefetching writes into it from a spawned task.
    stream_urls: Arc<StreamUrlCache>,
    /// The track a prefetch is currently resolving, if any.
    ///
    /// One slot, not a set: the point is to warm the *next* track, and
    /// allowing several at once would mean skipping through a queue spawns a
    /// yt-dlp process per keypress, all but one of them wasted.
    prefetching: Arc<Mutex<Option<i64>>>,
    /// Disposable on-disk copies of streamed audio. None disables caching.
    audio_cache: Option<AudioCache>,
    /// Needed to build a decoder here rather than on the audio thread.
    ffmpeg: Option<std::path::PathBuf>,
    /// The next track, already decoding.
    ///
    /// One at a time: each holds an ffmpeg process and a full ring buffer, and
    /// only the immediate next track is worth that.
    prepared: Option<Prepared>,
    /// Where prepare tasks deliver.
    prepares: UnboundedSender<Prepared>,
    /// Whether to fetch a complete copy of tracks abandoned part-way.
    ///
    /// Off unless asked for: it spends data the user cannot see being spent.
    keep_abandoned: bool,
    /// How far the current track has played, for judging that.
    last_position: Duration,
    /// Whether this play has already been recorded, so a long listen counts
    /// once rather than on every tick.
    recorded_play: bool,
    /// Whether the current decode began at the very start of the track.
    ///
    /// Only such a decode writes a cache copy, so a track that was seeked in
    /// reaches its end with nothing kept -- and a natural end is not an
    /// abandonment, so nothing else would notice.
    covered_from_zero: bool,
    /// Tracks already being fetched, so leaving the same one twice does not
    /// start two downloads of it.
    fetching: Arc<Mutex<std::collections::HashSet<i64>>>,
    /// Whether the current decoder is starving, and since when.
    stalled: bool,
    stalled_since: Option<std::time::Instant>,
    /// Where to begin the next load, when the track is being resumed.
    ///
    /// Applied by the load rather than as a seek afterwards, so a stream does
    /// not start ffmpeg once at zero and again at the real position.
    resume_at: Option<Duration>,
    events: E,
}

/// Starts the engine thread and the coordinator task.
pub fn spawn<E: PlayerEvents>(
    events: E,
    pool: SqlitePool,
    ffmpeg: Option<std::path::PathBuf>,
    yt_dlp: Option<std::path::PathBuf>,
    audio_cache: Option<AudioCache>,
) -> PlayerHandle {
    let (engine_tx, engine_rx) = mpsc::unbounded_channel();
    let engine = engine::spawn(engine_tx, ffmpeg.clone());

    let (command_tx, command_rx) = mpsc::unbounded_channel();
    let (loads_tx, loads_rx) = mpsc::unbounded_channel();
    let (prepares_tx, prepares_rx) = mpsc::unbounded_channel();

    tauri::async_runtime::spawn(async move {
        Coordinator {
            queue: PlayerQueue::default(),
            state: PlaybackState::Stopped,
            loaded: None,
            volume: 1.0,
            muted: false,
            epoch: 0,
            engine: Arc::new(engine),
            loads: loads_tx,
            pool,
            yt_dlp,
            stream_urls: Arc::new(StreamUrlCache::default()),
            prefetching: Arc::new(Mutex::new(None)),
            audio_cache,
            ffmpeg,
            prepared: None,
            prepares: prepares_tx,
            keep_abandoned: false,
            last_position: Duration::ZERO,
            recorded_play: false,
            covered_from_zero: true,
            fetching: Arc::new(Mutex::new(std::collections::HashSet::new())),
            stalled: false,
            stalled_since: None,
            resume_at: None,
            events,
        }
        .run(command_rx, engine_rx, loads_rx, prepares_rx)
        .await;
    });

    PlayerHandle { tx: command_tx }
}

impl<E: PlayerEvents> Coordinator<E> {
    async fn run(
        mut self,
        mut commands: UnboundedReceiver<PlayerCommand>,
        mut engine_events: UnboundedReceiver<EngineEvent>,
        mut loads: UnboundedReceiver<LoadOutcome>,
        mut prepares: UnboundedReceiver<Prepared>,
    ) {
        loop {
            tokio::select! {
                Some(command) = commands.recv() => self.handle_command(command).await,
                Some(event) = engine_events.recv() => self.handle_engine_event(event).await,
                Some(outcome) = loads.recv() => self.handle_load(outcome).await,
                Some(ready) = prepares.recv() => self.keep_prepared(ready),
                else => break,
            }
        }
    }

    /// Files a decoder that finished preparing.
    ///
    /// Discarded if the queue moved on while it was building -- dropping it
    /// kills its ffmpeg, so a stale one costs nothing but the work already
    /// done.
    fn keep_prepared(&mut self, ready: Prepared) {
        if self.queue.peek_next() == Some(ready.track_id) {
            self.prepared = Some(ready);
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
                self.consider_offline_copy();
                let target = self.queue.set_context(track_ids, start_index, context_name);
                self.start(target);
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

            PlayerCommand::Restore {
                track_id,
                position_secs,
            } => self.restore(track_id, position_secs).await,

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
                // Already resolving: a second load would race the first for
                // the output, and the epoch guard would simply discard one of
                // them. Waiting is the honest behaviour.
                PlaybackState::Loading => {}
                PlaybackState::Stopped => {
                    let target = self.queue.current();
                    self.start(target);
                }
            },

            PlayerCommand::Next => {
                self.consider_offline_copy();
                // The same call the natural end uses, so pressing Next and
                // letting a track run out cannot disagree about what is next.
                match self.queue.on_next() {
                    Some(track_id) => self.start(Some(track_id)),
                    None => self.halt(),
                }
            }

            PlayerCommand::Previous => {
                // Past the threshold, Previous restarts the current track
                // instead of leaving it.
                let restart = self.state != PlaybackState::Stopped
                    && self.engine.position().await > RESTART_THRESHOLD;

                let target = if restart {
                    self.queue.current()
                } else {
                    self.queue.on_previous()
                };

                self.rewind_or_start(target).await;
            }

            PlayerCommand::Stop => {
                self.consider_offline_copy();
                self.halt();
            }

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

            PlayerCommand::SetKeepAbandoned(enabled) => self.keep_abandoned = enabled,

            PlayerCommand::Seek(seconds) => {
                if self.state != PlaybackState::Stopped {
                    let position = Duration::from_secs_f64(seconds.max(0.0));

                    // A local file seeks in place, but anything ffmpeg decodes
                    // has its decode restarted -- about 0.4s for a YouTube
                    // stream, 2s for SoundCloud's HLS. Saying so beats leaving
                    // the bar sitting on the old position looking frozen.
                    // Pausing must survive the round trip, hence restoring the
                    // previous state rather than assuming Playing.
                    let resume_to = self.state;
                    self.state = PlaybackState::Loading;
                    self.emit_state();
                    self.state = resume_to;

                    match self.engine.seek(position).await {
                        // Echo the new position straight away rather than
                        // leaving the bar stale until the next tick.
                        Ok(()) => {
                            self.covered_from_zero = false;
                            self.last_position = position;
                            self.emit_progress(position);
                        }
                        Err(e) => {
                            // Some sources genuinely cannot be restarted --
                            // say so and carry on playing from where we were.
                            self.events.error(e);
                        }
                    }
                }
            }
        }

        self.emit_state();
        if affects_queue {
            self.emit_queue().await;
            // What plays next may have just changed -- `play_next` inserts
            // ahead of everything, so a start-only trigger would miss it.
            self.prefetch_next();
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

                // Reaching the end counts however short the track was.
                self.record_play();

                // A track that was seeked in reaches its end having written
                // nothing: the decode never covered it from the start. That is
                // not an abandonment, so only this notices.
                if !self.covered_from_zero {
                    self.consider_offline_copy();
                }

                // The engine dropped its player, so nothing is decoded any
                // more; a rewind is not available even for repeat-one.
                self.loaded = None;

                match self.queue.on_finished() {
                    Some(track_id) => self.start(Some(track_id)),
                    None => self.halt(),
                }

                self.emit_state();
                self.emit_queue().await;
                self.prefetch_next();
            }

            EngineEvent::Stalled { epoch, stalled } => {
                if epoch == self.epoch {
                    self.handle_stall(stalled);
                }
            }

            EngineEvent::Progress { epoch, position } => {
                // Progress deliberately does not emit full state: at five ticks
                // a second that would churn the whole UI.
                if epoch == self.epoch {
                    self.last_position = position;
                    if position >= PLAY_COUNTS_AFTER {
                        self.record_play();
                    }
                    self.emit_progress(position);
                }
            }
        }
    }

    /// The decoder has run dry, or recovered.
    ///
    /// Playing silence with the bar still moving is the worst of both: it
    /// looks like the track is fine and it sounds like nothing. So a stall is
    /// shown for what it is, and one that goes on long enough stops rather
    /// than advancing -- skipping to the next track while offline just fails
    /// three more times and reports something unrelated.
    fn handle_stall(&mut self, stalled: bool) {
        if !stalled {
            self.stalled_since = None;
            if self.stalled {
                self.stalled = false;
                self.emit_state();
            }
            return;
        }

        let since = *self.stalled_since.get_or_insert_with(std::time::Instant::now);

        if !self.stalled {
            self.stalled = true;
            self.emit_state();
        }

        if since.elapsed() >= STALL_GIVE_UP {
            self.stalled = false;
            self.stalled_since = None;
            self.halt();
            self.emit_state();
            self.events.error(
                "Lost connection to the stream. Check your internet and try again."
                    .to_string(),
            );
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
            self.start(Some(track_id));
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
            match self.engine.seek(Duration::ZERO).await {
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

        self.start(target);
    }

    /// Begins playing `first`, without waiting for it.
    ///
    /// A load -- a yt-dlp resolve, then ffmpeg buffering -- takes seconds, and
    /// awaiting it here left the coordinator deaf to every command for the
    /// whole of it: pressing Pause or Next during a load did nothing. It now
    /// runs on its own task and reports back through [`LoadOutcome`], which
    /// the run loop treats as just another event.
    ///
    /// Correctness comes from the epoch that already existed for exactly this
    /// class of problem. A load carries the epoch it began under, and an
    /// outcome whose epoch has moved on is discarded the same way a stale
    /// `Finished` is.
    fn start(&mut self, first: Option<i64>) {
        self.begin_load(first, 0);
    }

    fn begin_load(&mut self, candidate: Option<i64>, attempt: usize) {
        let Some(track_id) = candidate else {
            self.halt();
            self.emit_state();
            return;
        };

        // Claims this load, which makes anything already in flight stale.
        self.epoch += 1;
        let epoch = self.epoch;

        // Consumed here: a resume applies to the track being resumed, and
        // must not leak into whatever plays after it.
        let start_at = self.resume_at.take().unwrap_or(Duration::ZERO);
        self.covered_from_zero = start_at.is_zero();
        self.recorded_play = false;

        // Prepared while the previous track was still playing: the process is
        // running and the buffer is full, so this is a hand-off rather than a
        // load. Everything the slow path does has already happened.
        //
        // Never used for a resume: a prepared decoder always starts at zero.
        if let Some(ready) = self
            .prepared
            .take()
            .filter(|r| r.track_id == track_id && start_at.is_zero())
        {
            let engine = Arc::clone(&self.engine);
            let loads = self.loads.clone();

            tauri::async_runtime::spawn(async move {
                let result = engine
                    .play(ready.source, Some(ready.decoded), Duration::ZERO, epoch)
                    .await;

                let _ = loads.send(LoadOutcome {
                    epoch,
                    track_id,
                    start_at: Duration::ZERO,
                    attempt,
                    result,
                });
            });
            return;
        }

        // Without this the UI would sit on "stopped" and look broken while a
        // stream is being fetched.
        self.state = PlaybackState::Loading;
        self.emit_state();

        let pool = self.pool.clone();
        let engine = Arc::clone(&self.engine);
        let urls = Arc::clone(&self.stream_urls);
        let cache = self.audio_cache.clone();
        let yt_dlp = self.yt_dlp.clone();
        let loads = self.loads.clone();

        tauri::async_runtime::spawn(async move {
            let result = load_track(
                &pool,
                &engine,
                &urls,
                cache.as_ref(),
                yt_dlp.as_deref(),
                track_id,
                start_at,
                epoch,
            )
            .await;

            let _ = loads.send(LoadOutcome {
                epoch,
                track_id,
                start_at,
                attempt,
                result,
            });
        });
    }

    /// A load finished -- possibly for a track the user has already left.
    async fn handle_load(&mut self, outcome: LoadOutcome) {
        if outcome.epoch != self.epoch {
            // Superseded. The audio thread refuses stale plays itself, so
            // nothing is sounding that should not be.
            return;
        }

        match outcome.result {
            Ok(()) => {
                self.loaded = Some(outcome.track_id);
                self.state = PlaybackState::Playing;
                self.stalled = false;
                self.stalled_since = None;
                self.apply_volume();
                // Set the bar immediately; the first tick is up to a poll
                // interval away, and for a resume starting from zero would
                // show the wrong place until it arrived.
                self.emit_progress(outcome.start_at);
                self.emit_state();
            }

            // Lost the race, and already knows it. Not a failure, and above
            // all not a reason to skip a track.
            Err(e) if e == SUPERSEDED => {}

            Err(e) => {
                let next_attempt = outcome.attempt + 1;

                // Skipping a dead track goes through the same priority rule as
                // a natural end, so an unplayable context track cannot swallow
                // the tracks the user queued behind it.
                let candidate = (next_attempt < MAX_LOAD_ATTEMPTS)
                    .then(|| self.queue.on_next())
                    .flatten();

                match candidate {
                    Some(_) => {
                        self.begin_load(candidate, next_attempt);
                        self.emit_queue().await;
                    }
                    None => {
                        self.halt();
                        self.emit_state();
                        self.events.error(e);
                    }
                }
            }
        }
    }

    /// Resolves the next track's stream while the current one is still
    /// playing.
    ///
    /// The gap between two streamed tracks *was* the resolve -- six or seven
    /// seconds of silence with nothing to show for it. Doing that work ahead
    /// of time removes it, because by the time the track ends its URL is
    /// already in the cache and only ffmpeg's own start remains.
    ///
    /// Best effort throughout: failures are silent, because the real play
    /// resolves normally and reports anything that genuinely matters. The
    /// worst case is that a track starts exactly as slowly as it used to.
    fn prefetch_next(&mut self) {
        let Some(track_id) = self.queue.peek_next() else {
            // Nothing follows: release whatever was held for a track that is
            // no longer next, so its ffmpeg does not linger.
            self.prepared = None;
            return;
        };

        // What is held no longer matches what is coming.
        if self.prepared.as_ref().is_some_and(|r| r.track_id != track_id) {
            self.prepared = None;
        }

        self.prepare_next(track_id);

        let Some(yt_dlp) = self.yt_dlp.clone() else {
            return;
        };

        // Claim the single slot, or leave it to whoever holds it.
        {
            let Ok(mut slot) = self.prefetching.lock() else {
                return;
            };
            if slot.is_some() {
                return;
            }
            *slot = Some(track_id);
        }

        let pool = self.pool.clone();
        let urls = Arc::clone(&self.stream_urls);
        let slot = Arc::clone(&self.prefetching);

        // Spawned rather than awaited: the coordinator must stay responsive,
        // and nothing depends on the result arriving.
        tauri::async_runtime::spawn(async move {
            prefetch(&pool, &urls, &yt_dlp, track_id).await;

            // Released whatever happened, so one failure cannot wedge every
            // later prefetch.
            if let Ok(mut slot) = slot.lock() {
                *slot = None;
            }
        });
    }

    /// Puts a track back in the bar where it was left, without loading it.
    ///
    /// Loading here would mean a yt-dlp resolve before the window is usable --
    /// seconds of work for a track the user may never press play on. The
    /// position is held instead and applied to the load if and when they do.
    async fn restore(&mut self, track_id: i64, position_secs: f64) {
        // Only worth restoring a position the user would notice losing, and
        // never one at the very end -- resuming there just plays silence and
        // skips on.
        const MIN_POSITION: f64 = 10.0;
        const END_MARGIN: f64 = 15.0;

        let duration: Option<f64> =
            sqlx::query_scalar("SELECT duration_secs FROM tracks WHERE id = ?")
                .bind(track_id)
                .fetch_optional(&self.pool)
                .await
                .ok()
                .flatten()
                .map(|secs: i64| secs as f64);

        // A track deleted since the last session would otherwise sit in the
        // bar as a phantom that fails only when pressed.
        let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten();
        if exists.is_none() {
            return;
        }

        let near_end = duration.is_some_and(|total| position_secs > total - END_MARGIN);
        let position = if position_secs < MIN_POSITION || near_end {
            0.0
        } else {
            position_secs
        };

        self.queue
            .set_context(vec![track_id], 0, Some("where you left off".to_string()));
        self.resume_at =
            (position > 0.0).then(|| Duration::from_secs_f64(position));

        // Stopped, not Loading: nothing has been fetched, and the bar should
        // show a track ready to play rather than one that is starting.
        self.state = PlaybackState::Stopped;
        // State first: the frontend discards a progress tick whose track it
        // does not yet know about, and it learns the track from the state.
        self.emit_state();
        self.emit_progress(Duration::from_secs_f64(position));
    }

    /// Notes that the current track was really listened to.
    ///
    /// Fire-and-forget: a history entry is not worth making anyone wait for,
    /// and losing one to a failed write costs nothing that matters.
    fn record_play(&mut self) {
        if self.recorded_play {
            return;
        }
        let Some(track_id) = self.loaded else {
            return;
        };
        self.recorded_play = true;

        let pool = self.pool.clone();
        tauri::async_runtime::spawn(async move {
            let _ = sqlx::query(
                "UPDATE tracks SET last_played = unixepoch(), \
                 play_count = play_count + 1 WHERE id = ?",
            )
            .bind(track_id)
            .execute(&pool)
            .await;
        });
    }

    /// A track is being left part-way through. Decide whether to keep it.
    ///
    /// Only ever called when the user abandons a track, never on a natural
    /// end -- which is the whole reason it wastes nothing. A listen that runs
    /// to completion has already produced a cache entry for free, as a second
    /// output of the decode; this exists for the listens that cannot, because
    /// they were cut short or because a seek meant the decode never covered
    /// the track from its start.
    fn consider_offline_copy(&mut self) {
        if !self.keep_abandoned {
            return;
        }

        let Some(track_id) = self.loaded else {
            return;
        };
        let position = self.last_position.as_secs_f64();

        // Reset now: whatever happens next belongs to a different track.
        self.last_position = Duration::ZERO;

        let (Some(yt_dlp), Some(ffmpeg), Some(cache)) = (
            self.yt_dlp.clone(),
            self.ffmpeg.clone(),
            self.audio_cache.clone(),
        ) else {
            return;
        };

        let pool = self.pool.clone();
        let fetching = Arc::clone(&self.fetching);

        tauri::async_runtime::spawn(async move {
            let Ok(Some(row)) = sqlx::query(
                "SELECT source, state, remote_id, remote_url, duration_secs \
                 FROM tracks WHERE id = ?",
            )
            .bind(track_id)
            .fetch_optional(&pool)
            .await
            else {
                return;
            };

            let source: String = row.get("source");
            let state: String = row.get("state");
            // Local files need nothing, and a downloaded track already has a
            // permanent copy the user asked for.
            if Provider::from_source(&source).is_none() || state != "saved" {
                return;
            }

            // No duration means a live stream, which has no "whole track" to
            // fetch.
            let Some(duration) = row.get::<Option<i64>, _>("duration_secs") else {
                return;
            };
            let duration = duration as f64;
            if duration <= 0.0 || duration > OFFLINE_COPY_MAX_SECS {
                return;
            }
            if position < duration * OFFLINE_COPY_AFTER {
                return;
            }

            let (Some(remote_id), Some(remote_url)) = (
                row.get::<Option<String>, _>("remote_id"),
                row.get::<Option<String>, _>("remote_url"),
            ) else {
                return;
            };

            // Already cached, or already being fetched.
            let Some(pending) = cache.reserve_fetch(&source, &remote_id) else {
                return;
            };
            {
                let Ok(mut active) = fetching.lock() else {
                    return;
                };
                if !active.insert(track_id) {
                    return;
                }
            }

            // Silent either way: the user did not ask for this and must not be
            // told off for it failing.
            let _ = crate::download::fetch_into_cache(&yt_dlp, &ffmpeg, &remote_url, pending).await;

            if let Ok(mut active) = fetching.lock() {
                active.remove(&track_id);
            }
        });
    }

    /// Starts decoding the next track before it is needed.
    ///
    /// The remaining gap between tracks was ffmpeg's own start -- spawning the
    /// process and waiting for it to buffer. Doing that while the current
    /// track still plays turns the handover into an append.
    ///
    /// Best effort: if it fails or arrives late the ordinary load path runs
    /// exactly as before.
    fn prepare_next(&mut self, track_id: i64) {
        // Already held, or already building for this track.
        if self.prepared.as_ref().is_some_and(|r| r.track_id == track_id) {
            return;
        }

        let pool = self.pool.clone();
        let urls = Arc::clone(&self.stream_urls);
        let cache = self.audio_cache.clone();
        let yt_dlp = self.yt_dlp.clone();
        let ffmpeg = self.ffmpeg.clone();
        let prepares = self.prepares.clone();

        tauri::async_runtime::spawn(async move {
            let Ok(source) =
                resolve_track(&pool, &urls, cache.as_ref(), yt_dlp.as_deref(), track_id).await
            else {
                return;
            };

            // Blocking: building a decoder waits for ffmpeg to produce its
            // first half second. On a worker thread that is fine; on the
            // runtime it would stall every other task.
            let source_for_build = source.clone();
            let built = tauri::async_runtime::spawn_blocking(move || {
                engine::build_source(&source_for_build, ffmpeg.as_deref(), Duration::ZERO)
            })
            .await;

            if let Ok(Ok(decoded)) = built {
                let _ = prepares.send(Prepared {
                    track_id,
                    source,
                    decoded,
                });
            }
        });
    }

    fn halt(&mut self) {
        self.stalled = false;
        self.stalled_since = None;
        // Bump the epoch so any in-flight `Finished` for the stopped track is
        // discarded rather than triggering an advance.
        self.epoch += 1;
        self.state = PlaybackState::Stopped;
        self.loaded = None;
        self.report(self.engine.stop(self.epoch));
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
            stalled: self.stalled,
        });
    }

    /// Takes `&mut self` rather than `&self` deliberately.
    ///
    /// A shared reference held across an await makes the whole future require
    /// `Sync`, and the prepared decoder is a trait object that is `Send` but
    /// not `Sync`. An exclusive reference only asks for `Send`, which it has.
    async fn emit_queue(&mut self) {
        let state = self.queue_state().await;
        self.events.queue(state);
    }

    /// Builds the panel payload, hydrating titles in one query.
    async fn queue_state(&mut self) -> QueueState {
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

    async fn load_details(&mut self, ids: &[i64]) -> HashMap<i64, TrackDetail> {
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

/// Bounded far below the queue length on purpose. A local file fails
/// instantly, but resolving a stream takes seconds, so walking a long queue of
/// dead links would leave playback stalled for minutes with no explanation.
const MAX_LOAD_ATTEMPTS: usize = 3;

/// A decoder built ahead of time for the track that comes next.
///
/// Holding one keeps an ffmpeg process and a full ring buffer alive, which is
/// why there is only ever one. Dropping it kills the process.
pub struct Prepared {
    track_id: i64,
    source: PlayableSource,
    decoded: engine::BuiltSource,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("track_id", &self.track_id)
            .finish_non_exhaustive()
    }
}

/// What a spawned load reports back.
#[derive(Debug)]
pub struct LoadOutcome {
    /// The epoch it began under. Anything older than the coordinator's current
    /// epoch is a load the user has already moved past.
    epoch: u64,
    track_id: i64,
    /// Where the decode began, so the bar can show it at once rather than
    /// flashing zero before the first tick corrects it.
    start_at: Duration,
    /// How many tracks have already been skipped in this run, so the retry
    /// stays bounded across the tasks it is now spread over.
    attempt: usize,
    result: Result<(), String>,
}

/// Resolves `track_id` and hands it to the engine.
///
/// Retries once when a *cached* stream URL fails. Providers can revoke a link
/// before its stated expiry, and the alternative -- skipping to the next track
/// -- would punish the user for an optimisation they cannot see. Only worth
/// doing when something was actually cached: a first-time failure is the
/// provider's answer, and asking again just fails slower.
///
/// A free function because it runs on a task that outlives any borrow of the
/// coordinator: it owns everything it touches.
async fn load_track(
    pool: &SqlitePool,
    engine: &AudioEngine,
    urls: &StreamUrlCache,
    cache: Option<&AudioCache>,
    yt_dlp: Option<&std::path::Path>,
    track_id: i64,
    start_at: Duration,
    epoch: u64,
) -> Result<(), String> {
    const ATTEMPTS: usize = 2;

    let mut last_error = String::new();

    for attempt in 0..ATTEMPTS {
        let source = resolve_track(pool, urls, cache, yt_dlp, track_id).await?;

        match engine.play(source, None, start_at, epoch).await {
            Ok(()) => return Ok(()),
            // Another load won while this one was resolving. Retrying would
            // only lose again, and the caller must not treat it as a fault.
            Err(e) if e == SUPERSEDED => return Err(e),
            Err(e) => {
                last_error = e;

                let stale =
                    attempt + 1 < ATTEMPTS && forget_stream_url(pool, urls, track_id).await;
                if !stale {
                    break;
                }
            }
        }
    }

    Err(last_error)
}

async fn resolve_track(
    pool: &SqlitePool,
    urls: &StreamUrlCache,
    cache: Option<&AudioCache>,
    yt_dlp: Option<&std::path::Path>,
    track_id: i64,
) -> Result<crate::playable::PlayableSource, String> {
    let row = sqlx::query(
        "SELECT source, state, remote_id, local_path, remote_url FROM tracks WHERE id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?
    .ok_or("That track no longer exists.")?;

    get_playable_source(
        &PlayableTrack {
            source: row.get("source"),
            state: row.get("state"),
            remote_id: row.get("remote_id"),
            local_path: row.get("local_path"),
            remote_url: row.get("remote_url"),
        },
        yt_dlp,
        urls,
        cache,
    )
    .await
}

/// Drops any cached stream URL for `track_id`, reporting whether there was one
/// to drop.
async fn forget_stream_url(pool: &SqlitePool, urls: &StreamUrlCache, track_id: i64) -> bool {
    let url: Option<String> = sqlx::query_scalar("SELECT remote_url FROM tracks WHERE id = ?")
        .bind(track_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();

    url.is_some_and(|url| urls.invalidate(&url))
}

/// Warms the URL cache for `track_id`, if it is something that streams.
///
/// A free function rather than a method because it runs on a spawned task
/// that outlives the borrow -- it owns everything it touches.
async fn prefetch(
    pool: &SqlitePool,
    urls: &StreamUrlCache,
    yt_dlp: &std::path::Path,
    track_id: i64,
) {
    let Ok(Some(row)) =
        sqlx::query("SELECT source, local_path, remote_url FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_optional(pool)
            .await
    else {
        return;
    };

    // A local file has nothing to resolve, and a downloaded track plays from
    // disk -- prefetching either would spend a process on nothing.
    let source: String = row.get("source");
    let Some(provider) = Provider::from_source(&source) else {
        return;
    };
    if row.get::<Option<String>, _>("local_path").is_some() {
        return;
    }

    let Some(url) = row.get::<Option<String>, _>("remote_url") else {
        return;
    };
    // The same check the real path makes. Nobody is watching this result, so
    // a corrupt row must not reach a subprocess just because it is quiet here.
    if !provider.accepts_url(&url) {
        return;
    }

    // Already cached and still fresh costs nothing -- `resolve` returns
    // without spawning anything.
    let _ = urls.resolve(yt_dlp, &url).await;
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

/// Whether to keep a complete copy of tracks abandoned part-way through.
///
/// A data-usage decision, so it belongs to the user rather than to us. The
/// free path -- the copy written while a track plays to its end -- is
/// unaffected either way.
#[tauri::command]
pub async fn set_keep_abandoned(
    enabled: bool,
    player: State<'_, PlayerHandle>,
) -> Result<(), String> {
    player.send(PlayerCommand::SetKeepAbandoned(enabled))
}

/// Puts the last session's track back in the bar, at its position.
///
/// Deliberately does not start playing: reopening an app should not make
/// noise, and nothing is fetched until the user asks for it.
#[tauri::command]
pub async fn restore_playback(
    track_id: i64,
    position_secs: f64,
    player: State<'_, PlayerHandle>,
) -> Result<(), String> {
    player.send(PlayerCommand::Restore {
        track_id,
        position_secs,
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
