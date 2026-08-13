import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { trackStore } from "$lib/tracks.svelte";

export type LibraryFolder = {
  id: number;
  path: string;
  /** Unix seconds. */
  addedAt: number;
};

/**
 * Library folder state.
 *
 * A module-level singleton is safe here: `ssr = false` in +layout.ts means
 * there is no server, so the usual "shared across requests" hazard of
 * module-level $state cannot occur.
 */
class LibraryStore {
  folders = $state<LibraryFolder[]>([]);
  loading = $state(false);
  /** Last failure, shown inline. Cleared on the next successful action. */
  error = $state<string | null>(null);

  async load() {
    this.loading = true;
    try {
      this.folders = await invoke<LibraryFolder[]>("list_library_folders");
      this.error = null;
    } catch (e) {
      this.error = String(e);
    } finally {
      this.loading = false;
    }
  }

  /**
   * Opens the native picker, then persists the choice. No-op if cancelled.
   *
   * Scans immediately afterwards so a newly added folder's tracks appear
   * without the user having to press Rescan.
   */
  async addFromPicker() {
    const picked = await open({ directory: true, title: "Add music folder" });
    if (picked === null) return;

    try {
      const folder = await invoke<LibraryFolder>("add_library_folder", {
        path: picked,
      });
      this.folders.push(folder);
      this.error = null;
      await trackStore.rescan();
    } catch (e) {
      this.error = String(e);
    }
  }

  async remove(id: number) {
    try {
      await invoke("remove_library_folder", { id });
      this.folders = this.folders.filter((f) => f.id !== id);
      this.error = null;
      // Its tracks are now 'missing' rather than deleted, so the list changed.
      await trackStore.load();
    } catch (e) {
      this.error = String(e);
    }
  }
}

export const library = new LibraryStore();
