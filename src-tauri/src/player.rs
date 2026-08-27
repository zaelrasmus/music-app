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
use crate::stream_urls::{Encoding, StreamUrlCache};

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
///
/// Forty, not sixty. Sixty was tried and was wrong.
///
/// The argument for widening it was that a comfortable level sat at slider
/// 0.13 and needed room either side. That rested on a guess -- that people
/// listen at about -35 dB -- and the guess was too quiet. Used, sixty put
/// -43 dB at slider 0.3 and -32 dB at slider 0.5, so half the travel was
/// inaudible and nothing started sounding like music until past the middle.
///
/// The range is what decides how much a given nudge changes: sixty decibels
/// across the same travel is 0.56 dB per percent against forty's 0.36. Wider
/// range, coarser control, and every correction between songs overshoots.
///
/// Forty puts -28 dB at slider 0.3 and -20 dB at slider 0.5, which is within
/// three decibels of the curve this shipped with and was not complained
/// about.
///
/// Those two numbers read -29 and -22 until 2026-08-22; they were measured when
/// the ceiling was -4 dB and went stale when it returned to unity. The whole
/// curve is printed by `volume_curve_report::print_the_volume_curve`, which is
/// where to check rather than trusting this paragraph.
const MIN_DB: f32 = -40.0;

/// Where the top of the slider sits by default, in decibels.
///
/// Unity, after a detour. It was briefly -4 dB, because a lossy decode to f32
/// is not clamped and brick-walled masters reconstruct above full scale, where
/// the device hard-clips them. The headroom did fix that, but it was the wrong
/// instrument: counted over this library, the worst track exceeds full scale on
/// 169 samples out of 13 million and a well-mastered one on none at all, so
/// every track was being attenuated to round off a handful of samples in a few.
/// Clipping is now caught by the limiter in `engine.rs`, which touches only what
/// would have been chopped off.
///
/// The number stays adjustable because the *other* reason to want headroom is
/// real and cannot be solved here: this library spans about ten decibels of
/// mastered loudness, and no single curve makes a -6 LUFS master and a -13 LUFS
/// master both comfortable. That is arithmetic, not tuning. What a ceiling can
/// honestly offer is a worst case the listener chooses for themselves --
/// see `MIN_CEILING_DB`.
const DEFAULT_CEILING_DB: f32 = 0.0;

/// The quietest the ceiling may be set to.
///
/// Twelve decibels is already a quarter of the amplitude, and past it the
/// slider stops being a volume control and starts being a fault.
const MIN_CEILING_DB: f32 = -12.0;

/// How far into a track it must have been left for the position to survive a
/// restart.
///
/// Below this, starting over costs less than two minutes and hands back the
/// whole song, which is what the user wanted when they pressed play on it.
const RESUME_MIN_POSITION: f64 = 2.0 * 60.0;

/// How much of a track must remain for resuming to beat starting over.
///
/// Under five minutes left is less than one more song, so there is nothing to
/// save by dropping the user into the middle of it.
const RESUME_MIN_REMAINING: f64 = 5.0 * 60.0;

/// Where a track restored from the last session should begin.
///
/// Resuming mid-track is borrowed from podcast and video players, and it is
/// right *there* because their content is long, linear and heard once. A song
/// is none of those things. Dropping someone into the last forty seconds of a
/// three-minute track they chose to play is not a convenience; it costs them
/// the song and asks them to drag the handle back to zero.
///
/// So a position is kept only when losing it would cost something real: the
/// listen was already long enough to be worth not repeating, *and* enough of
/// the track is left to be worth resuming to. Both together mean nothing under
/// seven minutes can ever resume — which is 96% of this library, and every
/// track anyone would call a song. What resumes is what the rule was invented
/// for: long uploads, mixes, sets, anything an hour deep.
///
/// An unknown duration is a live stream or a row with no metadata. Neither has
/// a position worth restoring, so it starts at the beginning like the majority.
fn resume_position(saved: f64, duration: Option<f64>) -> f64 {
    let worth_keeping = saved >= RESUME_MIN_POSITION
        && duration.is_some_and(|total| total - saved >= RESUME_MIN_REMAINING);

    if worth_keeping {
        saved
    } else {
        0.0
    }
}

/// Maps the slider onto a gain, with `ceiling_db` as the top of the range.
///
/// The ceiling compresses the range rather than attenuating on top of it, so
/// lowering it keeps the whole travel usable instead of pushing everything
/// towards the bottom stop -- which is the mistake that made a -60 dB range
/// unusable in the first place.
fn slider_to_linear(slider: f32, muted: bool, ceiling_db: f32) -> f32 {
    if muted {
        return 0.0;
    }
    let slider = slider.clamp(0.0, 1.0);
    if slider <= 0.0 {
        // The bottom of the range is quiet, not silent; the bottom of the
        // slider must be silent.
        return 0.0;
    }
    let ceiling = ceiling_db.clamp(MIN_CEILING_DB, 0.0);
    rodio::math::db_to_linear(ceiling - (ceiling - MIN_DB) * (1.0 - slider))
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
    /// Whether the queue recycles instead of draining.
    pub loop_queue: bool,
    /// The chosen ceiling in decibels; 0.0 means the audio is passed through.
    pub volume_ceiling_db: f32,
    /// The loudness every track is corrected towards, in LUFS.
    pub target_lufs: f32,
    /// Whether per-track loudness correction is on.
    pub normalize: bool,
    /// Whether one track hands over to the next without a gap.
    pub gapless: bool,
    /// Whether a track ends when its music does rather than when its file does.
    pub trim_silence: bool,
    /// Whether an unmeasured stream is measured before it starts playing.
    /// What the current track is being corrected by, in dB. `null` means it has
    /// not been measured yet, which is distinct from a measured correction of
    /// zero.
    pub track_gain_db: Option<f32>,
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
    /// Names a file in the cover store; `None` means generated artwork.
    pub cover_key: Option<String>,
    /// The provider thumbnail, for a row whose cover was never stored.
    pub remote_thumbnail_url: Option<String>,
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
    /// Play a queued track now, dropping whatever was queued ahead of it.
    PlayManualEntry(u64),
    /// Play the nth row of "up next", counting from zero.
    PlayUpcoming(usize),
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
    /// Play, if not already playing.
    ///
    /// Distinct from the toggle because the system media panel sends what it
    /// *wants*, not "the other one": a Play arriving while already playing
    /// must do nothing, where a toggle would pause. That is the difference
    /// between a media key that works and one that fights the flyout.
    Resume,
    /// Pause, if not already paused. See [`PlayerCommand::Resume`].
    Pause,
    Next,
    Previous,
    Stop,
    SetVolume(f32),
    SetMuted(bool),
    SetRepeat(RepeatMode),
    SetShuffle(bool),
    /// Play the queued tracks round and round instead of consuming them.
    SetLoopQueue(bool),
    SetKeepAbandoned(bool),
    /// Lower the top of the slider, in decibels below unity.
    SetVolumeCeiling(f32),
    /// The loudness tracks are corrected towards, in LUFS.
    SetTargetLufs(f32),
    /// Whether to correct each track towards a common loudness.
    SetNormalize(bool),
    SetGapless(bool),
    SetTrimSilence(bool),
    /// Whether the equaliser is in circuit at all.
    SetEqualizerEnabled(bool),
    /// Every band at once, in dB, low to high.
    ///
    /// The whole curve rather than one band: a preset applied band by band
    /// would be audible as a sweep across the spectrum.
    SetEqualizerBands(Vec<f32>),
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
            PlayerCommand::SetVolume(_)
                | PlayerCommand::SetMuted(_)
                | PlayerCommand::SetVolumeCeiling(_)
                | PlayerCommand::SetTargetLufs(_)
                | PlayerCommand::Seek(_)
        )
    }
}

