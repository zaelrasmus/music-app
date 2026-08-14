import { invoke } from "@tauri-apps/api/core";
import { toast } from "svelte-sonner";
import type { Track } from "$lib/tracks.svelte";

export type ArtistGroup = {
  artist: string;
  tracks: Track[];
};

/** Same reasoning as the YouTube search box — this one is local, so shorter. */
const DEBOUNCE_MS = 200;

/**
 * The library view: text search, tag filtering, and artist grouping.
 *
 * Search and tag filtering are one backend query, not two lists intersected
 * here — the database does the set logic and only matching rows cross the
 * boundary.
 */
class LibraryViewStore {
  query = $state("");
  selectedTagIds = $state<number[]>([]);
  mode = $state<"all" | "any">("all");
  groupByArtist = $state(false);

  results = $state<Track[]>([]);
  groups = $state<ArtistGroup[]>([]);
  loading = $state(false);

  /** Local queries are fast but still finish out of order. */
  #latestRequest = 0;
  #debounce: ReturnType<typeof setTimeout> | undefined;

  get filtering() {
    return this.query.trim() !== "" || this.selectedTagIds.length > 0;
  }

  setQuery(query: string) {
    this.query = query;
    clearTimeout(this.#debounce);
    this.#debounce = setTimeout(() => this.refresh(), DEBOUNCE_MS);
  }

  toggleTag(tagId: number) {
    this.selectedTagIds = this.selectedTagIds.includes(tagId)
      ? this.selectedTagIds.filter((id) => id !== tagId)
      : [...this.selectedTagIds, tagId];
    this.refresh();
  }

  setMode(mode: "all" | "any") {
    this.mode = mode;
    if (this.selectedTagIds.length > 0) this.refresh();
  }

  clearFilters() {
    this.query = "";
    this.selectedTagIds = [];
    this.refresh();
  }

  async toggleGrouping() {
    this.groupByArtist = !this.groupByArtist;
    await this.refresh();
  }

  async refresh() {
    const request = ++this.#latestRequest;
    this.loading = true;

    try {
      if (this.groupByArtist) {
        const groups = await invoke<ArtistGroup[]>("group_tracks_by_artist");
        if (request !== this.#latestRequest) return;
        this.groups = groups;
      } else {
        const results = await invoke<Track[]>("query_library", {
          search: this.query.trim() === "" ? null : this.query,
          tagIds: this.selectedTagIds,
          mode: this.mode,
        });
        if (request !== this.#latestRequest) return;
        this.results = results;
      }
    } catch (e) {
      if (request !== this.#latestRequest) return;
      toast.error(String(e));
    } finally {
      if (request === this.#latestRequest) this.loading = false;
    }
  }
}

export const libraryView = new LibraryViewStore();
