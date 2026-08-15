import { invoke } from "@tauri-apps/api/core";
import { toast } from "svelte-sonner";
import { readSetting, writeSetting } from "$lib/settings.svelte";

export type CacheStats = {
  usedBytes: number;
  limitBytes: number;
};

const MB = 1024 * 1024;

/** Offered sizes. Roughly 0.93 MB per minute, so 1 GB is about 18 hours. */
export const LIMIT_CHOICES = [
  { label: "256 MB", bytes: 256 * MB },
  { label: "1 GB", bytes: 1024 * MB },
  { label: "4 GB", bytes: 4096 * MB },
  { label: "16 GB", bytes: 16384 * MB },
];

export function formatBytes(bytes: number) {
  if (bytes < MB) return `${Math.round(bytes / 1024)} KB`;
  if (bytes < 1024 * MB) return `${Math.round(bytes / MB)} MB`;
  return `${(bytes / (1024 * MB)).toFixed(1)} GB`;
}

/**
 * The disposable copies of streamed audio.
 *
 * Deliberately visible and adjustable rather than sized automatically: how
 * much disk to spend is the user's call, and asking the OS how much is free
 * would mean taking on a dependency for a number people can just be shown.
 */
class CacheStore {
  /**
   * Whether to fetch a complete copy of tracks left part-way through.
   *
   * Off by default: it spends data the user cannot see being spent. The free
   * path -- the copy written while a track plays to its end -- happens either
   * way, so turning this off does not mean nothing is kept.
   */
  keepAbandoned = $state(false);
  /**
   * Tracks with a cached copy right now.
   *
   * Refetched rather than remembered: an entry can be evicted at any moment,
   * and showing a track as playable offline when it is not would be the same
   * silent wrongness the cache rules exist to prevent.
   */
  cachedIds = $state<Set<number>>(new Set());
  usedBytes = $state(0);
  limitBytes = $state(1024 * MB);
  busy = $state(false);

  private apply(stats: CacheStats) {
    this.usedBytes = stats.usedBytes;
    this.limitBytes = stats.limitBytes;
  }

  private async run(command: string, args?: Record<string, unknown>) {
    this.busy = true;
    try {
      this.apply(await invoke<CacheStats>(command, args));
    } catch (e) {
      toast.error(String(e));
    } finally {
      this.busy = false;
    }
  }

  async refresh() {
    await this.run("audio_cache_stats");
    await this.refreshCached();
  }

  /** Cheap: one directory read and one query, not a lookup per row. */
  async refreshCached() {
    try {
      const ids = await invoke<number[]>("cached_track_ids");
      this.cachedIds = new Set(ids);
    } catch {
      // Purely decorative. A failure here must not disturb playback.
    }
  }

  isCached(trackId: number) {
    return this.cachedIds.has(trackId);
  }

  async setKeepAbandoned(enabled: boolean) {
    this.keepAbandoned = enabled;
    try {
      await invoke("set_keep_abandoned", { enabled });
      await writeSetting("keepAbandoned", enabled);
    } catch (e) {
      toast.error(String(e));
    }
  }

  /**
   * Pushes the saved limit to the backend, then reads back what it accepted.
   *
   * Same pattern as volume and repeat: the preference lives in the settings
   * store, and the backend is told at startup rather than persisting it twice.
   */
  async restore() {
    const saved = await readSetting("audioCacheLimitBytes", 1024 * MB);
    await this.run("set_audio_cache_limit", { limitBytes: saved });

    this.keepAbandoned = await readSetting("keepAbandoned", false);
    try {
      await invoke("set_keep_abandoned", { enabled: this.keepAbandoned });
    } catch (e) {
      toast.error(String(e));
    }
  }

  async setLimit(limitBytes: number) {
    await this.run("set_audio_cache_limit", { limitBytes });
    await writeSetting("audioCacheLimitBytes", limitBytes);
  }

  async clear() {
    await this.run("clear_audio_cache");
    toast.success("Cache cleared. Downloads were not touched.");
  }
}

export const cacheStore = new CacheStore();