/// Where the coordinator reports to.
///
/// The coordinator does not depend on Tauri directly, so its wiring can be
/// tested without a window -- which matters because the progress and
/// end-of-track paths are driven by the engine, never by a command, and so
/// cannot be exercised through the command API at all.
pub trait PlayerEvents: Send + Sync + Clone + 'static {
    fn state(&self, status: PlayerStatus);
    fn progress(&self, progress: PlayerProgress);
    fn error(&self, message: String);
    fn queue(&self, queue: QueueState);
    /// A background cache fill started or finished.
    ///
    /// Routed through this trait rather than an `AppHandle` because the
    /// coordinator deliberately has none -- and because a test that wants to
    /// know whether caching was announced should be able to see it.
    fn caching(&self, track_id: i64, title: Option<String>);
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

    fn caching(&self, track_id: i64, title: Option<String>) {
        match title {
            Some(title) => crate::downloads::caching_started(self, track_id, title),
            None => crate::downloads::caching_finished(self, track_id),
        }
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
    /// The top of the slider, in decibels. Chosen by the listener.
    ceiling_db: f32,
    /// What every track is corrected towards. See `loudness::TARGET_LUFS`.
    target_lufs: f32,
    /// Whether per-track loudness correction is applied.
    ///
    /// Off by default and switchable while playing, because the only honest way
    /// to judge it is to hear the same track both ways without restarting.
    normalize: bool,
    /// Whether an unmeasured stream is measured *before* it starts playing.
    ///
    /// Off by default. It is the only way to level the very first play of a
    /// track nobody has heard, and it costs about twelve seconds of waiting to
    /// do it -- a trade worth offering and not worth imposing.
    /// The correction for the track currently loaded, in decibels.
    ///
    /// Decided once, when the track starts, and never moved while it plays --
    /// a gain that drifts mid-song is a compressor, not a volume control.
    /// `None` when nothing is known about the track yet, which is the first
    /// play of a stream nobody has heard. Deliberately distinct from
    /// `Some(0.0)`, which is a track measured and found to already sit at the
    /// target -- the UI needs to tell "not measured" from "no correction".
    track_gain_db: Option<f32>,
    /// The epoch of the track *playing*. The engine echoes it back so a
    /// `Finished` for a track we already moved past can be discarded.
    epoch: u64,
    /// The last epoch handed out, playing or merely queued.
    ///
    /// Separate from `epoch` because a queued track needs an identity before
    /// it starts, and every guard here compares against what is playing.
    /// Reserving one by bumping `epoch` made the engine's reports about the
    /// track still playing look stale -- so progress stopped a few seconds
    /// from the end, and the handover, when it came, was discarded too.
    issued: u64,
    /// Whether one track may be handed to the next without a gap.
    ///
    /// A device preference like volume, and off is bit-identical to the
    /// behaviour before any of this existed: nothing is ever queued, so every
    /// track begins with a `Play` exactly as it always did.
    gapless: bool,
    /// Whether a track ends when its music does rather than when its file does.
    ///
    /// Separate from `gapless`, because they fix different things. Gapless is
    /// about this player: it stops *us* putting a gap between two tracks. This
    /// is about the file -- a great many uploads run on for seconds after the
    /// last note, and that silence is in the recording whatever the player
    /// does. Wanting either without the other is coherent; some listeners want
    /// every file played to its last sample.
    trim_silence: bool,
    /// The track appended behind the one playing, and the epoch it will become.
    ///
    /// Cleared by everything that makes the engine drop its player, because
    /// the appended source goes with it -- and a record of a handover that
    /// cannot happen is worse than none.
    enqueued: Option<(u64, i64)>,
    /// Whether anything audible has been heard from the track playing.
    ///
    /// Trimming a silent *tail* presupposes a head. A track that has been
    /// quiet since it started has no tail to trim -- silence is simply what it
    /// contains, and cutting it short would be playing less of the file than
    /// the file holds.
    heard_audio: bool,
    /// How long the track playing runs for, when it is known.
    ///
    /// Only to decide *when* to queue the next one. Early enough that ffmpeg
    /// has buffered, late enough that the queue is unlikely to change under
    /// it -- and a track of unknown length simply never qualifies.
    loaded_duration: Option<Duration>,
    /// Shared: a load runs on its own task and reaches the engine from there.
    engine: Arc<AudioEngine>,
    /// Where those tasks report back.
    loads: UnboundedSender<LoadOutcome>,
    /// Where a background loudness measurement reports back.
    levels: UnboundedSender<LevelOutcome>,
    pool: SqlitePool,
    /// Handed in at startup: the coordinator has no `AppHandle` to resolve it
    /// from, and doing so per play would repeat the lookup needlessly.
    yt_dlp: Option<std::path::PathBuf>,
    /// Tracks a background loudness sample has been started for.
    ///
    /// Once each, per session. Without it a track that cannot be measured
    /// would be re-sampled every time it played, and one already in flight
    /// would be sampled twice.
    sampling: std::collections::HashSet<i64>,
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
    let (levels_tx, levels_rx) = mpsc::unbounded_channel();
    let (prepares_tx, prepares_rx) = mpsc::unbounded_channel();

    tauri::async_runtime::spawn(async move {
        Coordinator {
            queue: PlayerQueue::default(),
            state: PlaybackState::Stopped,
            loaded: None,
            volume: 1.0,
            ceiling_db: DEFAULT_CEILING_DB,
            target_lufs: crate::loudness::TARGET_LUFS,
            // Off until asked for. The frontend restores the saved choice at
            // startup the same way it restores volume.
            normalize: false,
            track_gain_db: None,
            muted: false,
            epoch: 0,
            issued: 0,
            // The same default the frontend restores, so the two agree even if
            // the restore never arrives. They used to differ -- off here, on
            // there -- which meant a failed or late restore left the setting
            // reading one way and behaving the other.
            gapless: true,
            trim_silence: true,
            enqueued: None,
            heard_audio: false,
            loaded_duration: None,
            engine: Arc::new(engine),
            loads: loads_tx,
            levels: levels_tx,
            sampling: std::collections::HashSet::new(),
            // With the pool, so a resolve can file the upload date it learned.
            stream_urls: Arc::new(StreamUrlCache::with_pool(pool.clone())),
            pool,
            yt_dlp,
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
        .run(command_rx, engine_rx, loads_rx, prepares_rx, levels_rx)
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
        mut levels: UnboundedReceiver<LevelOutcome>,
    ) {
        loop {
            tokio::select! {
                Some(command) = commands.recv() => self.handle_command(command).await,
                Some(event) = engine_events.recv() => self.handle_engine_event(event).await,
                Some(outcome) = loads.recv() => self.handle_load(outcome).await,
                Some(ready) = prepares.recv() => self.keep_prepared(ready),
                Some(level) = levels.recv() => self.apply_measured_level(level).await,
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

            // Both of these are Next with a destination: the same start and
            // the same halt, so a clicked row and a pressed button cannot
            // end up in different states.
            PlayerCommand::PlayManualEntry(entry_id) => {
                self.consider_offline_copy();
                match self.queue.jump_to_manual(entry_id) {
                    Some(track_id) => self.start(Some(track_id)),
                    None => self.halt(),
                }
            }

            PlayerCommand::PlayUpcoming(offset) => {
                self.consider_offline_copy();
                match self.queue.jump_to_upcoming(offset) {
                    Some(track_id) => self.start(Some(track_id)),
                    None => self.halt(),
                }
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

            PlayerCommand::Resume => {
                if matches!(self.state, PlaybackState::Paused) && self.report(self.engine.resume())
                {
                    self.state = PlaybackState::Playing;
                    self.emit_state();
                }
            }

            PlayerCommand::Pause => {
                if matches!(self.state, PlaybackState::Playing) && self.report(self.engine.pause()) {
                    self.state = PlaybackState::Paused;
                    self.emit_state();
                }
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
                // `promote_loop_to_repeat` only fires where this would otherwise
                // stop, and only when Loop is on -- see its docs.
                match self
                    .queue
                    .on_next()
                    .or_else(|| self.queue.promote_loop_to_repeat())
                {
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
            PlayerCommand::SetNormalize(on) => {
                self.normalize = on;
                // Re-read rather than trusting a stale figure: switching this on
                // mid-track is the whole point, and the reading may have landed
                // since the track started.
                if let Some(track_id) = self.loaded {
                    self.track_gain_db = self.track_gain_for(track_id).await;
                }
                self.apply_track_gain();
            }

            // Straight through to the shared atomics the audio thread reads.
            // No decode restart and no gap: the change lands on the next frame.
            PlayerCommand::SetTrimSilence(on) => self.trim_silence = on,

            PlayerCommand::SetGapless(on) => {
                self.gapless = on;
                // Nothing is un-queued. Whatever is already appended plays as
                // it was going to; the setting decides the *next* handover.
            }

            PlayerCommand::SetEqualizerEnabled(on) => {
                self.engine.equaliser().set_enabled(on);
            }
            PlayerCommand::SetEqualizerBands(bands) => {
                self.engine.equaliser().set_gains(&bands);
            }

            // Both change the preview, so both re-emit the queue.
            PlayerCommand::SetRepeat(mode) => self.queue.set_repeat(mode),
            PlayerCommand::SetShuffle(on) => self.queue.set_shuffle(on),

            PlayerCommand::SetLoopQueue(on) => self.queue.set_loop_manual(on),
            PlayerCommand::SetVolumeCeiling(db) => {
                self.ceiling_db = db.clamp(MIN_CEILING_DB, 0.0);
                // Heard immediately: a ceiling that only took effect on the
                // next track would look like it had done nothing.
                self.apply_volume();
            }

            PlayerCommand::SetTargetLufs(lufs) => {
                self.target_lufs = lufs.clamp(
                    crate::loudness::MIN_TARGET_LUFS,
                    crate::loudness::MAX_TARGET_LUFS,
                );
                // The gain is derived from the target, so the track playing
                // right now has to be re-derived -- and heard, because moving
                // this and hearing nothing is how a setting gets called broken.
                // The volume stage glides, so it is a level change, not a click.
                if let Some(track_id) = self.loaded {
                    self.track_gain_db = self.track_gain_for(track_id).await;
                }
                self.apply_track_gain();
            }

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

                            // A seek rebuilds the decode, and anything queued
                            // behind it went with the player it was appended
                            // to. Say so, and start preparing again: seeking
                            // into the last few seconds of a track is exactly
                            // when the handover matters, and it would be a
                            // strange feature that worked everywhere except
                            // where the user was looking.
                            self.enqueued = None;
                            self.prefetch_next();
                        }
                        // The decode could not be rebuilt from here, which is
                        // how the AAC failure shows up mid-listen: the track
                        // has been playing happily, and seeking asks ffmpeg to
                        // start again at a point it will not decode.
                        //
                        // The engine cannot recover from that on its own -- it
                        // holds a resolved URL and has no way to ask for
                        // another -- so this reloads the track at the seek
                        // target instead. The load path tries the preferred
                        // encoding, fails the same way, and falls back. That
                        // costs one wasted decode and turns a dead seek plus a
                        // baffling toast into a seek that works.
                        Err(e) if crate::transcode::is_undecodable(&e) => {
                            self.resume_at = Some(position);
                            let current = self.queue.current();
                            self.start(current);
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
            EngineEvent::Finished { epoch, handed_to } => {
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

                if let Some(next_epoch) = handed_to {
                    self.adopt_handover(next_epoch).await;
                    return;
                }

                // The engine dropped its player, so nothing is decoded any
                // more; a rewind is not available even for repeat-one.
                self.loaded = None;

                // The same promotion Next gets: pressing Next and letting a track
                // run out must not disagree about what happens at the end.
                match self
                    .queue
                    .on_finished()
                    .or_else(|| self.queue.promote_loop_to_repeat())
                {
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
                    // The one place that knows how close the end is, which is
                    // the only thing deciding when to queue the next track --
                    // and when the track has, for listening purposes, ended.
                    if self.engine.silent_for().is_zero() {
                        self.heard_audio = true;
                    }
                    self.enqueue_next().await;
                    self.trim_silent_tail();
                }
            }

            EngineEvent::Output { name, error } => self.handle_output_change(name, error),
        }
    }

    /// The output device was reopened underneath us, or could not be.
    ///
    /// Two things have to happen here and neither belongs to the engine, which
    /// reports facts and decides nothing.
    ///
    /// The prepared decode is dropped because it was built to the *old*
    /// device's sample rate. Handing it to a device running at another rate
    /// would put rodio's linear-interpolation resampler -- 33 dB below the
    /// music -- in front of a track for its whole length, which is the exact
    /// thing `output_rate` exists to prevent. The engine refuses a stale one
    /// as a backstop; dropping it here is what stops a doomed ffmpeg process
    /// sitting in memory until the track it was for comes round.
    ///
    /// A failure is surfaced because it is otherwise completely silent. The
    /// engine has no player, so no progress arrives and no track finishes: the
    /// bar simply stops, mid-song, with nothing to say why.
    fn handle_output_change(&mut self, name: Option<String>, error: Option<String>) {
        // Unconditional. Even a reopen that failed to resume the track may
        // have landed on a device running at another rate.
        self.prepared = None;

        // Success says nothing. Playback was rebuilt on the new device and
        // carried on from where it was, and the sound coming out of the right
        // speakers is the whole message.
        let Some(reason) = error else { return };

        self.events.error(match name {
            Some(device) => {
                format!("Audio switched to {device}, but playback could not resume: {reason}")
            }
            None => format!("Lost the audio output device. {reason}"),
        });
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

    /// Reserves an epoch nothing else will be given.
    fn next_epoch(&mut self) -> u64 {
        self.issued += 1;
        self.issued
    }

    fn begin_load(&mut self, candidate: Option<i64>, attempt: usize) {
        let Some(track_id) = candidate else {
            self.halt();
            self.emit_state();
            return;
        };

        // Claims this load, which makes anything already in flight stale --
        // including a track appended behind the one playing, which the engine
        // drops with the player it was appended to.
        self.enqueued = None;
        self.heard_audio = false;
        self.loaded_duration = None;
        self.epoch = self.next_epoch();
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
            // Turned into a level here rather than when it was prepared: the
            // target may have moved, or normalisation been switched off, in
            // the minutes since. This is the last moment the answer can be
            // right, and it is still before the first sample.
            let track_gain = match (self.normalize, ready.measured) {
                (true, Some(measured)) => {
                    rodio::math::db_to_linear(crate::loudness::gain_db(measured, self.target_lufs))
                }
                _ => 1.0,
            };

            tauri::async_runtime::spawn(async move {
                let result = engine
                    .play(
                        ready.source,
                        Some(ready.decoded),
                        Duration::ZERO,
                        epoch,
                        track_gain,
                    )
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
        // Where this track is about to begin, said now rather than when the
        // load returns. Resolving a stream takes seconds, and until this the
        // only position anyone had was the *previous* track's -- so the bar
        // spent those seconds counting on through a track that had already
        // stopped. State first: a tick whose track the frontend does not yet
        // know about is discarded.
        self.emit_progress(start_at);

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
                // Before `apply_volume`, so the track starts at its own level
                // rather than at the previous track's for a poll interval.
                self.track_gain_db = self.track_gain_for(outcome.track_id).await;
                self.apply_track_gain();
                self.loaded_duration = self.duration_of(outcome.track_id).await;
                // Only after the gain is known, so a track that already has a
                // reading is never sampled. This is the cold-stream case and
                // nothing else.
                self.measure_while_playing(outcome.track_id);
                // Set the bar immediately; the first tick is up to a poll
                // interval away, and for a resume starting from zero would
                // show the wrong place until it arrived.
                self.emit_progress(outcome.start_at);
                self.emit_state();
                // With the state, not instead of it.
                //
                // `emit_state` carries the track's *id*; the queue payload
                // carries everything that describes it. The player bar needs
                // both, and it can only fall back to the library list when the
                // track is in it -- an audition is `in_library = 0`, so for a
                // streamed track played for the first time the queue payload is
                // the only source of a title or a cover. Emitting one without
                // the other leaves the bar on "Loading track details…".
                //
                // The failure branch below already did this. The success branch
                // -- the one that runs on every track that actually plays --
                // did not.
                self.emit_queue().await;
            }

            // Lost the race, and already knows it. Not a failure, and above
            // all not a reason to skip a track.
            Err(e) if e == SUPERSEDED => {}

            Err(e) => {
                let next_attempt = outcome.attempt + 1;

                // Skipping a dead track goes through the same priority rule as
                // a natural end, so an unplayable context track cannot swallow
                // the tracks the user queued behind it.
                //
                // Except when there is no decoder at all. Skipping assumes the
                // *track* is the problem and the next one may be fine; with
                // ffmpeg missing every track fails identically, so retrying
                // walks two innocent tracks out of the queue and leaves the
                // listener reading an error about the third. Stop on the one
                // they actually asked for, and say why.
                let candidate = (next_attempt < MAX_LOAD_ATTEMPTS
                    && !crate::sidecar::is_missing_decoder(&e))
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


    /// How close to the end of a track the next one is queued.
    ///
    /// Late, deliberately. Once appended a source cannot be un-appended --
    /// rodio can clear a player's whole queue but not its tail -- so anything
    /// that changes what plays next has to throw away the running player and
    /// rebuild it. That is exactly the work every track used to cost, so it is
    /// never *wrong*, only wasteful; queueing late is what makes it rare.
    ///
    /// Far enough out that ffmpeg's prefill is long finished, since the decode
    /// was prepared earlier still.
    const ENQUEUE_WITHIN: Duration = Duration::from_secs(8);

    /// How long the output must be silent before the track is called finished.
    ///
    /// Three seconds of true digital silence. A dramatic pause inside a song
    /// carries a reverb tail and a noise floor well above -60 dBFS, so this is
    /// reached by a track that has actually stopped rather than one being
    /// quiet -- and three seconds is longer than the pause before almost every
    /// false ending, which is the failure that would actually cost music.
    const SILENT_TAIL: Duration = Duration::from_secs(3);

    /// How near the end a silent tail may be trimmed.
    ///
    /// Twenty seconds. Wide enough for the tails actually seen -- the track
    /// that prompted this runs on for fourteen and a half seconds after its
    /// last note -- and narrow enough that a false ending further back than
    /// that is out of reach entirely.
    ///
    /// The pair of thresholds is the whole safety argument: to lose music, a
    /// song would need more than three seconds of *digital* silence starting
    /// within twenty seconds of its end, and then more music after it.
    const TRIM_WITHIN: Duration = Duration::from_secs(20);

    /// Ends a track whose music has stopped but whose file has not.
    ///
    /// The gap this removes is not one this app creates. A great many uploads
    /// run on after the last note -- fourteen seconds, in the track that
    /// prompted this -- and playing that faithfully is a silence between songs
    /// however seamless the handover after it is.
    ///
    /// Tied to the gapless setting, because it is the same promise: the record
    /// should run the way it was meant to. Switching that off plays every file
    /// to its last sample, silence included.
    fn trim_silent_tail(&mut self) {
        if !self.trim_silence {
            return;
        }

        let Some(duration) = self.loaded_duration else {
            return;
        };
        if duration.saturating_sub(self.last_position) > Self::TRIM_WITHIN {
            return;
        }

        // Nothing audible yet means nothing to trim: see `heard_audio`.
        if !self.heard_audio {
            return;
        }

        // A stalled decoder emits silence to cover the shortfall, which at the
        // output is indistinguishable from a track that has ended. The engine
        // zeroes the count while starving; this is the second line of the same
        // defence, for the window before a stall is confirmed.
        if self.stalled {
            return;
        }

        if self.engine.silent_for() >= Self::SILENT_TAIL {
            // Best effort: if it fails the track simply plays out, which is
            // what it did before any of this.
            let _ = self.engine.skip_tail();
        }
    }

    /// Hands the prepared next track to the engine, to follow this one.
    ///
    /// Best effort in the strongest sense: every failure leaves the ordinary
    /// path untouched, and the ordinary path is what shipped before gapless
    /// existed. The cost of a refusal is a gap, never a track.
    async fn enqueue_next(&mut self) {
        if !self.gapless || self.enqueued.is_some() {
            return;
        }

        // Only when the end is close. See `ENQUEUE_WITHIN`.
        let Some(duration) = self.loaded_duration else {
            return;
        };
        if duration.saturating_sub(self.last_position) > Self::ENQUEUE_WITHIN {
            return;
        }

        // Repeat-one replays the track that is playing, and `peek_next` does
        // not describe that: it reports the next track in the *list*. Queueing
        // that would hand over to a track the end-of-track path then refuses,
        // and the resynchronisation costs a full cold load -- a longer silence
        // than there would have been with no gapless at all.
        //
        // Looping one track into itself seamlessly would need a second decode
        // of the same track, which is a different feature.
        if self.queue.repeat() == crate::queue::RepeatMode::One {
            return;
        }

        // What the queue says comes next has to still be what was prepared.
        let Some(next_id) = self.queue.peek_next() else {
            return;
        };
        let Some(ready) = self.prepared.take().filter(|r| r.track_id == next_id) else {
            return;
        };

        // Turned into a level here, with the settings in force now -- the same
        // reasoning as the handover path in `begin_load`, and the last moment
        // the answer can be right while still preceding the first sample.
        let track_gain = match (self.normalize, ready.measured) {
            (true, Some(measured)) => {
                rodio::math::db_to_linear(crate::loudness::gain_db(measured, self.target_lufs))
            }
            _ => 1.0,
        };

        // Claimed before the engine is asked, so the epoch the engine echoes
        // back in `Finished` is one this already knows about.
        let next_epoch = self.next_epoch();

        if self
            .engine
            .enqueue(ready.source, ready.decoded, next_epoch, track_gain)
            .await
            .is_ok()
        {
            // Deliberately *not* assigned to `self.epoch`: the track playing
            // still owns that, and its progress and its eventual end are both
            // reported under it.
            self.enqueued = Some((next_epoch, next_id));
        } else {
            // Refused: nothing playing to follow, something already queued, or
            // a decode that no longer suits the device. The decode went with
            // the attempt, so prepare another -- without this, a single
            // refusal leaves the next track to load from cold, which is a
            // longer gap than there would have been with no gapless at all.
            self.prefetch_next();
        }
    }

    /// Takes over from a handover the engine has already performed.
    ///
    /// The track is *already sounding* by the time this runs. So this may not
    /// start anything -- the queue pointer and the coordinator's idea of what
    /// is playing have to catch up to audio that is ahead of them, which is
    /// the exact inverse of every other path here.
    async fn adopt_handover(&mut self, next_epoch: u64) {
        let claimed = self
            .enqueued
            .take()
            .filter(|(epoch, _)| *epoch == next_epoch)
            .map(|(_, track_id)| track_id);

        // Advances the queue the same way an ordinary end does, so the two
        // cannot disagree about what "next" meant.
        let advanced = self
            .queue
            .on_finished()
            .or_else(|| self.queue.promote_loop_to_repeat());

        let Some(track_id) = claimed.filter(|id| Some(*id) == advanced) else {
            // The engine handed over to something this no longer agrees with.
            // Stopping and starting the queue's own answer costs a gap and
            // resynchronises, which beats leaving the two out of step.
            self.enqueued = None;
            self.loaded = None;
            match advanced {
                Some(track_id) => self.start(Some(track_id)),
                None => self.halt(),
            }
            self.emit_state();
            self.emit_queue().await;
            self.prefetch_next();
            return;
        };

        self.epoch = next_epoch;
        self.loaded = Some(track_id);
        self.state = PlaybackState::Playing;
        self.stalled = false;
        self.stalled_since = None;
        // It began at zero and has been decoded from there, which is what
        // makes it worth keeping a copy of.
        self.covered_from_zero = true;
        self.recorded_play = false;
        self.heard_audio = false;
        self.last_position = Duration::ZERO;
        self.resume_at = None;

        // The engine already levelled it from the reading carried with the
        // decode. This re-reads it so the *settings panel* agrees, and so a
        // reading that landed since preparation is picked up.
        self.track_gain_db = self.track_gain_for(track_id).await;
        self.apply_track_gain();
        self.loaded_duration = self.duration_of(track_id).await;

        self.measure_while_playing(track_id);
        self.emit_progress(Duration::ZERO);
        self.emit_state();
        self.emit_queue().await;
        self.prefetch_next();
    }

    /// How long a track runs for, as the database has it.
    ///
    /// `&mut self` for the same reason every other query here takes it: a
    /// shared borrow held across an await makes the whole coordinator have to
    /// be `Sync`, and the prepared decoder it holds is `Send` but not `Sync`.
    async fn duration_of(&mut self, track_id: i64) -> Option<Duration> {
        // `i64`, because that is what the column is. Asking sqlx for an `f64`
        // from an INTEGER column is a decode error, not a conversion -- and
        // the error goes into `.ok()` and disappears, leaving every track
        // looking like one of unknown length. Which is exactly what happened:
        // gapless never fired, because the one thing deciding *when* to queue
        // the next track could never answer.
        let secs: Option<i64> = sqlx::query_scalar("SELECT duration_secs FROM tracks WHERE id = ?")
            .bind(track_id)
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten();

        secs.filter(|s| *s > 0).map(|s| Duration::from_secs(s as u64))
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
        let next = self.queue.peek_next();

        // Something is already appended behind the current track, and it is
        // not what comes next any more -- the queue was reordered, or the
        // track was removed from it. It cannot be pulled back out, so the
        // engine drops it by rebuilding, which costs a short break. Rare, and
        // the alternative is hearing the wrong track.
        if self
            .enqueued
            .as_ref()
            .is_some_and(|(_, id)| Some(*id) != next)
        {
            self.enqueued = None;
            let _ = self.engine.cancel_queued();
        }

        let Some(track_id) = next else {
            // Nothing follows: release whatever was held for a track that is
            // no longer next, so its ffmpeg does not linger.
            self.prepared = None;
            return;
        };

        // What is held no longer matches what is coming.
        if self.prepared.as_ref().is_some_and(|r| r.track_id != track_id) {
            self.prepared = None;
        }

        // Or matches the track but not the device. Preparing is triggered by
        // queueing, which can happen before the output device has finished
        // opening -- and a decode built against the fallback rate is one the
        // engine will refuse, rebuild on its own thread, and charge the gap
        // for. Dropping it here is what gets it built again, correctly.
        let rate = self.engine.output_rate();
        if self.prepared.as_ref().is_some_and(|r| r.rate != rate) {
            self.prepared = None;
        }

        self.prepare_next(track_id);
        // Same moment, same track: the decode is warmed and the level is
        // learned while there is still time for both.
        self.premeasure_next(track_id);

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

        let position = resume_position(position_secs, duration);

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
        // Cloned in because the fill runs on its own task: it reports when it
        // starts and when it stops, minutes apart.
        let events = self.events.clone();

        tauri::async_runtime::spawn(async move {
            let Ok(Some(row)) = sqlx::query(
                // `title` is listed only for the activity panel, which makes it
                // the easy one to drop as unused -- and the loss is invisible
                // until runtime: `Row::get` panics on a column the statement
                // never selected. This runs on a spawned task in a build that
                // aborts on panic, so that panic takes the whole app down the
                // moment a track is left part-way through.
                "SELECT source, state, title, remote_id, remote_url, duration_secs \
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

            // Shown in the activity panel while it runs -- quietly, and
            // below the downloads. The user did not ask for this; they only
            // allowed it, and it should read that way.
            events.caching(track_id, Some(row.get("title")));

            // Silent either way: the user did not ask for this and must not be
            // told off for it failing.
            let _ = crate::download::fetch_into_cache(&yt_dlp, &ffmpeg, &remote_url, pending).await;

            events.caching(track_id, None);

            if let Ok(mut active) = fetching.lock() {
                active.remove(&track_id);
            }
        });
    }

    /// Measures the track that is about to play, while the current one still is.
    ///
    /// This is what levels ordinary sequential listening from the first sample.
    /// A stream has nothing to measure until some of it has arrived, so the
    /// work is done against the minutes the current track has left rather than
    /// against the listener's patience.
    ///
    /// Sampling four tenths of it, rather than fetching the whole thing, is a
    /// recent change and the reason this is now worth relying on: the old path
    /// downloaded the entire track and took about twelve seconds, which on a
    /// modest connection could easily lose the race against a three-minute
    /// song. Sampling takes a second or two and roughly 40% of the traffic --
    /// for something the listener may well skip before it plays.
    ///
    /// What it produces is an approximation, within 1 LU of the truth on every
    /// track measured here, and recorded as sampled so the background pass
    /// still replaces it with the exact figure once playback has left a copy
    /// on disk.
    ///
    /// Best effort and silent. If it fails, or loses the race, the track is
    /// levelled a second or two in by [`Self::measure_while_playing`] instead
    /// -- so the failure mode is a slightly later correction, never a wrong one.
    fn premeasure_next(&mut self, track_id: i64) {
        if !self.normalize {
            return;
        }
        let pool = self.pool.clone();
        let ffmpeg = self.ffmpeg.clone();
        let yt_dlp = self.yt_dlp.clone();
        let cache = self.audio_cache.clone();
        let stream_urls = Arc::clone(&self.stream_urls);

        tauri::async_runtime::spawn(async move {
            // Nothing to do if this track already has a reading.
            if crate::loudness::stored(&pool, track_id).await.is_some() {
                return;
            }

            // Sampled first. Against a whole extra download this is roughly
            // 40% of the traffic and a couple of seconds rather than twelve --
            // and being quick is most of the point, because this is racing the
            // rest of the current track. A sample that lands is a track
            // levelled from its first sample; one that does not is a track
            // levelled a second or two in by `measure_while_playing`.
            //
            // The reading is marked sampled, so the background pass still
            // replaces it with the exact figure once playback has left a copy
            // on disk.
            if let Some(ffmpeg) = ffmpeg.as_deref() {
                if let Some(measured) = sample_playing_track(
                    &pool,
                    ffmpeg,
                    yt_dlp.as_deref(),
                    &stream_urls,
                    cache.as_ref(),
                    track_id,
                )
                .await
                {
                    crate::loudness::record_sampled(&pool, track_id, measured).await;
                    return;
                }
            }

            // Sampling declines short tracks and fails on anything it cannot
            // read. The full path handles both, and for a short track costs
            // about what sampling would have.
            crate::loudness::ensure_measured(
                &pool,
                ffmpeg.as_deref(),
                yt_dlp.as_deref(),
                cache.as_ref(),
                track_id,
            )
            .await;
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
        // Only for its output rate: the decode has to be built at whatever the
        // device runs at, and the engine is the only thing that knows.
        let engine_for_build = Arc::clone(&self.engine);

        tauri::async_runtime::spawn(async move {
            // Preferred only. If it turns out not to decode, the real load
            // discovers that and falls back; building a second decoder here to
            // find out early would spend an ffmpeg process on a rare case.
            let Ok(source) = resolve_track(
                &pool,
                &urls,
                cache.as_ref(),
                yt_dlp.as_deref(),
                track_id,
                Encoding::Preferred,
            )
            .await
            else {
                return;
            };

            // Blocking: building a decoder waits for ffmpeg to produce its
            // first half second. On a worker thread that is fine; on the
            // runtime it would stall every other task.
            // Read once, so what is recorded on `Prepared` is exactly what the
            // decode was built for.
            let rate = engine_for_build.output_rate();
            let source_for_build = source.clone();
            let built = tauri::async_runtime::spawn_blocking(move || {
                // The device rate, so the decode comes back at the rate rodio
                // is already mixing at and never has to be resampled.
                engine::build_source(&source_for_build, ffmpeg.as_deref(), rate, Duration::ZERO)
            })
            .await;

            if let Ok(Ok(decoded)) = built {
                let _ = prepares.send(Prepared {
                    track_id,
                    source,
                    decoded,
                    measured: crate::loudness::stored(&pool, track_id).await,
                    rate,
                });
            }
        });
    }

    fn halt(&mut self) {
        self.stalled = false;
        self.stalled_since = None;
        self.enqueued = None;
        self.loaded_duration = None;
        // Bump the epoch so any in-flight `Finished` for the stopped track is
        // discarded rather than triggering an advance.
        self.epoch = self.next_epoch();
        self.state = PlaybackState::Stopped;
        self.loaded = None;
        self.report(self.engine.stop(self.epoch));
    }

    /// The slider, times the track's own correction.
    ///
    /// Two independent decisions multiplied, rather than one blended number:
    /// the slider is where the listener put it and must keep meaning that, and
    /// the correction belongs to the track. Anything that would clip is caught
    /// by the look-ahead limiter in `engine.rs`, which is what makes a *boost*
    /// safe to apply at all.
    /// Two independent decisions, sent as two independent numbers.
    ///
    /// They used to be multiplied here and pushed into one cell, which quietly
    /// made the correction a property of *when it was sent* rather than of the
    /// track it was measured from. That was invisible while every track began
    /// with a `Play` command -- and it is exactly what breaks when a track can
    /// start because the one before it ended.
    fn apply_volume(&self) {
        let slider = slider_to_linear(self.volume, self.muted, self.ceiling_db);
        self.report(self.engine.set_volume(slider));
    }

    /// The correction for the track playing now.
    ///
    /// Sent whenever the answer changes underneath a track already playing: a
    /// reading arriving mid-song, normalisation being toggled, the target
    /// moving. The level a track *starts* at travels with it instead.
    fn apply_track_gain(&self) {
        self.report(self.engine.set_track_gain(self.track_gain_linear()));
    }

    /// What the current reading is worth, as a linear multiplier.
    fn track_gain_linear(&self) -> f32 {
        if self.normalize {
            rodio::math::db_to_linear(self.track_gain_db.unwrap_or(0.0))
        } else {
            1.0
        }
    }

    /// Starts measuring a track that is already playing, if it needs it.
    ///
    /// The gap this closes: a stream nobody has played has no reading, so it
    /// used to play at whatever level it was mastered at and be corrected from
    /// the *next* time. Measuring needs the whole track, and the whole track is
    /// not there yet -- which was read as "impossible", and was not. Sampling
    /// four tenths of it spread across its length lands within 1 LU of the
    /// truth on every track in this library, and the slices fetch in parallel
    /// while the song plays, so nothing waits.
    ///
    /// Deliberately does nothing when:
    ///
    /// - there is already a reading, which is every play after the first;
    /// - the track is short enough that the background analyser will measure it
    ///   outright for the same cost, so an approximation buys nothing;
    /// - levelling is switched off, because the reading would not be used and
    ///   the traffic would be spent for nothing.
    fn measure_while_playing(&mut self, track_id: i64) {
        if !self.normalize || self.track_gain_db.is_some() {
            return;
        }
        let Some(ffmpeg) = self.ffmpeg.clone() else {
            return;
        };

        // Only sampled once per track per session. A track that fails -- no
        // network, an expired URL -- must not be retried on every poll, and a
        // track already being sampled must not be sampled twice.
        if !self.sampling.insert(track_id) {
            return;
        }

        let pool = self.pool.clone();
        let levels = self.levels.clone();
        let yt_dlp = self.yt_dlp.clone();
        let stream_urls = Arc::clone(&self.stream_urls);
        let audio_cache = self.audio_cache.clone();

        tauri::async_runtime::spawn(async move {
            let measured = sample_playing_track(
                &pool,
                &ffmpeg,
                yt_dlp.as_deref(),
                &stream_urls,
                audio_cache.as_ref(),
                track_id,
            )
            .await;
            let _ = levels.send(LevelOutcome { track_id, measured });
        });
    }
    /// Applies a reading that arrived mid-track.
    ///
    /// Three ways this can be stale by the time it lands, all of them ordinary:
    /// the listener skipped to another song, they stopped, or they switched
    /// levelling off while the slices were in flight. In every case the reading
    /// is still worth *recording* -- it was paid for, and the next play wants
    /// it -- but must not touch the gain.
    ///
    /// When it does apply, `apply_volume` moves the gain and the volume stage
    /// glides rather than steps. That is what makes a correction arriving a
    /// second or two into a song inaudible instead of a click.
    async fn apply_measured_level(&mut self, outcome: LevelOutcome) {
        let Some(measured) = outcome.measured else {
            // Recorded as attempted-and-failed so the background analyser does
            // not immediately try the same thing again.
            crate::loudness::record(&self.pool, outcome.track_id, None).await;
            return;
        };

        // As an approximation, deliberately. The background pass will replace it
        // with the exact figure once the cache copy is complete; recording it as
        // final would end that search and leave the estimate standing forever.
        crate::loudness::record_sampled(&self.pool, outcome.track_id, measured).await;

        if !reading_applies_now(self.loaded, outcome.track_id, self.normalize) {
            return;
        }

        self.track_gain_db = Some(crate::loudness::gain_db(measured, self.target_lufs));
        self.apply_track_gain();
        // So the settings panel stops saying the track has not been measured.
        self.emit_state();
    }
    /// Looks up what the track currently loading should be played at.
    ///
    /// Zero when there is no reading yet -- a stream nobody has finished once,
    /// or a local file the analyser has not reached. Unnormalised is the right
    /// fallback: it is what the app did before this existed.
    ///
    /// `&mut self` for the same reason as [`Self::emit_queue`]: a shared
    /// reference held across an await makes the whole future require `Sync`,
    /// and the prepared decoder is `Send` but not `Sync`.
    async fn track_gain_for(&mut self, track_id: i64) -> Option<f32> {
        let measured = crate::loudness::stored(&self.pool, track_id).await?;
        Some(crate::loudness::gain_db(measured, self.target_lufs))
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
            loop_queue: self.queue.loops_manual(),
            volume_ceiling_db: self.ceiling_db,
            target_lufs: self.target_lufs,
            normalize: self.normalize,
            gapless: self.gapless,
            trim_silence: self.trim_silence,
            track_gain_db: self.track_gain_db,
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
            "SELECT id, source, title, artist, duration_secs, state, cover_key, \
          remote_thumbnail_url FROM tracks WHERE id IN (",
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
                        cover_key: row.get("cover_key"),
                        remote_thumbnail_url: row.get("remote_thumbnail_url"),
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

/// A loudness reading that arrived while a track was already playing.
///
/// Carries the track it belongs to because it is answered from a spawned task
/// and the queue may have moved on: applying a gain measured for the previous
/// song is exactly the mistake this guards against.
struct LevelOutcome {
    track_id: i64,
    /// `None` when the sampling failed. Recorded either way, so a track that
    /// cannot be measured is not sampled again on every play.
    measured: Option<crate::loudness::Loudness>,
}

/// Whether a reading that has just arrived should move the gain.
///
/// Extracted so it can be tested. The condition sits in a spawned path an
/// integration test cannot reach: the sampler only runs for long uncached
/// streams, so a test built on local fixtures never produces a `LevelOutcome`
/// at all, and a guard deleted from here would go on passing. That is exactly
/// what happened to the first attempt at covering this.
fn reading_applies_now(loaded: Option<i64>, measured_for: i64, normalize: bool) -> bool {
    normalize && loaded == Some(measured_for)
}

/// A decoder built ahead of time for the track that comes next.
///
/// Holding one keeps an ffmpeg process and a full ring buffer alive, which is
/// why there is only ever one. Dropping it kills the process.
pub struct Prepared {
    track_id: i64,
    source: PlayableSource,
    decoded: engine::BuiltSource,
    /// What this track was measured at, in LUFS, if it has been.
    ///
    /// Read here rather than after the handover, because the handover is
    /// exactly where there is no time to read it: the track starts the moment
    /// the engine is handed this, and a correction applied afterwards is a
    /// correction the first sample did not get.
    ///
    /// The *reading*, not a gain. What that reading is worth depends on the
    /// target and on whether normalisation is on at all, and both can change
    /// between preparing a track and playing it.
    measured: Option<crate::loudness::Loudness>,
    /// The device rate this was decoded for.
    ///
    /// Kept because it can go stale, and because the failure is silent when it
    /// does: the engine quietly refuses a decode built for another rate and
    /// rebuilds it, so the preparation is simply wasted and the gap it exists
    /// to remove comes back.
    ///
    /// It goes stale most often within the first second of the app's life.
    /// Queueing tracks is what triggers preparation, and that can happen
    /// before the output device has finished opening -- at which point the
    /// engine is still reporting the fallback rate rather than the real one.
    rate: u32,
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
    // Enough for the whole recovery chain and no more: a poisoned cache copy
    // is thrown away, the provider is asked again, and a stream that will not
    // decode is asked for in another encoding. Three attempts covers all of
    // it; a fourth would only be more seconds of silence before the same
    // answer.
    const MAX_ATTEMPTS: usize = 3;

    let mut encoding = Encoding::Preferred;

    for attempt in 0..MAX_ATTEMPTS {
        let source = resolve_track(pool, urls, cache, yt_dlp, track_id, encoding).await?;

        // Remembered before the source is handed over, because a copy the app
        // made itself is the one failure it can clear up rather than report.
        let disposable = match &source {
            PlayableSource::Cached(path) => Some(path.clone()),
            _ => None,
        };

        // Unity: this path has no reading in hand, and the coordinator sends
        // the real correction the moment the load reports back. That is the
        // behaviour that shipped before any of this, and it is inaudible --
        // the level stage glides, and the window is one poll interval.
        match engine.play(source, None, start_at, epoch, 1.0).await {
            Ok(()) => return Ok(()),
            // Another load won while this one was resolving. Retrying would
            // only lose again, and the caller must not treat it as a fault.
            Err(e) if e == SUPERSEDED => return Err(e),
            Err(e) => {
                if attempt + 1 == MAX_ATTEMPTS {
                    return Err(e);
                }

                let undecodable = crate::transcode::is_undecodable(&e);

                if let Some(path) = disposable.filter(|_| undecodable) {
                    // A copy this app wrote and cannot read back. Interrupt a
                    // stream mid-copy and what lands decodes cleanly right up
                    // to the damage, so nothing notices until someone seeks
                    // past it -- and then the track is unplayable *forever*,
                    // because every later play finds the same file.
                    //
                    // Deleting it costs one re-download and is the only thing
                    // that ends that. The provider still has the track.
                    let _ = std::fs::remove_file(&path);
                } else if undecodable {
                    // The stream arrived and ffmpeg would not decode it. That
                    // is a fact about this *encoding*, not about the track --
                    // ffmpeg's AAC decoder rejects some provider streams
                    // outright -- so the same URL resolved again would fail
                    // identically. Ask for a different one instead.
                    encoding = Encoding::Alternate;
                } else if !forget_stream_url(pool, urls, track_id).await {
                    // Nothing cached, so staleness was not the cause and a
                    // different encoding would not have helped either.
                    return Err(e);
                }
                // Otherwise: a cached link that has since been revoked. Worth
                // exactly one fresh resolve of the same encoding.
            }
        }
    }

    Err("That track could not be played.".to_string())
}

async fn resolve_track(
    pool: &SqlitePool,
    urls: &StreamUrlCache,
    cache: Option<&AudioCache>,
    yt_dlp: Option<&std::path::Path>,
    track_id: i64,
    encoding: Encoding,
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
        encoding,
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
    //
    // Only the preferred encoding is warmed. A fallback is by definition the
    // unusual case, and resolving both ahead of time would double the cost of
    // prefetching every track to save a few seconds on the rare one.
    let _ = urls.resolve(yt_dlp, &url, Encoding::Preferred).await;
}

struct TrackDetail {
    source: String,
    title: String,
    artist: Option<String>,
    duration_secs: Option<i64>,
    state: String,
    cover_key: Option<String>,
    remote_thumbnail_url: Option<String>,
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
            cover_key: detail.cover_key.clone(),
            remote_thumbnail_url: detail.remote_thumbnail_url.clone(),
        },
        None => QueueEntry {
            entry_id,
            track_id,
            title: "Unavailable".to_string(),
            artist: None,
            duration_secs: None,
            state: "missing".to_string(),
            source: "local".to_string(),
            cover_key: None,
            remote_thumbnail_url: None,
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
pub async fn play_queued_entry(
    entry_id: u64,
    player: State<'_, PlayerHandle>,
) -> Result<(), String> {
    player.send(PlayerCommand::PlayManualEntry(entry_id))
}

#[tauri::command]
pub async fn play_upcoming(offset: usize, player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::PlayUpcoming(offset))
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

/// Plays the queued tracks round and round instead of consuming them.
///
/// Deliberately separate from repeat. Repeat acts on the context -- a whole
/// playlist or library view -- while this acts on the handful of tracks the
/// user picked out by hand. "Play these four forever" and "play this album
/// again" are different requests that share a word.
#[tauri::command]
pub async fn set_loop_queue(on: bool, player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::SetLoopQueue(on))
}

/// Lowers the top of the volume slider, in decibels below unity.
///
/// Offered because the app cannot solve the problem it exists for. Tracks come
/// from files, YouTube and SoundCloud, mastered by different people to
/// different levels: measured across this library, integrated loudness spans
/// about ten decibels. Streaming services level every track before you hear it;
/// this app plays what it is given. So a track can arrive far louder than the
/// one before it, and the honest answer is to let the listener decide what the
/// loudest possible moment is allowed to be.
#[tauri::command]
pub async fn set_volume_ceiling(
    db: f32,
    player: State<'_, PlayerHandle>,
) -> Result<(), String> {
    player.send(PlayerCommand::SetVolumeCeiling(db))
}

/// Turns per-track loudness correction on or off.
///
/// Takes effect on the track already playing, not just the next one -- the only
/// way to judge whether it helps is to hear the same passage both ways.
#[tauri::command]
pub async fn set_normalize(on: bool, player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::SetNormalize(on))
}

/// Turns gapless handover on or off.
///
/// Takes effect from the next handover: a track already queued behind the one
/// playing stays queued, because pulling it back out would mean tearing down
/// the running player to do it -- a gap, to switch off gaps.
#[tauri::command]
pub async fn set_gapless(on: bool, player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::SetGapless(on))
}

/// Turns trailing-silence trimming on or off.
#[tauri::command]
pub async fn set_trim_silence(on: bool, player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::SetTrimSilence(on))
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

    /// The case the whole rule exists for: an ordinary song.
    ///
    /// Three and a half minutes, left two minutes in. The old rule resumed it,
    /// and what the user got on pressing play was the last eighty seconds of a
    /// song they had chosen to hear.
    #[test]
    fn a_song_always_starts_from_the_beginning() {
        assert_eq!(resume_position(120.0, Some(210.0)), 0.0);
    }

    /// Nothing under seven minutes can satisfy both halves of the rule at
    /// once, which is what makes "songs restart" a property rather than a
    /// threshold anyone has to remember.
    #[test]
    fn no_short_track_can_resume_wherever_it_was_left() {
        for duration in [60.0, 180.0, 210.0, 330.0, 419.0] {
            let mut position = 0.0;
            while position < duration {
                assert_eq!(
                    resume_position(position, Some(duration)),
                    0.0,
                    "a {duration}s track resumed at {position}s",
                );
                position += 5.0;
            }
        }
    }

    /// The long upload the rule is *for*. Twenty minutes in on an hour-long
    /// set is exactly the position nobody wants to find again by dragging.
    #[test]
    fn a_long_track_keeps_the_position_it_was_left_at() {
        assert_eq!(resume_position(1200.0, Some(3600.0)), 1200.0);
    }

    /// Barely started is not a listen. Restarting costs under two minutes and
    /// gives back the beginning, which is where a track heard once belongs.
    #[test]
    fn a_long_track_barely_begun_starts_over() {
        assert_eq!(resume_position(45.0, Some(3600.0)), 0.0);
    }

    /// Near the end there is nothing left to resume *to*: playing the outro
    /// and skipping on is not what the position was kept for.
    #[test]
    fn a_long_track_near_its_end_starts_over() {
        assert_eq!(resume_position(3500.0, Some(3600.0)), 0.0);
    }

    /// A live stream, or a row whose duration was never learned. With no total
    /// there is no way to tell the middle from the end, so it starts over --
    /// the same answer the overwhelming majority of tracks get.
    #[test]
    fn an_unknown_duration_starts_over() {
        assert_eq!(resume_position(1200.0, None), 0.0);
    }

    /// The top of the slider passes the audio through untouched.
    ///
    /// Deliberately pinned. Attenuating here is the tempting fix for anything
    /// that sounds too loud, and it is nearly always the wrong one: it makes
    /// every well-mastered track quieter than other players to solve a problem
    /// belonging to a handful of samples in a few tracks. Clipping is the
    /// limiter's job, not the slider's.
    #[test]
    fn the_top_of_the_slider_is_unity() {
        assert!((slider_to_linear(1.0, false, DEFAULT_CEILING_DB) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn the_bottom_of_the_slider_is_truly_silent() {
        // -40 dB is quiet but audible, so zero must be special-cased.
        assert_eq!(slider_to_linear(0.0, false, DEFAULT_CEILING_DB), 0.0);
    }

    #[test]
    fn muting_silences_regardless_of_the_slider() {
        assert_eq!(slider_to_linear(1.0, true, DEFAULT_CEILING_DB), 0.0);
    }

    #[test]
    fn the_curve_is_perceptual_not_linear() {
        let half = slider_to_linear(0.5, false, DEFAULT_CEILING_DB);
        // A linear mapping would give 0.5 here, which sounds barely quieter.
        assert!(half < 0.2, "midpoint should be well below half amplitude");
        assert!(half > 0.01, "but still clearly audible");
    }

    /// The reason the range was widened: a comfortable level has to be
    /// somewhere you can adjust, not pinned against the bottom stop.
    #[test]
    fn a_comfortable_level_sits_in_the_middle_of_the_travel() {
        // Not a guess. At -32 dB -- what the sixty-decibel range put
        // at slider 0.5 -- tracks were reported as too quiet to listen to, and
        // the curve this shipped with put -20 dB there and was not. So a real
        // listening level is around -20, and that is what has to sit mid-travel.
        let comfortable = rodio::math::db_to_linear(-20.0);

        let mut slider = 0.0f32;
        while slider < 1.0 && slider_to_linear(slider, false, DEFAULT_CEILING_DB) < comfortable {
            slider += 0.01;
        }

        assert!(
            (0.3..=0.7).contains(&slider),
            "a normal level should land mid-slider, not at {slider}",
        );
    }

    /// A ceiling has to actually cap the loudest the app can be, or it is
    /// decoration on a settings page.
    #[test]
    fn the_ceiling_decides_the_loudest_the_app_can_get() {
        for ceiling in [0.0f32, -3.0, -6.0, -12.0] {
            let top = slider_to_linear(1.0, false, ceiling);
            let expected = rodio::math::db_to_linear(ceiling);

            assert!(
                (top - expected).abs() < 1e-4,
                "a ceiling of {ceiling} dB produced {top}, not {expected}",
            );
        }
    }

    /// The mistake this is designed around.
    ///
    /// Lowering the ceiling must compress the range, not shift it down. A -60 dB
    /// range once put -43 dB at slider 0.3 and made half the travel inaudible;
    /// attenuating on top of the existing curve would recreate exactly that,
    /// one setting at a time.
    #[test]
    fn lowering_the_ceiling_does_not_strand_the_bottom_of_the_slider() {
        let full = rodio::math::linear_to_db(slider_to_linear(0.3, false, 0.0));
        let capped = rodio::math::linear_to_db(slider_to_linear(0.3, false, -12.0));

        // Twelve decibels off the top should cost far less than twelve at 30%.
        let lost = full - capped;
        assert!(
            lost < 6.0,
            "a 12 dB ceiling cost {lost} dB at slider 0.3, which is a shift, not a cap",
        );
        assert!(lost > 0.0, "the ceiling did nothing at slider 0.3");
    }

    /// A setting that arrives from the frontend as anything at all.
    #[test]
    fn an_out_of_range_ceiling_cannot_silence_or_amplify() {
        // Above unity would mean amplifying, which is where clipping lives.
        assert!(slider_to_linear(1.0, false, 40.0) <= 1.0 + 1e-4);
        // Far below the floor would leave nothing audible at any position.
        assert!(slider_to_linear(1.0, false, -400.0) > 0.0);
    }

    #[test]
    fn the_curve_rises_monotonically() {
        let mut previous = -1.0;
        for step in 0..=20 {
            let value = slider_to_linear(step as f32 / 20.0, false, DEFAULT_CEILING_DB);
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

#[cfg(test)]
mod volume_curve_report {
    use super::*;

    /// Prints what every position of the volume slider actually does.
    ///
    /// The slider has 20 stops (`step={0.05}` in `player-bar.svelte`), so this
    /// is the whole control, not a sample of it. Kept as a test rather than a
    /// comment because the numbers move whenever `MIN_DB` or the ceiling does,
    /// and a stale table is worse than none -- the doc on `MIN_DB` still quotes
    /// "-29 dB at slider 0.3", which was true when the ceiling was -4 dB.
    #[test]
    #[ignore = "diagnostic, prints a table"]
    fn print_the_volume_curve() {
        for ceiling in [DEFAULT_CEILING_DB, -6.0, MIN_CEILING_DB] {
            println!("\n--- ceiling {ceiling:.0} dB ---");
            println!("{:>8} {:>10} {:>12} {:>14}", "slider", "dB", "amplitude", "step from last");
            let mut previous: Option<f32> = None;
            for step in 0..=20 {
                let slider = step as f32 / 20.0;
                let linear = slider_to_linear(slider, false, ceiling);
                let db = if linear > 0.0 {
                    rodio::math::linear_to_db(linear)
                } else {
                    f32::NEG_INFINITY
                };
                let delta = match (previous, linear > 0.0) {
                    (Some(p), true) => format!("{:+.2} dB", db - p),
                    _ => "-".to_string(),
                };
                println!(
                    "{:>7.0}% {:>10.1} {:>12.5} {:>14}",
                    slider * 100.0,
                    db,
                    linear,
                    delta
                );
                if linear > 0.0 {
                    previous = Some(db);
                }
            }
        }
    }
}


/// Turns the equaliser on or off without disturbing the band settings.
///
/// Separate from the bands so the panel can offer a bypass: comparing "with"
/// against "without" is the only way to judge an equaliser, and it must not
/// cost the curve someone just spent a minute setting.
#[tauri::command]
pub async fn set_equalizer_enabled(
    on: bool,
    player: State<'_, PlayerHandle>,
) -> Result<(), String> {
    player.send(PlayerCommand::SetEqualizerEnabled(on))
}

/// Sets every band at once, in dB, low to high.
///
/// Values outside the allowed range are clamped rather than rejected: this
/// arrives from the frontend as JSON, and a slider that silently did nothing
/// because one number was out of range would be a worse bug than a curve that
/// stops where the control does.
#[tauri::command]
pub async fn set_equalizer_bands(
    bands: Vec<f32>,
    player: State<'_, PlayerHandle>,
) -> Result<(), String> {
    player.send(PlayerCommand::SetEqualizerBands(bands))
}

/// The band centres, so the panel labels itself from the audio code.
///
/// Hardcoding ten frequencies in the frontend would let the labels drift away
/// from the filters they name -- and a label that lies about which frequency a
/// slider moves is worse than no label.
#[tauri::command]
pub async fn equalizer_bands() -> Vec<f32> {
    crate::equalizer::CENTRES.to_vec()
}

/// Resolves a playing track to something measurable, and measures it.
///
/// Prefers exact over approximate whenever exact is cheap:
///
/// 1. **A local file** -- measured in full. It is on disk, it costs about a
///    second, and an approximation would be strictly worse for no saving.
/// 2. **A cached stream** -- the same. The audio cache only ever holds complete
///    copies (a partial write is discarded rather than published), so this is
///    the whole track and the answer is exact. Reaching for the network here
///    would spend traffic to get a worse number than the one already on disk.
/// 3. **A cold stream** -- sampled, because there is nothing else to do. Four
///    tenths of it, in parallel, while the song plays.
///
/// Short tracks are declined outright: the background pass measures them in
/// full for about what sampling would cost, so an estimate buys nothing.
async fn sample_playing_track(
    pool: &SqlitePool,
    ffmpeg: &std::path::Path,
    yt_dlp: Option<&std::path::Path>,
    stream_urls: &StreamUrlCache,
    audio_cache: Option<&crate::audio_cache::AudioCache>,
    track_id: i64,
) -> Option<crate::loudness::Loudness> {
    let row = sqlx::query(
        "SELECT source, local_path, remote_id, remote_url, duration_secs \
           FROM tracks WHERE id = ?",
    )
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .ok()??;

    let duration: Option<f64> = row.try_get("duration_secs").ok().flatten();
    let duration = duration?;

    // On disk in one form or the other: measure it properly.
    let local: Option<String> = row.try_get("local_path").ok().flatten();
    let on_disk = local
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| {
            let source: String = row.try_get("source").ok()?;
            let remote_id: Option<String> = row.try_get("remote_id").ok().flatten();
            audio_cache?.lookup(&source, &remote_id?)
        });

    if let Some(path) = on_disk {
        let ffmpeg = ffmpeg.to_path_buf();
        return tokio::task::spawn_blocking(move || crate::loudness::measure(&ffmpeg, &path).ok())
            .await
            .ok()
            .flatten();
    }

    if !crate::loudness_sample::worth_sampling(duration) {
        return None;
    }

    // A cold stream. `resolve` hands back the URL playback is already using
    // while it is still valid, so in the ordinary case this costs no yt-dlp run.
    let page: Option<String> = row.try_get("remote_url").ok().flatten();
    let page = page?;
    let url = stream_urls
        .resolve(yt_dlp?, &page, crate::stream_urls::Encoding::Preferred)
        .await
        .ok()?;

    let ffmpeg = ffmpeg.to_path_buf();
    tokio::task::spawn_blocking(move || crate::loudness_sample::sample(&ffmpeg, &url, duration).ok())
        .await
        .ok()
        .flatten()
}

#[cfg(test)]
mod level_tests {
    use super::reading_applies_now;

    /// The ordinary case: the track that was measured is the one playing.
    #[test]
    fn a_reading_for_the_playing_track_is_applied() {
        assert!(reading_applies_now(Some(7), 7, true));
    }

    /// The failure this guard exists for.
    ///
    /// Sampling answers from a spawned task, seconds after it started. By then
    /// the listener may have skipped. Applying the previous song's gain to this
    /// one is the single most audible thing that could go wrong here -- a track
    /// suddenly playing at a level nobody measured for it.
    #[test]
    fn a_reading_for_a_track_that_has_been_skipped_is_not_applied() {
        assert!(!reading_applies_now(Some(8), 7, true));
    }

    /// Stopped between starting the measurement and finishing it.
    #[test]
    fn a_reading_arriving_after_playback_stopped_is_not_applied() {
        assert!(!reading_applies_now(None, 7, true));
    }

    /// Levelling switched off while the slices were in flight. The reading is
    /// still worth recording, but must not move a gain the listener has just
    /// asked not to have.
    #[test]
    fn a_reading_is_not_applied_when_levelling_is_off() {
        assert!(!reading_applies_now(Some(7), 7, false));
        assert!(!reading_applies_now(Some(8), 7, false));
        assert!(!reading_applies_now(None, 7, false));
    }
}

/// The loudness every track is corrected towards, in LUFS.
///
/// Offered because the right answer depends on where the music is going.
/// -14 is what YouTube and Spotify converge on and suits most listening. A
/// quieter target leaves more headroom, so fewer tracks are pulled down by the
/// limiter; a louder one gets closer to how a phone speaker wants to be driven,
/// at the cost of asking for boost that many tracks have no room for.
///
/// Clamped rather than rejected: it arrives from the frontend as JSON, and a
/// slider that silently did nothing would be a worse bug than one that stops.
#[tauri::command]
pub async fn set_target_lufs(lufs: f32, player: State<'_, PlayerHandle>) -> Result<(), String> {
    player.send(PlayerCommand::SetTargetLufs(lufs))
}
