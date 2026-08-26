import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toast } from "svelte-sonner";
import { readSetting, writeSetting } from "$lib/settings.svelte";
import { cacheStore } from "$lib/cache.svelte";
import { loudnessStore } from "$lib/loudness.svelte";
import { historyStore } from "$lib/history.svelte";

export type PlaybackState = "loading" | "playing" | "paused" | "stopped";
export type RepeatMode = "off" | "all" | "one";

/** The consolidated snapshot the coordinator emits on every change. */
export type PlayerStatus = {
  state: PlaybackState;
  trackId: number | null;
  repeat: RepeatMode;
  shuffle: boolean;
  volume: number;
  muted: boolean;
  /**
   * The *context* queue, not the total. The manual queue is not positional,
   * so "3 of 12" would be a lie the moment anything is queued.
   */
  contextLength: number;
  contextPosition: number;
  manualLength: number;
  /** Whether the queue recycles instead of draining. */
  loopQueue: boolean;
  /** The chosen top of the slider, in dB below unity. 0 passes audio through. */
  volumeCeilingDb: number;
  targetLufs: number;
  /** The stream has run dry without ending: the connection is not keeping up. */
  stalled: boolean;
  /** Whether per-track loudness correction is on. */
  normalize: boolean;
  /**
   * What the current track is being corrected by, in dB. `null` means it has
   * not been measured yet — the honest state for a stream nobody has finished
   * playing once — and is distinct from a measured correction of zero.
   */
  trackGainDb: number | null;
  /** Whether an unheard stream is measured before it starts playing. */
};

export type PlayerProgress = {
  trackId: number | null;
  positionSecs: number;
};

const REPEAT_CYCLE: RepeatMode[] = ["off", "all", "one"];

/**
 * Mirrors the backend player.
 *
 * Nothing here is optimistic: every field is only ever written from a
 * `player-state` event, so the UI always shows what the coordinator actually
 * decided rather than what we asked for. Preferences are persisted in the
 * action methods (not the event handler), so echoing state back never causes
 * a redundant write.
 */
class PlayerStore {
  state = $state<PlaybackState>("stopped");
  trackId = $state<number | null>(null);
  repeat = $state<RepeatMode>("off");
  shuffle = $state(false);
  volume = $state(1);
  muted = $state(false);
  contextLength = $state(0);
  contextPosition = $state(0);
  manualLength = $state(0);
  loopQueue = $state(false);
  volumeCeilingDb = $state(0);
  /** The loudness every track is corrected towards. */
  targetLufs = $state(-14);
  normalize = $state(false);
  trackGainDb = $state<number | null>(null);
  stalled = $state(false);

  /** Authoritative position from the backend, in seconds. */
  positionSecs = $state(0);

  /**
   * True while the user is dragging the progress handle.
   *
   * Backend ticks are ignored for the duration. Without this the 5/sec tick
   * overwrites the drag and the handle jumps backwards under the cursor.
   */
  scrubbing = $state(false);
  /** Where the handle sits mid-drag, before the seek is committed. */
  scrubSecs = $state(0);

  /**
   * What the progress bar should display right now.
   *
   * Rounded to whole seconds on purpose. The slider snaps any value that is
   * not exactly on its step and writes the snapped value *back* through its
   * change callback — indistinguishable from the user grabbing the handle.
   * With fractional positions that latched `scrubbing` on permanently and
   * froze the bar. Keeping the value on-step means no write-back happens.
   */
  get displaySecs() {
    return this.scrubbing ? this.scrubSecs : Math.round(this.positionSecs);
  }

