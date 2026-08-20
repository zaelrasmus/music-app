import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toast } from "svelte-sonner";
import { trackStore } from "$lib/tracks.svelte";
import { libraryView } from "$lib/library-view.svelte";

export type JobState = "queued" | "running" | "done" | "failed";

export type Job = {
  id: number;
  trackId: number;
  title: string;
  artist: string | null;
  groupId: number | null;
  state: JobState;
  error: string | null;
};

export type Group = { id: number; name: string };

/** A track being filled into the streaming cache in the background. */
export type Caching = { trackId: number; title: string };

export type Activity = {
  jobs: Job[];
  groups: Group[];
  caching: Caching[];
};

/** A batch and its tracks, as the panel shows it. */
export type GroupedJobs = {
  group: Group;
  jobs: Job[];
  done: number;
  failed: number;
};

/**
 * What the app is busy doing.
 *
 * Downloads used to be invisible: a menu item that either worked or produced a
 * toast several seconds later. That is survivable for one track and not for a
 * playlist, where the honest answer to "is it doing anything" was to watch the
 * library and wait.
 */
class DownloadStore {
  jobs = $state<Job[]>([]);
  groups = $state<Group[]>([]);
  caching = $state<Caching[]>([]);

  /** Which batches the user has expanded. Panel state, not app state. */
  expanded = $state<Set<number>>(new Set());

  private apply(activity: Activity) {
    const finished = this.pending;

    this.jobs = activity.jobs;
    this.groups = activity.groups;
    this.caching = activity.caching;

    // A download changes what the library holds — the track becomes
    // `downloaded` and joins the library — and nothing else would notice.
    if (finished > 0 && this.pending === 0) {
      void trackStore.load();
      void libraryView.refresh();
    }
  }

  /** Work outstanding. What the button counts. */
  get pending() {
    return this.jobs.filter(
      (job) => job.state === "queued" || job.state === "running",
    ).length;
  }

  get running() {
    return this.jobs.find((job) => job.state === "running") ?? null;
  }

  get failed() {
    return this.jobs.filter((job) => job.state === "failed").length;
  }

  /** True when anything is happening at all, downloads or caching. */
  get busy() {
    return this.pending > 0 || this.caching.length > 0;
  }

  /** Jobs that belong to no batch, newest last. */
  get loose() {
    return this.jobs.filter((job) => job.groupId === null);
  }

  /**
   * Batches with their tracks and progress.
   *
   * Built here rather than in the component so the panel can stay a list of
   * rows: a group is one line until it is opened, which is the whole reason
   * fifty queued tracks are readable at all.
   */
  get grouped(): GroupedJobs[] {
    return this.groups.map((group) => {
      const jobs = this.jobs.filter((job) => job.groupId === group.id);
      return {
        group,
        jobs,
        done: jobs.filter((job) => job.state === "done").length,
        failed: jobs.filter((job) => job.state === "failed").length,
      };
    });
  }

  toggleGroup(id: number) {
    const next = new Set(this.expanded);
    if (!next.delete(id)) next.add(id);
    this.expanded = next;
  }

  listenForActivity() {
    return listen<Activity>("download-activity", ({ payload }) =>
      this.apply(payload),
    );
  }

  async refresh() {
    try {
      this.apply(await invoke<Activity>("download_activity"));
    } catch (e) {
      console.debug("could not read download activity", e);
    }
  }

  async queueTrack(trackId: number) {
    try {
      await invoke("download_track", { trackId });
    } catch (e) {
      toast.error(String(e));
    }
  }

  /** Queues everything in a playlist that is not already on this device. */
  async queuePlaylist(playlistId: number) {
    try {
      const queued = await invoke<number>("download_playlist", { playlistId });
      toast.success(
        `Downloading ${queued} ${queued === 1 ? "track" : "tracks"} — one at a time.`,
      );
    } catch (e) {
      toast.error(String(e));
    }
  }

  async cancel(jobId: number) {
    try {
      await invoke("cancel_download", { jobId });
    } catch (e) {
      toast.error(String(e));
    }
  }

  async clearFinished() {
    try {
      await invoke("clear_finished_downloads");
    } catch (e) {
      toast.error(String(e));
    }
  }
}

export const downloads = new DownloadStore();
