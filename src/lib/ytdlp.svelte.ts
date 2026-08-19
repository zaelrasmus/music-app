import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toast } from "svelte-sonner";

/** Mirrors `updater::Status`. */
export type YtDlpStatus = {
  version: string | null;
  channel: string;
  checkedAt: number | null;
  updating: boolean;
  error: string | null;
  updated: boolean;
};

/**
 * The state of the tool that talks to YouTube and SoundCloud.
 *
 * Surfaced at all because this is the one component that breaks on its own,
 * without anyone changing anything here. When a tester says "it stopped
 * working", the version and the date of the last check are the two facts that
 * turn that sentence into something actionable — and the button next to them
 * is usually the fix.
 */
class YtDlpStore {
  version = $state<string | null>(null);
  channel = $state("nightly");
  checkedAt = $state<number | null>(null);
  updating = $state(false);
  error = $state<string | null>(null);

  /**
   * Whether this store started the check that is running.
   *
   * The backend emits the same status event either way, so without this a
   * manual check reports itself twice: once when the event lands and once
   * when the command returns.
   */
  private mine = false;

  private apply(status: YtDlpStatus) {
    this.version = status.version;
    this.channel = status.channel;
    this.checkedAt = status.checkedAt;
    this.updating = status.updating;
    this.error = status.error;
  }

  /**
   * Follows checks the backend starts by itself.
   *
   * Those are the interesting ones: the daily sweep, and the check a failed
   * track triggers. An update that lands while the user is staring at a track
   * that will not play is worth a word, because the retry is theirs to make.
   */
  listenForUpdates() {
    return listen<YtDlpStatus>("yt-dlp-status", ({ payload }) => {
      const finished = this.updating && !payload.updating;
      this.apply(payload);

      if (finished && payload.updated && !this.mine) {
        toast.success("Updated the YouTube extractor. Try that track again.");
      }
    });
  }

  /** Cheap unless the version is not known yet, which costs one process. */
  async refresh() {
    try {
      this.apply(await invoke<YtDlpStatus>("yt_dlp_status"));
    } catch (e) {
      // Informational only. Nothing here should interrupt playback.
      console.debug("could not read the yt-dlp status", e);
    }
  }

  /**
   * The button. Never rate limited, unlike the automatic checks: someone who
   * presses it has a reason, and "nothing happened" is not an answer they can
   * do anything with.
   */
  async check() {
    this.mine = true;
    this.updating = true;
    try {
      const status = await invoke<YtDlpStatus>("update_yt_dlp");
      this.apply(status);
      toast.success(
        status.updated
          ? `Updated to ${status.version}.`
          : "Already up to date.",
      );
    } catch (e) {
      this.updating = false;
      this.error = String(e);
      toast.error(String(e));
    } finally {
      this.mine = false;
    }
  }
}

export const ytDlp = new YtDlpStore();
