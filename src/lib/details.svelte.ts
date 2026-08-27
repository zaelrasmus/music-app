import { invoke } from "@tauri-apps/api/core";
import { toast } from "svelte-sonner";
import { trackStore } from "$lib/tracks.svelte";
import { libraryView } from "$lib/library-view.svelte";
import { lyricsStore } from "$lib/lyrics.svelte";

export type Proposal = { artist: string; title: string };

export type TrackProposal = {
  trackId: number;
  currentTitle: string;
  /** Read out of the title's own "Artist - Title" shape, when it has one. */
  fromTitle: Proposal | null;
  /** The same track read as though the folder named the artist. */
  fromFolder: Proposal;
};

export type FolderGroup = {
  path: string;
  name: string;
  total: number;
  fromTitles: number;
};

/** Where a row's proposed artist comes from, or that it is being left alone. */
export type Source = "title" | "folder" | "skip";

/**
 * Filling in the artists 1003 local tracks never had.
 *
 * The backend proposes and this decides nothing on its own: every row starts
 * on the reading the app can actually justify — its own title — and the folder
 * reading is never picked by default, because measured on this library a
 * folder names the artist only about half the time. `Creo` and `ElRichMC` are
 * artists; `Artcore` and `Math Rock` are genres, and 413 tracks sit in those
 * two alone.
 *
 * That one judgement is what a person is here to make, and it is one click per
 * folder rather than one per track.
 */
class DetailsStore {
  folders = $state<FolderGroup[]>([]);
  loading = $state(false);

  /** The folder being reviewed. `null` while the list is on screen. */
  open = $state<FolderGroup | null>(null);
  rows = $state<TrackProposal[]>([]);

  /** Per track, keyed by id. Rebuilt whenever a folder opens. */
  choices = $state<Record<number, Source>>({});

  /**
   * Whether to also file everything here under an album named for the folder.
   *
   * Off by default and separate from the artist question, because they have
   * different answers: `Celeste` is an album and not an artist, `Creo` is an
   * artist and not an album, and `Artcore` is neither.
   */
  useFolderAsAlbum = $state(false);

  saving = $state(false);

  /** How many rows would be written if applied now. */
  accepted = $derived(
    this.rows.filter((row) => this.resolve(row) !== null).length,
  );

  /** Rows whose title cannot answer for itself, so only the folder can. */
  needsFolder = $derived(this.rows.filter((row) => row.fromTitle === null).length);

  async load() {
    this.loading = true;
    try {
      this.folders = await invoke<FolderGroup[]>("untagged_folders");
    } catch (e) {
      toast.error(String(e));
      this.folders = [];
    } finally {
      this.loading = false;
    }
  }

  async openFolder(folder: FolderGroup) {
    this.open = folder;
    this.rows = [];
    this.useFolderAsAlbum = false;
    this.loading = true;
    try {
      const rows = await invoke<TrackProposal[]>("folder_proposals", {
        folder: folder.path,
      });
      // Someone can go back and open another folder faster than this returns.
      if (this.open?.path !== folder.path) return;
      this.rows = rows;
      // The title is the only reading the app can justify on its own, so it is
      // the only one that starts selected. A row it cannot answer for is left
      // alone rather than quietly assigned the folder's name.
      this.choices = Object.fromEntries(
        rows.map((row) => [row.trackId, row.fromTitle ? "title" : "skip"]),
      );
    } catch (e) {
      toast.error(String(e));
      if (this.open?.path === folder.path) this.close();
    } finally {
      this.loading = false;
    }
  }

  close() {
    this.open = null;
    this.rows = [];
    this.choices = {};
    this.useFolderAsAlbum = false;
  }

  choose(trackId: number, source: Source) {
    this.choices = { ...this.choices, [trackId]: source };
  }

  /**
   * Sets every row at once.
   *
   * "Use the folder name" is the gesture this whole view exists for: one
   * decision covering 56 tracks. Rows whose own title already answers keep
   * that answer when picking `title`, and rows with no title reading fall back
   * to skip rather than silently taking the folder's name.
   */
  chooseAll(source: Source) {
    this.choices = Object.fromEntries(
      this.rows.map((row) => [
        row.trackId,
        source === "title" && !row.fromTitle ? "skip" : source,
      ]),
    );
  }

  sourceFor(row: TrackProposal): Source {
    return this.choices[row.trackId] ?? "skip";
  }

  /** What would be written for a row, or null if it is being left alone. */
  resolve(row: TrackProposal): Proposal | null {
    const source = this.sourceFor(row);
    if (source === "folder") return row.fromFolder;
    if (source === "title") return row.fromTitle;
    return null;
  }

  async apply() {
    const folder = this.open;
    if (!folder || this.saving) return;

    const edits = this.rows.flatMap((row) => {
      const chosen = this.resolve(row);
      if (!chosen) return [];
      return [
        {
          trackId: row.trackId,
          title: chosen.title,
          artist: chosen.artist,
          album: this.useFolderAsAlbum ? folder.name : null,
        },
      ];
    });

    if (edits.length === 0) return;

    this.saving = true;
    try {
      const written = await invoke<number>("apply_track_details", { edits });
      toast.success(
        `Filled in ${written} ${written === 1 ? "track" : "tracks"} in ${folder.name}.`,
      );
      this.close();
      // The library lists, the artist grouping and the lyrics panel are all
      // downstream of what just changed.
      await Promise.all([
        this.load(),
        trackStore.load(),
        libraryView.refresh(),
        lyricsStore.reload(),
      ]);
    } catch (e) {
      toast.error(String(e));
    } finally {
      this.saving = false;
    }
  }
}

export const details = new DetailsStore();
