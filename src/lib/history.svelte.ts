import { invoke } from "@tauri-apps/api/core";
import { toast } from "svelte-sonner";
import type { Track } from "$lib/tracks.svelte";

/**
 * How many recent tracks to show.
 *
 * A display choice rather than a storage one: a play is recorded on the track
 * row itself, so nothing is discarded by showing fewer. Fifty covers a few
 * days of listening and still scrolls sensibly; a few hundred would be more
 * than anyone reads.
 */
const HISTORY_LIMIT = 50;

/**
 * Recently played tracks, local and streamed together.
 *
 * Reloaded rather than appended to as tracks play: the backend decides when a
 * listen counts, and mirroring that rule here would be two places to get it
 * wrong.
 */
class HistoryStore {
  tracks = $state<Track[]>([]);

  async load() {
    try {
      this.tracks = await invoke<Track[]>("recently_played", {
        limit: HISTORY_LIMIT,
      });
    } catch (e) {
      toast.error(String(e));
    }
  }
}

export const historyStore = new HistoryStore();
