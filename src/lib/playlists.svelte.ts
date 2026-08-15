import { invoke } from "@tauri-apps/api/core";
import { toast } from "svelte-sonner";
import { player } from "$lib/player.svelte";
import type { Track } from "$lib/tracks.svelte";

export type Playlist = {
  id: number;
  name: string;
  coverPath: string | null;
  createdAt: number;
  trackCount: number;
  coverKey?: string | null;
};

export type PlaylistDetail = {
  playlist: Playlist;
  tracks: Track[];
};

type AddOutcome = { added: number; skipped: number };

/**
 * Playlists and the currently opened one.
 *
 * Playing a playlist deliberately goes through the ordinary queue: the ids in
 * display order become the queue, and each resolves through the same seam as
 * anything else. There is no playlist-specific playback path.
 */
class PlaylistStore {
  playlists = $state<Playlist[]>([]);
  /** The opened playlist, or null when showing the list. */
  open = $state<PlaylistDetail | null>(null);
  loading = $state(false);

  async load() {
    try {
      this.playlists = await invoke<Playlist[]>("list_playlists");
    } catch (e) {
      toast.error(String(e));
    }
  }

  /**
   * Filter applied *within* the opened playlist.
   *
   * Deliberately separate from the library's filter: narrowing a playlist and
   * narrowing the library are different questions, and having one clobber the
   * other when you navigate between them would be surprising.
   */
  query = $state("");
  selectedTagIds = $state<number[]>([]);
  mode = $state<"all" | "any">("all");

  #debounce: ReturnType<typeof setTimeout> | undefined;

  get filtering() {
    return this.query.trim() !== "" || this.selectedTagIds.length > 0;
  }

  async openPlaylist(playlistId: number) {
    this.loading = true;
    try {
      this.open = await invoke<PlaylistDetail>("get_playlist", {
        playlistId,
        search: this.query.trim() === "" ? null : this.query,
        tagIds: this.selectedTagIds,
        mode: this.mode,
      });
    } catch (e) {
      toast.error(String(e));
      this.open = null;
    } finally {
      this.loading = false;
    }
  }

  setQuery(query: string) {
    this.query = query;
    clearTimeout(this.#debounce);
    this.#debounce = setTimeout(() => this.reloadOpen(), 200);
  }

  toggleTag(tagId: number) {
    this.selectedTagIds = this.selectedTagIds.includes(tagId)
      ? this.selectedTagIds.filter((id) => id !== tagId)
      : [...this.selectedTagIds, tagId];
    this.reloadOpen();
  }

  setMode(mode: "all" | "any") {
    this.mode = mode;
    if (this.selectedTagIds.length > 1) this.reloadOpen();
  }

  clearFilters() {
    this.query = "";
    this.selectedTagIds = [];
    this.reloadOpen();
  }

  close() {
    this.open = null;
    // A filter left over from the last playlist would silently hide tracks in
    // the next one.
    this.query = "";
    this.selectedTagIds = [];
  }

  /** Re-runs the opened playlist's query only. Used while filtering. */
  private async reloadOpen() {
    if (this.open) await this.openPlaylist(this.open.playlist.id);
  }

  /**
   * Re-runs the opened playlist *and* the list.
   *
   * For mutations only: adding or removing a track changes the counts shown in
   * the list, which a filter keystroke does not.
   */
  private async refreshOpen() {
    await this.reloadOpen();
    await this.load();
  }

  async create(name: string) {
    try {
      const playlist = await invoke<Playlist>("create_playlist", { name });
      await this.load();
      return playlist;
    } catch (e) {
      toast.error(String(e));
      return null;
    }
  }

  async rename(playlistId: number, name: string) {
    try {
      await invoke("rename_playlist", { playlistId, name });
      await this.refreshOpen();
      return true;
    } catch (e) {
      toast.error(String(e));
      return false;
    }
  }

  /**
   * Gives the playlist artwork from a file on disk.
   *
   * The backend copies the image into the cover store and normalises it, so
   * nothing here depends on where the file was or how large it is.
   */
  async setCover(playlistId: number, path: string) {
    try {
      await invoke("set_playlist_cover", { playlistId, path });
      await this.refreshOpen();
    } catch (e) {
      toast.error(String(e));
    }
  }

  /** Back to artwork generated from the playlist's name. */
  async clearCover(playlistId: number) {
    try {
      await invoke("clear_playlist_cover", { playlistId });
      await this.refreshOpen();
    } catch (e) {
      toast.error(String(e));
    }
  }

  async remove(playlistId: number) {
    try {
      await invoke("delete_playlist", { playlistId });
      if (this.open?.playlist.id === playlistId) this.open = null;
      await this.load();
    } catch (e) {
      toast.error(String(e));
    }
  }

  /**
   * Adds tracks, reporting what actually happened.
   *
   * Adding is idempotent, so "already there" is a normal outcome rather than a
   * failure — saying nothing would look like the click did not register.
   */
  async addTracks(playlistId: number, trackIds: number[]) {
    try {
      const outcome = await invoke<AddOutcome>("add_tracks_to_playlist", {
        playlistId,
        trackIds,
      });

      const name =
        this.playlists.find((p) => p.id === playlistId)?.name ?? "playlist";

      if (outcome.added === 0) {
        toast.info(`Already in ${name}.`);
      } else {
        const suffix = outcome.skipped > 0 ? `, ${outcome.skipped} already there` : "";
        toast.success(`Added ${outcome.added} to ${name}${suffix}.`);
      }

      await this.refreshOpen();
    } catch (e) {
      toast.error(String(e));
    }
  }

  async removeTrack(playlistId: number, trackId: number) {
    try {
      await invoke("remove_track_from_playlist", { playlistId, trackId });
      await this.refreshOpen();
    } catch (e) {
      toast.error(String(e));
    }
  }

  /** Moves a track to a new index. Positions are dense, so index == position. */
  async reorder(playlistId: number, trackId: number, newPosition: number) {
    try {
      await invoke("reorder_playlist_track", {
        playlistId,
        trackId,
        newPosition,
      });
      await this.refreshOpen();
    } catch (e) {
      toast.error(String(e));
    }
  }

  /** Plays the opened playlist, starting at `startIndex`. */
  async play(startIndex = 0) {
    if (!this.open || this.open.tracks.length === 0) return;
    await player.playQueue(
      this.open.tracks.map((t) => t.id),
      startIndex,
      this.open.playlist.name,
    );
  }
}

export const playlistStore = new PlaylistStore();
