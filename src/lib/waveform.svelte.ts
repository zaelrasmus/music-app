import { invoke } from "@tauri-apps/api/core";
import { extras } from "$lib/extras.svelte";

/**
 * The shape of the track in the bar.
 *
 * One request per track, and the backend caches the answer in the row — so
 * this is a fetch on first play and a query every time after. Held for one
 * track only: keeping a map would be caching a cache.
 *
 * `null` is a real answer and the common one. A stream has no file to measure
 * and never gets a shape, which the bar draws by simply not drawing one.
 */
class WaveformStore {
  /** Peaks, 0–255, one per column. `null` when this track has no shape. */
  peaks = $state<Uint8Array | null>(null);

  #loadedFor: number | null = null;

  /**
   * Asks again for the track already loaded.
   *
   * Needed because [`load`] short-circuits on the track it already answered
   * for, and the two moments that change the answer without changing the track
   * are the setting being switched on and the preference finishing loading at
   * startup. Without this, turning the waveform on would draw nothing until
   * the next song.
   */
  async reload(trackId: number | null) {
    this.#loadedFor = null;
    await this.load(trackId);
  }

  async load(trackId: number | null) {
    if (trackId === this.#loadedFor) return;

    this.#loadedFor = trackId;
    this.peaks = null;
    if (trackId === null) return;

    // Checked here rather than at the drawing end, so someone who does not want
    // a waveform never pays for one: no ffmpeg run, no decode, nothing stored.
    // Measuring and then hiding it would be the expensive half of the feature
    // with none of the benefit.
    if (!extras.waveform) return;

    try {
      const found = await invoke<number[] | null>("track_waveform", { trackId });
      // Measuring can take a moment, and the track may have moved on. A shape
      // drawn under the wrong song is worse than none.
      if (this.#loadedFor !== trackId) return;
      this.peaks = found ? Uint8Array.from(found) : null;
    } catch {
      if (this.#loadedFor === trackId) this.peaks = null;
    }
  }
}

export const waveform = new WaveformStore();
