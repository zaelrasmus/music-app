import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type Track = {
  id: number;
  title: string;
  artist: string | null;
  album: string | null;
  durationSecs: number | null;
  state: "present" | "missing" | "saved" | "downloaded";
};

export type ScanSummary = {
  scanned: number;
  added: number;
  updated: number;
  unchanged: number;
  markedMissing: number;
  errors: number;
  skippedFolders: string[];
};

/**
 * Track list plus scan control.
 *
 * Safe as a module-level singleton: `ssr = false` means there is no server, so
 * the usual cross-request leak hazard of module-level $state cannot occur.
 */
class TrackStore {
  tracks = $state<Track[]>([]);
  scanning = $state(false);
  lastSummary = $state<ScanSummary | null>(null);
  error = $state<string | null>(null);

  /** Refetches whenever the backend reports a finished scan. */
  listenForScans() {
    return listen("scan-finished", () => {
      this.load();
    });
  }

  async load() {
    try {
      this.tracks = await invoke<Track[]>("list_tracks");
      this.error = null;
    } catch (e) {
      this.error = String(e);
    }
  }

  async rescan() {
    this.scanning = true;
    try {
      // null means a scan was already running -- the backend refuses to start
      // a second one rather than interleaving two passes.
      const summary = await invoke<ScanSummary | null>("rescan_library");
      if (summary === null) {
        this.error = "A scan is already running.";
        return;
      }
      this.lastSummary = summary;
      this.error = null;
    } catch (e) {
      this.error = String(e);
    } finally {
      this.scanning = false;
    }
  }
}

export const trackStore = new TrackStore();
