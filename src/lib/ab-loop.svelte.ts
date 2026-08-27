import { invoke } from "@tauri-apps/api/core";
import { player } from "$lib/player.svelte";

/**
 * The A-B loop, as the user builds it.
 *
 * Three states, and the middle one is why this is a store rather than two
 * numbers: between marking A and marking B there is a half-built loop that
 * exists only here. Sending it to the backend at that point would mean sending
 * something that cannot be looped, so the backend never sees a loop until both
 * ends are known.
 *
 * The points themselves are then owned by the coordinator, which drops them
 * when the track changes — thirty seconds is a chorus in one song and a verse
 * in the next.
 */
class AbLoopStore {
  /** Where A was marked, before B exists. */
  pending = $state<number | null>(null);

  /** Mirrors the coordinator, so what is drawn is what will actually loop. */
  points = $state<[number, number] | null>(null);

  get active() {
    return this.points !== null;
  }

  /** Takes the backend's word for it, from every `player-state` event. */
  sync(points: [number, number] | null) {
    this.points = points;
    // A loop arriving from the backend settles any half-built one here.
    if (points) this.pending = null;
  }

  /**
   * The one button: mark A, then mark B, then clear.
   *
   * A single control rather than three, because the three states are
   * sequential and a person doing this is listening rather than looking — the
   * gesture is "press it at the start, press it at the end".
   */
  async mark() {
    if (this.points) {
      await this.clear();
      return;
    }

    const at = player.positionSecs;

    if (this.pending === null) {
      this.pending = at;
      return;
    }

    const a = Math.min(this.pending, at);
    const b = Math.max(this.pending, at);
    this.pending = null;
    await invoke("set_loop_points", { points: [a, b] });
  }

  async clear() {
    this.pending = null;
    this.points = null;
    await invoke("set_loop_points", { points: null });
  }
}

export const abLoop = new AbLoopStore();
