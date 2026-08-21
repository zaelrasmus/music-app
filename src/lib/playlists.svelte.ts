import { invoke } from "@tauri-apps/api/core";
import { toast } from "svelte-sonner";
import { player } from "$lib/player.svelte";
import { trackStore } from "$lib/tracks.svelte";
import { selection } from "$lib/selection.svelte";
import { readSetting, writeSetting } from "$lib/settings.svelte";
import type { Direction, Sort } from "$lib/sorting";

/**
 * How the playlist grid is ordered.
 *
 * A separate vocabulary from the track sort, because the questions differ: a
 * playlist has no artist, no duration and no upload date.
 */
export type PlaylistSort = "lastPlayed" | "mostPlayed" | "name" | "dateCreated";

export const PLAYLIST_GRID_SORTS: {
  id: PlaylistSort;
  label: string;
  asc: string;
  desc: string;
}[] = [
  {
    id: "lastPlayed",
    label: "Recently played",
    asc: "Longest ago first",
    desc: "Most recent first",
  },
  { id: "mostPlayed", label: "Most played", asc: "Least first", desc: "Most first" },
  {
    id: "dateCreated",
    label: "Date added",
    asc: "Oldest first",
    desc: "Newest first",
  },
  { id: "name", label: "Name", asc: "A – Z", desc: "Z – A" },
];
import type { Track } from "$lib/tracks.svelte";

/** One name that counts as a playlist's artist. */
export type ArtistRule = {
  artistKey: string;
  label: string;
  /**
   * The artist's own picture, found in the background after the rule was made.
   *
   * Null until it arrives, and null forever when the provider has nothing to
   * offer. Deliberately *not* a track thumbnail: that is one release's cover,
   * and standing it in for the artist puts an arbitrary song's artwork on a
   * collection of forty.
   */
  avatarUrl: string | null;
};

/** An artist present in the library, for the picker and the browse list. */
export type LibraryArtist = {
  artistKey: string;
  name: string;
  trackCount: number;
  source: string | null;
};

export type Playlist = {
  id: number;
  name: string;
  coverPath: string | null;
  createdAt: number;
  trackCount: number;
  coverKey?: string | null;
  /**
   * The artist names this playlist fills itself from.
   *
   * Empty for an ordinary playlist. Non-empty is what makes it an artist
   * collection, and the only thing the UI reads to decide that -- so the
   * circle it draws can never disagree with how the playlist behaves.
   */
  artistRules: ArtistRule[];
};

/** Whether a playlist fills itself from exactly one artist. */
export function isArtistPlaylist(playlist: Playlist) {
  return (playlist.artistRules?.length ?? 0) > 0;
}

/**
 * Whether to draw it as a circle.
 *
 * One artist only. Two rules means two faces, and a circle showing one of
 * them would be a claim about identity nobody made.
 */
