import { invoke } from "@tauri-apps/api/core";
import { selection } from "$lib/selection.svelte";
import { toast } from "svelte-sonner";
import type { Track } from "$lib/tracks.svelte";
import type { Direction, Sort } from "$lib/sorting";
import { readSetting, writeSetting } from "$lib/settings.svelte";

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

  sort = $state<Sort>("auto");
  direction = $state<Direction>("asc");

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

  /**
   * Restores the saved order.
   *
   * A device preference, like volume -- it says how you like to look at your
   * library, not anything about the library itself, so it lives in settings
   * rather than the database.
   */
  async restore() {
    const [sort, direction] = await Promise.all([
      readSetting<Sort>("librarySort", "auto"),
      readSetting<Direction>("librarySortDirection", "asc"),
    ]);
    this.sort = sort;
    this.direction = direction;
    await this.refresh();
  }

  setSort(sort: Sort, direction: Direction) {
    this.sort = sort;
    this.direction = direction;
    void writeSetting("librarySort", sort);
    void writeSetting("librarySortDirection", direction);
    void this.refresh();
  }

  /** Flips the current field without leaving it. */
  toggleDirection() {
    this.setSort(this.sort, this.direction === "asc" ? "desc" : "asc");
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
          sort: this.sort,
          direction: this.direction,
        });
        if (request !== this.#latestRequest) return;
        this.results = results;

        // Rows that survived the reload keep their selection; the rest cannot
        // be acted on any more, and "12 selected" must never mean twelve rows
        // nobody can see.
        selection.retain(results.map((track) => track.id));
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
