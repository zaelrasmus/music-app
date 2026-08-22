import { invoke } from "@tauri-apps/api/core";
import { toast } from "svelte-sonner";

/**
 * Which tracks have a loudness reading, and how to take one on demand.
 *
 * Ids in a set rather than a field on `Track`, the same shape the offline
 * badge uses: the reading changes on its own schedule -- a background pass, a
 * stream finishing -- and threading it through every query that builds a row
 * would couple all of them to a fact only two places display.
 */
class LoudnessStore {
  /** Tracks with a usable reading right now. */
  measuredIds = $state<Set<number>>(new Set());
  /** Tracks being measured this moment, so the row can say so. */
  measuring = $state<Set<number>>(new Set());

  /** Cheap: one indexed query, not a lookup per row. */
  async refresh() {
    try {
      const ids = await invoke<number[]>("measured_track_ids");
      this.measuredIds = new Set(ids);
    } catch {
      // Decorative. A failure here must not disturb playback.
    }
  }

  isMeasured(trackId: number) {
    return this.measuredIds.has(trackId);
  }

  isMeasuring(trackId: number) {
    return this.measuring.has(trackId);
  }

  /**
   * Measures one track now.
   *
   * The background pass is deliberately unhurried, which is right for a
   * thousand files and wrong for the one track someone is listening to.
   */
  async measure(trackId: number) {
    if (this.measuring.has(trackId)) return;
    // Reassigned rather than mutated: `$state` tracks the reference.
    this.measuring = new Set(this.measuring).add(trackId);

    try {
      const gainDb = await invoke<number | null>("measure_track", { trackId });
      this.measuredIds = new Set(this.measuredIds).add(trackId);
      if (gainDb === null || Math.abs(gainDb) < 0.05) {
        toast.success("Measured — this track is already at the target level.");
      } else {
        const rounded = Math.round(gainDb * 10) / 10;
        toast.success(
          `Measured — plays ${rounded > 0 ? "+" : ""}${rounded.toFixed(1)} dB from here.`,
        );
      }
    } catch (e) {
      toast.error(String(e));
    } finally {
      const next = new Set(this.measuring);
      next.delete(trackId);
      this.measuring = next;
    }
  }
}

export const loudnessStore = new LoudnessStore();