  async listenForPlayer() {
    const unlistenState = await listen<PlayerStatus>("player-state", (e) => {
      const s = e.payload;
      const previousTrack = this.trackId;
      this.state = s.state;
      this.trackId = s.trackId;
      this.repeat = s.repeat;
      this.shuffle = s.shuffle;
      this.volume = s.volume;
      this.muted = s.muted;
      this.contextLength = s.contextLength;
      this.contextPosition = s.contextPosition;
      this.manualLength = s.manualLength;
      this.loopQueue = s.loopQueue;
      this.volumeCeilingDb = s.volumeCeilingDb;
      this.targetLufs = s.targetLufs;
      this.normalize = s.normalize;
      this.trackGainDb = s.trackGainDb;
      this.stalled = s.stalled;

      // Leaving a track is when one most often becomes cached, so this is the
      // moment the badges elsewhere go stale.
      if (s.trackId !== previousTrack) {
        // A position belongs to the track it was measured in. Carrying the
        // old one over means the bar keeps counting where the last track got
        // to -- for a stream, the several seconds until the new track's first
        // tick arrives, which reads as "the song did not restart".
        //
        // Zero rather than the resume point: the backend sends that as a
        // progress tick immediately after this, and it is the only thing that
        // knows there is one.
        this.positionSecs = 0;
        // The handle cannot still be being dragged in a track that is no
        // longer playing, and leaving the flag set would freeze the bar.
        this.scrubbing = false;
        void cacheStore.refreshCached();
        // The background pass may have measured things since the last track.
        void loudnessStore.refresh();
        // The track just left may have become a history entry; the backend
        // decides whether it counted, so ask rather than guess.
        void historyStore.load();
      }

      // Only when nothing is shown at all. A stopped state with a track
      // still in the bar is where the user left off -- including the one
      // restored at startup, which would otherwise be wiped the moment it
      // arrived.
      if (s.state === "stopped" && s.trackId === null) this.positionSecs = 0;
    });

    const unlistenProgress = await listen<PlayerProgress>(
      "player-progress",
      (e) => {
        // A tick that arrived just after a track change belongs to the old
        // track; and while scrubbing the user owns the handle, not the clock.
        if (this.scrubbing) return;
        if (e.payload.trackId !== this.trackId) return;
        this.positionSecs = e.payload.positionSecs;
        this.rememberPosition();
      },
    );

    // Resolution failures happen after the command returns, so they arrive
    // here rather than as a rejected invoke.
    const unlistenError = await listen<string>("player-error", (e) => {
      toast.error(e.payload);
    });

    return () => {
      unlistenState();
      unlistenProgress();
      unlistenError();
    };
  }

  /**
   * How often the resume point is written.
   *
   * Ticks arrive several times a second; persisting each one would be a
   * write per tick for a value nobody reads until the next launch. Losing at
   * most a few seconds of position is imperceptible.
   */
  #lastSaved = 0;

