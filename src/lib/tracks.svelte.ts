import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toast } from "svelte-sonner";

export type Track = {
  id: number;
  source: "local" | "youtube";
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

  // --- offline downloads and metadata ---------------------------------

  /** Track ids with a download in flight, so each row can show progress. */
  downloading = $state<number[]>([]);

  isDownloading(trackId: number) {
    return this.downloading.includes(trackId);
  }

  /** Fetches a saved YouTube track for offline play. */
  async download(trackId: number) {
    if (this.isDownloading(trackId)) return;
    this.downloading.push(trackId);

    try {
      await invoke("download_track", { trackId });
      await this.load();
    } catch (e) {
      toast.error(String(e));
    } finally {
      this.downloading = this.downloading.filter((id) => id !== trackId);
    }
  }

  /** Removes the downloaded file, returning the track to `saved`. */
  async deleteDownload(trackId: number) {
    try {
      await invoke("delete_download", { trackId });
      await this.load();
    } catch (e) {
      toast.error(String(e));
    }
  }

  /**
   * Renames a track for display. The original YouTube title and channel are
   * kept separately by the backend and are not touched.
   */
  async updateMetadata(trackId: number, title: string, artist: string | null) {
    try {
      await invoke("update_track_metadata", { trackId, title, artist });
      await this.load();
      return true;
    } catch (e) {
      toast.error(String(e));
      return false;
    }
  }
}

export const trackStore = new TrackStore();
