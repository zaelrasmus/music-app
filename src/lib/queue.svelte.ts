import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { toast } from "svelte-sonner";

/** One row in the queue panel, hydrated by the backend. */
export type QueueEntry = {
  /**
   * Present only for manual entries.
   *
   * The manual queue is addressed by this rather than by list index: the
   * front pops whenever a track ends, so an index captured when the panel
   * rendered can point at a different row by the time it is clicked. It is
   * also what keys the list — the same track may legitimately be queued
   * twice, so `trackId` is not unique here.
   */
  entryId: number | null;
  trackId: number;
  title: string;
  artist: string | null;
  durationSecs: number | null;
  state: string;
  source: string;
  coverKey: string | null;
  /** The provider thumbnail, for a row with no stored cover. */
  remoteThumbnailUrl: string | null;
};

export type QueueState = {
  current: QueueEntry | null;
  manual: QueueEntry[];
  upNext: QueueEntry[];
  contextName: string | null;
  contextRemaining: number;
};

/**
 * Mirrors the backend's two-tier queue.
 *
 * Nothing here is optimistic. Every mutation goes to the coordinator and the
 * panel redraws from the event it emits back, so what you see is always what
 * will actually play — the alternative is a list that briefly disagrees with
 * the player, which is exactly the bug this panel exists to prevent.
 */
class QueueStore {
  current = $state<QueueEntry | null>(null);
  manual = $state<QueueEntry[]>([]);
  upNext = $state<QueueEntry[]>([]);
  contextName = $state<string | null>(null);
  contextRemaining = $state(0);

  /** Whether the panel is showing. */
  open = $state(false);

  async listenForQueue() {
    const unlisten = await listen<QueueState>("player-queue", (e) => {
      const q = e.payload;
      this.current = q.current;
      this.manual = q.manual;
      this.upNext = q.upNext;
      this.contextName = q.contextName;
      this.contextRemaining = q.contextRemaining;
    });

    // The coordinator only emits on change, so a panel mounting into an
    // already-running player would otherwise start empty.
    await this.refresh();

    return unlisten;
  }

  private async run(command: string, args?: Record<string, unknown>) {
    try {
      await invoke(command, args);
    } catch (e) {
      toast.error(String(e));
    }
  }

  /** Asks the coordinator to re-emit the current queue. */
  async refresh() {
    await this.run("request_queue_state");
  }

  toggle() {
    this.open = !this.open;
  }

  async remove(entryId: number) {
    await this.run("remove_from_queue", { entryId });
  }

  async reorder(entryId: number, toIndex: number) {
    await this.run("reorder_queue", { entryId, toIndex });
  }

  async clear() {
    await this.run("clear_queue");
  }
}

export const queueStore = new QueueStore();