  private rememberPosition() {
    if (this.trackId === null || this.positionSecs <= 0) return;

    const now = Date.now();
    if (now - this.#lastSaved < 5000) return;
    this.#lastSaved = now;

    void writeSetting("resume", {
      trackId: this.trackId,
      positionSecs: this.positionSecs,
    });
  }

  /**
   * Puts the last session's track back in the bar, where it was left.
   *
   * Nothing is fetched: the backend holds the position and applies it to the
   * load only if the user actually presses play. Resolving a stream here
   * would cost seconds before the window was even usable.
   */
  async restorePlayback() {
    const saved = await readSetting<{
      trackId: number;
      positionSecs: number;
    } | null>("resume", null);

    if (!saved) return;

    await this.run("restore_playback", {
      trackId: saved.trackId,
      positionSecs: saved.positionSecs,
    });
  }

  /** Restores persisted preferences and pushes them to the backend. */
  async restorePreferences() {
    const [volume, muted, repeat, shuffle, ceiling, normalize, target] =
      await Promise.all([
      readSetting("volume", 1),
      readSetting("muted", false),
      readSetting<RepeatMode>("repeat", "off"),
      readSetting("shuffle", false),
      // Defaults to passing the audio through, like every other player.
      // Choosing a quieter default for everyone is the mistake this
      // setting exists to undo.
      readSetting("volumeCeilingDb", 0),
      // Off by default: it changes how every track sounds, so it should be a
      // thing someone turned on rather than something that happened to them.
      readSetting("normalizeLoudness", false),
      // What levelling aims at. -14 LUFS is where YouTube and Spotify sit, so
      // it is the level this library was mostly mastered near.
      readSetting("targetLufs", -14),
    ]);

    await Promise.all([
      invoke("set_volume", { volume }),
      invoke("set_muted", { muted }),
      invoke("set_repeat", { mode: repeat }),
      invoke("set_shuffle", { shuffle }),
      invoke("set_volume_ceiling", { db: ceiling }),
      invoke("set_normalize", { on: normalize }),
      invoke("set_target_lufs", { lufs: target }),
    ]);
  }

  isCurrent(trackId: number) {
    return this.trackId === trackId;
  }

  private async run(command: string, args?: Record<string, unknown>) {
    try {
      await invoke(command, args);
    } catch (e) {
      toast.error(String(e));
    }
  }

  /**
   * Replaces the *context* queue and starts playing at `startIndex`.
   *
   * Deliberately leaves the manual queue alone — tracks the user queued are a
   * separate intention from "play this playlist", and losing them because
   * they clicked something else is the behaviour people notice.
   *
   * `contextName` is the "Next from …" heading.
   */
  async playQueue(trackIds: number[], startIndex: number, contextName?: string) {
    await this.run("play_queue", {
      trackIds,
      startIndex,
      contextName: contextName ?? null,
    });
  }

  /** Queues `trackId` to play right after the current track. */
  async playNext(trackId: number) {
    await this.run("play_next", { trackId });
  }

  /** Queues `trackId` behind everything already queued. */
  async addToQueue(trackId: number) {
    await this.run("add_to_queue", { trackId });
  }

  async togglePlayPause() {
    await this.run("toggle_play_pause");
  }

  async next() {
    await this.run("next_track");
  }

  async previous() {
    await this.run("previous_track");
  }

  async stop() {
    await this.run("stop");
  }

  /**
   * Live while the volume slider is being dragged.
   *
   * Moves the audio and nothing else. `setVolume` persists, and persisting
   * means `store.save()`, which is a synchronous flush to settings.json --
   * fine once on release, wrong on every pointermove. `ScrubBar` says as much
   * in its own docs: `onScrub` is for cheap operations, `onCommit` is "the one
   * that costs something".
   */
  async previewVolume(volume: number) {
    await this.run("set_volume", { volume });
  }

  /**
   * Turns per-track loudness correction on or off.
   *
   * Applies to the track already playing, which is deliberate: judging this by
   * ear needs the same passage both ways, not a restart.
   */
  async setNormalize(on: boolean) {
    await this.run("set_normalize", { on });
    await writeSetting("normalizeLoudness", on);
  }

  async setVolume(volume: number) {
    await this.run("set_volume", { volume });
    await writeSetting("volume", volume);
    // Adjusting the slider implicitly unmutes, which the backend also does.
    if (volume > 0 && this.muted) await writeSetting("muted", false);
  }

  async toggleMute() {
    const muted = !this.muted;
    await this.run("set_muted", { muted });
    await writeSetting("muted", muted);
  }

  async cycleRepeat() {
    const next =
      REPEAT_CYCLE[(REPEAT_CYCLE.indexOf(this.repeat) + 1) % REPEAT_CYCLE.length];
    await this.run("set_repeat", { mode: next });
    await writeSetting("repeat", next);
  }

  async toggleShuffle() {
    const shuffle = !this.shuffle;
    await this.run("set_shuffle", { shuffle });
    await writeSetting("shuffle", shuffle);
  }

  /**
   * Plays the queued tracks round and round instead of consuming them.
   *
   * Deliberately not persisted. Repeat and shuffle are standing preferences;
   * this is about the handful of tracks in the queue right now, and having it
   * survive a restart that empties the queue would be a setting about nothing.
   */
  async toggleLoopQueue() {
    await this.run("set_loop_queue", { on: !this.loopQueue });
  }

  /**
   * Sets the loudest the app may get, in dB below unity.
   *
   * Applied to what is already playing, not the next track — a control
   * whose effect you cannot hear is one you cannot judge.
   */
  /**
   * The loudness every track is corrected towards.
   *
   * Heard immediately, because the gain is derived from it: moving this and
   * hearing nothing until the next track is how a setting gets called broken.
   */
  async setTargetLufs(lufs: number) {
    await this.run("set_target_lufs", { lufs });
    await writeSetting("targetLufs", lufs);
  }

  async setVolumeCeiling(db: number) {
    await this.run("set_volume_ceiling", { db });
    await writeSetting("volumeCeilingDb", db);
  }

  /** Called continuously while dragging — updates the handle, does not seek. */
  scrubTo(positionSecs: number) {
    // The slider also calls this when it normalises a value we pushed in,
    // which is not the user touching anything. Treating that as a drag would
    // latch `scrubbing` and stop the bar updating forever.
    if (positionSecs === this.displaySecs) return;

    this.scrubbing = true;
    this.scrubSecs = positionSecs;
  }

  /**
   * Called on release. Seeking on every drag event would fire a blocking
   * `try_seek` per pixel of travel.
   */
  async commitScrub(positionSecs: number) {
    this.scrubSecs = positionSecs;
    // Show the target straight away, then let ticks resume from the backend.
    this.positionSecs = positionSecs;
    this.scrubbing = false;
    await this.run("seek", { positionSecs });
  }
}

export const player = new PlayerStore();
