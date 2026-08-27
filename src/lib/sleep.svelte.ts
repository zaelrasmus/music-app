import { invoke } from "@tauri-apps/api/core";

/**
 * The choices offered, in minutes.
 *
 * Short enough at the bottom to be a nap, long enough at the top to be an
 * album. "End of track" is not on this list because it is not a duration —
 * see [`SleepStore.endOfTrack`].
 */
export const SLEEP_MINUTES = [5, 10, 15, 30, 45, 60, 90] as const;

/**
 * The sleep timer's countdown.
 *
 * The backend owns the deadline and does the pausing; this exists only to draw
 * a number that changes every second. Doing that from the backend would mean
 * an event a second for something nobody is looking at most of the time, which
 * is exactly the churn `player-progress` was kept separate to avoid.
 *
 * So the backend reports how long is left *when state changes*, and this
 * counts down locally from there. The two can drift by a fraction of a second
 * and it does not matter: the number is a reassurance, not a contract, and the
 * pause happens on the backend's clock either way.
 */
class SleepStore {
  /** Seconds left, or `null` when no timer is set. */
  remaining = $state<number | null>(null);

  /** True when the timer is waiting for the track to finish, not a clock. */
  endOfTrack = $state(false);

  #deadline: number | null = null;
  #ticker: ReturnType<typeof setInterval> | null = null;

  get armed() {
    return this.remaining !== null || this.endOfTrack;
  }

  /**
   * Takes the backend's word for it.
   *
   * Called from every `player-state` event, which is also what makes a timer
   * that fired on the backend disappear here: it stops reporting one.
   */
  sync(secs: number | null, endOfTrack: boolean) {
    this.endOfTrack = endOfTrack;

    if (secs === null) {
      this.#deadline = null;
      this.remaining = null;
      this.#stop();
      return;
    }

    this.#deadline = Date.now() + secs * 1000;
    this.remaining = Math.max(0, Math.round(secs));
    this.#start();
  }

  async setMinutes(minutes: number) {
    await invoke("set_sleep_timer", { sleep: { kind: "in", secs: minutes * 60 } });
  }

  async setEndOfTrack() {
    await invoke("set_sleep_timer", { sleep: { kind: "endOfTrack" } });
  }

  async cancel() {
    await invoke("set_sleep_timer", { sleep: null });
  }

  #start() {
    if (this.#ticker !== null) return;
    this.#ticker = setInterval(() => {
      if (this.#deadline === null) return this.#stop();
      const left = Math.max(0, Math.round((this.#deadline - Date.now()) / 1000));
      this.remaining = left;
      // At zero the backend is pausing; it will send a state event saying so,
      // and that is what clears this.
      if (left === 0) this.#stop();
    }, 1000);
  }

  #stop() {
    if (this.#ticker === null) return;
    clearInterval(this.#ticker);
    this.#ticker = null;
  }
}

/** `5:00`, or `0:07`. */
export function formatCountdown(secs: number) {
  const total = Math.max(0, Math.round(secs));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

export const sleepStore = new SleepStore();
