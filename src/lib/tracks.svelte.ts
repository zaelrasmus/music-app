import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toast } from "svelte-sonner";
import { downloads } from "$lib/downloads.svelte";
import { libraryView } from "$lib/library-view.svelte";

export type Track = {
  id: number;
  source: "local" | "youtube" | "soundcloud";
  title: string;
  artist: string | null;
  album: string | null;
  durationSecs: number | null;
  /** Names a file in the cover store; null means generated artwork. */
  coverKey?: string | null;
  /**
   * The provider's own thumbnail, for a track whose cover was never stored.
   *
   * Artwork is only kept on disk once a track is kept — added to the library
   * or downloaded. Everything else shows this URL directly, which costs
   * nothing and looks identical while there is a network.
   */
  remoteThumbnailUrl?: string | null;
  /**
   * Whether the user keeps this in their library.
   *
   * Always true for local files. False for a streamed track that has been
   * played but never explicitly kept — it stays in history and stays playable,
   * it just is not filed in the library.
   */
  inLibrary?: boolean;
  state: "present" | "missing" | "saved" | "downloaded";
};

export type ScanSummary = {
  scanned: number;
  added: number;
  updated: number;
  unchanged: number;
  markedMissing: number;
  errors: number;
  /**
   * Files the scan gave up on rather than wait for.
   *
   * Named, not just counted: this is the one outcome that could be wrong, and
   * a legitimate file wrongly abandoned would otherwise be silently absent.
   */
  skippedFiles: string[];
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
  /**
   * How far the running scan has got.
   *
   * A thousand files takes long enough that a spinner alone is
   * indistinguishable from a hang. Null when nothing is scanning.
   */
  progress = $state<{ folder: string; file: string | null; done: number; total: number } | null>(
    null,
  );
  lastSummary = $state<ScanSummary | null>(null);
  error = $state<string | null>(null);

  /** Refetches whenever the backend reports a finished scan. */
  listenForScans() {
    const finished = listen("scan-finished", () => {
      this.load();
    });

    const progress = listen<{ folder: string; file: string | null; done: number; total: number }>(
      "scan-progress",
      ({ payload }) => {
        this.progress = payload;
      },
    );

    return Promise.all([finished, progress]).then(
      ([offFinished, offProgress]) => () => {
        offFinished();
        offProgress();
      },
    );
  }

  async load() {
    try {
      this.tracks = await invoke<Track[]>("list_tracks");
      this.error = null;
      // The library view runs its own filtered query, so it will not see an
      // edit, download or scan unless it is told to look again.
      await libraryView.refresh();
    } catch (e) {
      this.error = String(e);
    }
  }

  async rescan() {
    this.scanning = true;
    this.progress = null;
    try {
      // null means a scan was already running -- the backend refuses to start
      // a second one rather than interleaving two passes.
      const summary = await invoke<ScanSummary | null>("rescan_library");
      if (summary === null) {
        this.error = "A scan is already running.";
        toast.info("A scan is already running.");
        return;
      }
      this.lastSummary = summary;
      this.error = null;

      // Said out loud, where the user is. The summary was only ever shown in
      // Settings, so a rescan started from the library finished in silence --
      // indistinguishable from one that never finished at all.
      toast.success(describeScan(summary));
    } catch (e) {
      this.error = String(e);
      toast.error(String(e));
    } finally {
      this.scanning = false;
      this.progress = null;
    }
  }

  // --- offline downloads and metadata ---------------------------------

  /**
   * Whether this track is queued or downloading.
   *
   * Read from the download queue rather than tracked here. There used to be a
   * local list of ids, which was only ever a guess at what the backend was
   * doing and knew nothing about tracks queued by a playlist download.
   */
  isDownloading(trackId: number) {
    return downloads.jobs.some(
      (job) =>
        job.trackId === trackId &&
        (job.state === "queued" || job.state === "running"),
    );
  }

  /**
   * Queues a saved track for offline play.
   *
   * Returns as soon as it is queued, not when it is on disk. The command used
   * to run the whole download and only then come back, which meant a row that
   * sat spinning for a minute with no way to see why or what else was waiting.
   * The activity panel in the titlebar is where that lives now, so a click
   * here is a request rather than a wait.
   */
  async download(trackId: number) {
    await downloads.queueTrack(trackId);
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

/**
 * Files a track in the library, or takes it out.
 *
 * Nothing is destroyed either way: removing leaves the row, its history, its
 * cached audio and its playlist memberships exactly where they were. It is a
 * statement about filing, not about deletion.
 */
export async function setInLibrary(trackId: number, inLibrary: boolean) {
  try {
    await invoke("set_in_library", { trackId, inLibrary });
    return true;
  } catch (e) {
    toast.error(String(e));
    return false;
  }
}

/**
 * What a finished scan actually did, in one sentence.
 *
 * Only the parts that changed. "1019 scanned, 0 added, 0 updated, 1019
 * unchanged" is four numbers to read before learning that nothing happened.
 */
function describeScan(summary: ScanSummary) {
  const parts: string[] = [];
  if (summary.added > 0) parts.push(`${summary.added} added`);
  if (summary.updated > 0) parts.push(`${summary.updated} updated`);
  if (summary.markedMissing > 0) parts.push(`${summary.markedMissing} missing`);
  if (summary.errors > 0) parts.push(`${summary.errors} could not be read`);
  if (summary.skippedFiles.length > 0) {
    parts.push(
      `${summary.skippedFiles.length} skipped as too slow — see Settings`,
    );
  }

  const scanned = `Scanned ${summary.scanned} ${
    summary.scanned === 1 ? "file" : "files"
  }`;

  return parts.length > 0
    ? `${scanned} — ${parts.join(", ")}.`
    : `${scanned} — nothing changed.`;
}

export const trackStore = new TrackStore();