export function drawsAsArtist(playlist: Playlist) {
  return (playlist.artistRules?.length ?? 0) === 1;
}

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

  /** Every artist in the library, for the rule picker and the browse list. */
  artists = $state<LibraryArtist[]>([]);

  /**
   * How the open playlist is ordered.
   *
   * `custom` is the playlist's own order and the only mode where dragging
   * means anything -- every other option is a view, and reordering is turned
   * off under one for the same reason it is turned off under a filter.
   */
  sort = $state<Sort>("custom");
  direction = $state<Direction>("asc");

  /** How the grid is ordered. Its own small vocabulary; see `PlaylistSort`. */
  gridSort = $state<PlaylistSort>("lastPlayed");
  /**
   * And which way round.
   *
   * Descending by default because every option here reads that way first:
   * most recently played, most played, newest. Name is the exception and
   * flips itself on selection.
   */
  gridDirection = $state<Direction>("desc");

  /** True while anything other than the playlist's own order is showing. */
  get sorted() {
    return this.sort !== "custom";
  }

  setSort(sort: Sort, direction: Direction) {
    this.sort = sort;
    this.direction = direction;
    void writeSetting("playlistSort", sort);
    void writeSetting("playlistSortDirection", direction);
    void this.refreshOpen();
  }

  async setGridSort(sort: PlaylistSort, direction?: Direction) {
    // Picking a new field resets the direction to the one that field reads
    // naturally: "Name ▾" meaning Z–A would be a strange thing to land on.
    const next = direction ?? (sort === "name" ? "asc" : "desc");

    this.gridSort = sort;
    this.gridDirection = next;
    await writeSetting("playlistGridSort", sort);
    await writeSetting("playlistGridDirection", next);
    await this.load();
  }

  async toggleGridDirection() {
    await this.setGridSort(
      this.gridSort,
      this.gridDirection === "asc" ? "desc" : "asc",
    );
  }

  /** Restores both persisted orders. */
  async restoreSorts() {
    this.sort = await readSetting<Sort>("playlistSort", "custom");
    this.direction = await readSetting<Direction>("playlistSortDirection", "asc");
    this.gridSort = await readSetting<PlaylistSort>(
      "playlistGridSort",
      "lastPlayed",
    );
    this.gridDirection = await readSetting<Direction>(
      "playlistGridDirection",
      "desc",
    );
  }

  /** Filter over playlist *names*. Separate from the in-playlist filter. */
  listQuery = $state("");

  /**
   * Which kinds of playlist the list shows.
   *
   * Derived from the rules rather than stored anywhere: a playlist is an
   * artist because it fills itself from one, so this can never need
   * maintaining and a new playlist classifies itself.
   */
  kind = $state<"all" | "artists" | "other">("all");

  get visiblePlaylists() {
    const needle = this.listQuery.trim().toLowerCase();
    return this.playlists.filter((playlist) => {
      if (this.kind === "artists" && !isArtistPlaylist(playlist)) return false;
      if (this.kind === "other" && isArtistPlaylist(playlist)) return false;
      if (!needle) return true;
      return (
        playlist.name.toLowerCase().includes(needle) ||
        playlist.artistRules?.some((rule) =>
          rule.label.toLowerCase().includes(needle),
        )
      );
    });
  }

  async loadArtists() {
    try {
      this.artists = await invoke<LibraryArtist[]>("list_library_artists");
    } catch (e) {
      toast.error(String(e));
    }
  }

  /**
   * Makes an artist one of the names a playlist fills itself from.
   *
   * The list is reloaded as well as the open playlist: a rule changes the
   * track count and the shape of the row behind this screen.
   */
  async addArtistRule(playlistId: number, label: string) {
    try {
      await invoke<Playlist>("add_playlist_artist_rule", { playlistId, label });
      await this.refreshOpen();
      await this.load();
    } catch (e) {
      toast.error(String(e));
    }
  }

  /**
   * Files everything in a playlist in the library.
   *
   * The gesture an imported playlist needs: importing deliberately leaves its
   * tracks unclaimed, which keeps them out of the library *and* out of every
   * artist rule, since a rule only collects what you kept.
   */
  async addAllToLibrary(playlistId: number) {
    try {
      const filed = await invoke<number>("add_playlist_to_library", {
        playlistId,
      });

      if (filed === 0) {
        toast.info("Everything here is already in your library.");
        return;
      }

      toast.success(
        `Added ${filed} ${filed === 1 ? "track" : "tracks"} to your library.`,
      );

      // The playlist rows now say "in library", the library list has grown,
      // and any artist rule naming these artists has just gained tracks.
      await this.refreshOpen();
      await this.load();
      await trackStore.load();
    } catch (e) {
      toast.error(String(e));
    }
  }

  async removeArtistRule(playlistId: number, artistKey: string) {
    try {
      await invoke<Playlist>("remove_playlist_artist_rule", {
        playlistId,
        artistKey,
      });
      await this.refreshOpen();
      await this.load();
    } catch (e) {
      toast.error(String(e));
    }
  }

  /**
   * Creates a playlist that fills itself from this artist.
   *
   * The path from "I like this artist" to "here are their songs" in one
   * gesture -- which is the only reason the artist browse list exists.
   */
  async createFromArtist(artist: LibraryArtist) {
    try {
      const created = await invoke<Playlist>("create_playlist", {
        name: artist.name,
      });
      await invoke<Playlist>("add_playlist_artist_rule", {
        playlistId: created.id,
        label: artist.name,
      });
      await this.load();
      await this.openPlaylist(created.id);
    } catch (e) {
      toast.error(String(e));
    }
  }

  async load() {
    try {
      this.playlists = await invoke<Playlist[]>("list_playlists", {
        sort: this.gridSort,
        direction: this.gridDirection,
      });
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
        sort: this.sort,
        direction: this.direction,
      });
    } catch (e) {
      toast.error(String(e));
      this.open = null;
    } finally {
      this.loading = false;
      // Same rule as the library: a selection may not outlive its rows.
      selection.retain(this.open?.tracks.map((track) => track.id) ?? []);
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

    const playlistId = this.open.playlist.id;
    await player.playQueue(
      this.open.tracks.map((t) => t.id),
      startIndex,
      this.open.playlist.name,
    );

    // Counted here rather than inferred from track history: playing a song
    // from the library that happens to sit in some playlist must not push that
    // playlist to the top of "recently played". Putting the list on is the
    // event, and this is where it happens.
    //
    // Not awaited before playback, and failure is silent: an ordering hint is
    // not worth delaying a play or interrupting one.
    try {
      await invoke("mark_playlist_played", { playlistId });
      await this.load();
    } catch {
      // The grid keeps the order it had.
    }
  }
}

export const playlistStore = new PlaylistStore();
