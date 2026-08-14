import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toast } from "svelte-sonner";
import { readSetting, writeSetting } from "$lib/settings.svelte";

export type PlaybackState = "playing" | "paused" | "stopped";
export type RepeatMode = "off" | "all" | "one";

/** The consolidated snapshot the coordinator emits on every change. */
export type PlayerStatus = {
  state: PlaybackState;
  trackId: number | null;
  repeat: RepeatMode;
  shuffle: boolean;
  volume: number;
  muted: boolean;
  queueLength: number;
  queuePosition: number;
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
  queueLength = $state(0);
  queuePosition = $state(0);

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
      this.state = s.state;
      this.trackId = s.trackId;
      this.repeat = s.repeat;
      this.shuffle = s.shuffle;
      this.volume = s.volume;
      this.muted = s.muted;
      this.queueLength = s.queueLength;
      this.queuePosition = s.queuePosition;

      if (s.state === "stopped") this.positionSecs = 0;
    });

    const unlistenProgress = await listen<PlayerProgress>(
      "player-progress",
      (e) => {
        // A tick that arrived just after a track change belongs to the old
        // track; and while scrubbing the user owns the handle, not the clock.
        if (this.scrubbing) return;
        if (e.payload.trackId !== this.trackId) return;
        this.positionSecs = e.payload.positionSecs;
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

  /** Restores persisted preferences and pushes them to the backend. */
  async restorePreferences() {
    const [volume, muted, repeat, shuffle] = await Promise.all([
      readSetting("volume", 1),
      readSetting("muted", false),
      readSetting<RepeatMode>("repeat", "off"),
      readSetting("shuffle", false),
    ]);

    await Promise.all([
      invoke("set_volume", { volume }),
      invoke("set_muted", { muted }),
      invoke("set_repeat", { mode: repeat }),
      invoke("set_shuffle", { shuffle }),
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

  /** Plays `trackIds` as the queue, starting at `startIndex`. */
  async playQueue(trackIds: number[], startIndex: number) {
    await this.run("play_queue", { trackIds, startIndex });
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
