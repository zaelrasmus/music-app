import { invoke } from "@tauri-apps/api/core";
import { toast } from "svelte-sonner";
import { player } from "$lib/player.svelte";
import { trackStore } from "$lib/tracks.svelte";
import { libraryView } from "$lib/library-view.svelte";
import { playlistStore } from "$lib/playlists.svelte";
import { SvelteSet } from "svelte/reactivity";

export type Provider = "youtube" | "soundcloud";

/**
 * A tab in the search picker.
 *
 * A superset of `Provider`: "ytmusic" is not a service and is never stored,
 * only a different way of asking YouTube. See `ProviderSearch.source`.
 */
export type SearchSource = Provider | "ytmusic";

/** What a search is looking for. */
export type SearchKind = "track" | "playlist" | "artist";

/** One entry in the provider picker, as the backend lists them. */
export type ProviderInfo = {
  id: Provider;
  name: string;
  /**
   * What this provider can be searched for.
   *
   * Sent by the backend rather than decided here, so the tabs on screen and
   * the searches that can actually run cannot drift apart.
   */
  kinds: SearchKind[];
};

/** A playlist or an artist, as a row in a result list. */
export type Collection = {
  provider: Provider;
  kind: SearchKind;
  url: string;
  title: string;
  uploader: string | null;
  itemCount: number | null;
  /** Followers or subscribers, for an artist. Never shown as zero. */
  followerCount: number | null;
  thumbnailUrl: string | null;
};

/** A collection the user is looking inside, with what it holds. */
export type OpenedCollection = {
  collection: Collection;
  tracks: SearchResult[];
};

export type SearchResult = {
  provider: Provider;
  remoteId: string;
  /**
   * The provider's page. Stored, not derived — SoundCloud URLs embed the
   * uploader's handle and cannot be rebuilt from the numeric id.
   */
  remoteUrl: string;
  title: string;
  channel: string | null;
  durationSecs: number | null;
  viewCount: number | null;
  thumbnailUrl: string | null;
  /**
   * The uploader's own page, when the provider gives one.
   *
   * What makes "go to the artist" work from any track on both providers —
   * including SoundCloud, where artists cannot be searched for at all.
   */
  channelUrl: string | null;
  isLive: boolean;
};

/**
 * Long enough that typing a few more characters cancels the previous attempt,
 * short enough not to feel laggy. yt-dlp itself takes ~4s, so this is about
 * not stacking searches rather than saving milliseconds.
 */
const DEBOUNCE_MS = 450;

/**
 * SoundCloud hands back a 30-second snippet for Go+ gated uploads, and reports
 * the *snippet's* length as the duration.
 *
 * There is a definitive signal — the resolved format id ends in `_preview` —
 * but reading it costs a full per-track lookup, seconds each, which would make
 * a 15-result search unusable. So this infers from the duration instead, and
 * the badge says "likely" rather than asserting: a genuinely 30-second upload
 * would look identical here.
 */
export function looksLikePreview(result: SearchResult) {
  return result.provider === "soundcloud" && result.durationSecs === 30;
}

class ProviderSearchStore {
  /** Filled from the backend so the enum stays the single source of truth. */
  providers = $state<ProviderInfo[]>([]);
  /**
   * Which search is running, which is not the same as which service.
   *
   * "YT Music" and "YouTube" both reach YouTube and both store `youtube`
   * tracks — the schema allows no third value, and everything found either way
   * streams down the same path. They differ in what they *know*: the music
   * catalogue returns a title, an artist and a duration as separate fields,
   * while a plain search returns an upload title and whoever posted it. That
   * is the difference between a track filed under "Set It Off" and one filed
   * under "Music Terminal".
   *
   * Neither is the better search. The catalogue is tidier and misses things —
   * it will not find the Queen recording of Bohemian Rhapsody, and it happily
   * returns a re-upload credited to "Freddy Mercury". A plain search is
   * messier and reaches everything, including versions that were never
   * released. So this is a choice offered, not a default with a fallback.
   */
  source = $state<SearchSource>("ytmusic");

  /** Which service the current source actually talks to. */
  readonly provider = $derived<Provider>(
    this.source === "ytmusic" ? "youtube" : this.source,
  );

  query = $state("");
  results = $state<SearchResult[]>([]);
  searching = $state(false);
  error = $state<string | null>(null);
  /** True once a search has actually run, so "no results" isn't shown up front. */
  searched = $state(false);

  /**
   * Searches take seconds and finish out of order. Every request carries a
   * number and only the newest is allowed to write results, so a slow earlier
   * query cannot overwrite a fast later one.
   */
  #latestRequest = 0;
  #debounce: ReturnType<typeof setTimeout> | undefined;

  async loadProviders() {
    try {
      this.providers = await invoke<ProviderInfo[]>("list_providers");
    } catch (e) {
      toast.error(String(e));
    }
  }

  /**
   * The tabs, with the music catalogue offered beside YouTube itself.
   *
   * Built from the backend's provider list rather than hardcoded, so a
   * provider that disappears takes its tab with it — and inserted rather than
   * appended, because "YT Music" and "YouTube" are two ways of searching the
   * same service and belong next to each other.
   */
  readonly sources = $derived.by(() => {
    const tabs: { id: SearchSource; name: string; kinds: SearchKind[] }[] = [];
    for (const provider of this.providers) {
      if (provider.id === "youtube") {
        // Tracks only. The catalogue has albums and artists behind different
        // filters, but nothing here reads them yet, and offering a tab that
        // cannot answer is worse than not offering it.
        tabs.push({ id: "ytmusic", name: "YT Music", kinds: ["track"] });
      }
      tabs.push(provider);
    }
    return tabs;
  });

  get providerName() {
    return this.sources.find((s) => s.id === this.source)?.name ?? "YouTube";
  }

  /**
   * Switches service and re-runs the current query.
   *
   * Results are cleared first: they belong to the old provider, and leaving
   * them on screen under the new one's label for the several seconds a search
   * takes would be actively misleading.
   */
  async setProvider(source: SearchSource) {
    if (source === this.source) return;

    this.source = source;
    this.results = [];
    this.collections = [];
    this.opened = null;
    this.searched = false;
    this.error = null;

    // A provider that cannot be searched the current way falls back to
    // tracks, rather than leaving a tab selected that it does not offer.
    if (!this.kinds.includes(this.kind)) this.kind = "track";

    if (this.query.trim() !== "") {
      clearTimeout(this.#debounce);
      await this.search(this.query);
    }
  }

  /** Called on every keystroke. */
  queueSearch(query: string) {
    this.query = query;
    clearTimeout(this.#debounce);

    if (query.trim() === "") {
      // Abandon anything in flight so late results cannot repopulate a box
      // the user has just cleared.
      this.#latestRequest += 1;
      this.results = [];
      this.collections = [];
      this.searching = false;
      this.searched = false;
      this.error = null;
      return;
    }

    this.#debounce = setTimeout(() => this.search(query), DEBOUNCE_MS);
  }

  /** The result currently being saved, so its row can show progress. */
  saving = $state<string | null>(null);

  /**
   * Which results are in the library, keyed by `provider:remoteId`.
   *
   * Asked of the database after every search rather than remembered from this
   * session's own clicks. Remembering only the clicks was the bug: a track
   * added last week came back from a search looking exactly like one never
   * seen before, so the obvious move was to add it again — which changes
   * nothing, gives no feedback, and reads as the add having failed.
   *
   * Keyed by provider as well as id because `remoteId` is not an identity.
   * SoundCloud ids are plain integers and the database's own uniqueness is on
   * `(source, remote_id)`; a bare-id set would let one provider's result mark
   * another's.
   */
  filed = new SvelteSet<string>();

  /** The key `filed` is indexed by. */
  #key(provider: Provider, remoteId: string) {
    return `${provider}:${remoteId}`;
  }

  /** Whether the library already holds this result. */
  isFiled(result: SearchResult) {
    return this.filed.has(this.#key(result.provider, result.remoteId));
  }

  /**
   * Re-asks about whatever is currently on screen.
   *
   * For coming back to a search that was left open: the results are still the
   * ones the provider returned, but the library may have changed underneath
   * them in the meantime.
   */
  async refreshFiled() {
    await this.#markFiled([...this.results, ...(this.opened?.tracks ?? [])]);
  }

  /**
   * Asks the database which of these it already holds, and marks them.
   *
   * Best effort and silent. A failure here leaves rows offering to add
   * something already added, which is where this started — annoying, not
   * broken — and is not worth a toast over a list the user is reading.
   *
   * The answer replaces what was known about these exact ids, in both
   * directions -- so a track removed from the library elsewhere loses its
   * badge the next time it is searched for, rather than claiming forever that
   * it is still filed. Ids this was not asked about are left alone, which is
   * what keeps two searches in flight from undoing each other.
   */
  async #markFiled(results: SearchResult[]) {
    if (results.length === 0) return;

    // One call per provider. In practice a result list is all one provider,
    // but an opened artist page reached from a track is not guaranteed to be,
    // and the backend scopes the question by provider anyway.
    const byProvider = new Map<Provider, string[]>();
    for (const result of results) {
      const ids = byProvider.get(result.provider);
      if (ids) ids.push(result.remoteId);
      else byProvider.set(result.provider, [result.remoteId]);
    }

    await Promise.all(
      [...byProvider].map(async ([provider, remoteIds]) => {
        try {
          const found = new Set(
            await invoke<string[]>("filed_remote_ids", { provider, remoteIds }),
          );
          for (const id of remoteIds) {
            const key = this.#key(provider, id);
            if (found.has(id)) this.filed.add(key);
            else this.filed.delete(key);
          }
        } catch {
          // Deliberately silent -- see above.
        }
      }),
    );
  }

  /**
   * Saves a result as a `saved` track and plays it.
   *
   * Saving is idempotent per provider, so picking the same result twice reuses
   * the existing track rather than duplicating it — which also means play
   * counts and any edited title survive.
   */
  /**
   * Plays a track, and everything around it when there is a surrounding.
   *
   * Pressing play on the fourth song of a playlist means "play this playlist,
   * from here" — not "play this one song and then stop". The context is what
   * makes the rest follow, and what gives shuffle and repeat something to act
   * on; without it the queue ends after one track and the other forty-nine
   * might as well not have been opened.
   *
   * Nothing about this needs the playlist to be in the library. The tracks
   * become ordinary rows on the way to the queue, exactly as a single
   * audition does, and stay out of the library just the same.
   */
  async playFromHere(result: SearchResult) {
    const opened = this.opened;
    if (!opened) return this.playResult(result);

    const index = opened.tracks.findIndex(
      (track) => track.remoteUrl === result.remoteUrl,
    );
    if (index < 0) return this.playResult(result);

    this.saving = result.remoteId;
    try {
      await this.playAll(index);
    } finally {
      this.saving = null;
    }
  }

  async playResult(result: SearchResult) {
    this.saving = result.remoteId;

    try {
      const trackId = await invoke<number>("save_remote_track", { result });
      // Deliberately *not* added to the library. Playing something to find out
      // whether you like it is not a decision to keep it, and nine rejected
      // auditions in the library is the cost of assuming otherwise. History
      // remembers it either way, which is the way back if it turns out to be
      // the one.
      //
      // Resolving the stream takes seconds; the player reports `loading`
      // until audio actually starts.
      await player.playQueue([trackId], 0, `${this.providerName} search`);
    } catch (e) {
      toast.error(String(e));
    } finally {
      this.saving = null;
    }
  }

  /**
   * Saves a result as a track without playing it, returning its id.
   *
   * This is what makes "queue a search result" and "add one to a playlist"
   * work: the result becomes an ordinary track first, and from then on nothing
   * downstream can tell it apart from a local one.
   */
  async saveResult(result: SearchResult): Promise<number | null> {
    this.saving = result.remoteId;
    try {
      const trackId = await invoke<number>("save_remote_track", { result });
      await trackStore.load();
      return trackId;
    } catch (e) {
      toast.error(String(e));
      return null;
    } finally {
      this.saving = null;
    }
  }

  /**
   * Saves a result and files it in the library.
   *
   * The explicit gesture that nothing else performs: every other path here
   * creates the row without claiming the user wants to keep it.
   */
  async addToLibrary(result: SearchResult): Promise<boolean> {
    this.saving = result.remoteId;
    try {
      const trackId = await invoke<number>("save_remote_track", { result });
      await invoke("set_in_library", { trackId, inLibrary: true });
      this.filed.add(this.#key(result.provider, result.remoteId));
      await Promise.all([trackStore.load(), libraryView.refresh()]);
      return true;
    } catch (e) {
      toast.error(String(e));
      return false;
    } finally {
      this.saving = null;
    }
  }

  /** Runs immediately — for the Enter key. */
  async searchNow() {
    clearTimeout(this.#debounce);
    if (this.query.trim() === "") return;
    await this.search(this.query);
  }

  async search(query: string) {
    const request = ++this.#latestRequest;
    this.searching = true;
    this.error = null;

    try {
      if (this.kind === "track") {
        // The music catalogue has its own command, which falls back to yt-dlp
        // by itself if the endpoint is unavailable — so a failure here is a
        // real failure rather than the private API having moved again.
        const results =
          this.source === "ytmusic"
            ? await invoke<SearchResult[]>("search_yt_music", {
                query,
                limit: 25,
              })
            : await invoke<SearchResult[]>("search_provider", {
                provider: this.provider,
                query,
                limit: 25,
              });

        if (request !== this.#latestRequest) return;
        this.results = results;

        // Not awaited: the rows are already on screen and readable, and the
        // badge is an annotation on them rather than something to wait for.
        void this.#markFiled(results);
      } else {
        const collections = await invoke<Collection[]>("search_collections", {
          provider: this.provider,
          kind: this.kind,
          query,
        });

        if (request !== this.#latestRequest) return;
        this.collections = collections;
      }

      this.searched = true;
    } catch (e) {
      if (request !== this.#latestRequest) return;
      this.error = String(e);
      this.results = [];
      this.collections = [];
    } finally {
      // Only the newest request owns the spinner, or a superseded one would
      // switch it off while a newer search is still running.
      if (request === this.#latestRequest) this.searching = false;
    }
  }

  // --- playlists and artists ------------------------------------------

  /**
   * What the search is looking for.
   *
   * Not every provider can answer every question, and which ones it can comes
   * from the backend rather than from a list kept here — see
   * `Provider::searchable_kinds`. SoundCloud has no playlist or artist search
   * at all, so those tabs simply do not appear for it.
   */
  kind = $state<SearchKind>("track");
  collections = $state<Collection[]>([]);

  /** The playlist or artist being looked inside, if any. */
  opened = $state<OpenedCollection | null>(null);
  expanding = $state(false);
  importing = $state(false);

  get kinds(): SearchKind[] {
    return this.sources.find((s) => s.id === this.source)?.kinds ?? ["track"];
  }

  async setKind(kind: SearchKind) {
    if (kind === this.kind) return;

    this.kind = kind;
    // Results of the old kind are meaningless under the new tab, and a
    // search takes seconds — leaving them up would be a lie for that whole
    // time.
    this.results = [];
    this.collections = [];
    this.searched = false;
    this.error = null;
    this.opened = null;

    if (this.query.trim() !== "") {
      clearTimeout(this.#debounce);
      await this.search(this.query);
    }
  }

  /**
   * Looks inside a playlist or artist.
   *
   * The search results behind it are kept exactly as they are: going back has
   * to be instant, and re-running the search to rebuild a list the user was
   * just looking at would cost seconds to arrive at the same thing.
   */
  async openCollection(collection: Collection) {
    this.expanding = true;
    this.error = null;
    this.opened = { collection, tracks: [] };

    try {
      const expansion = await invoke<OpenedCollection>("expand_collection", {
        provider: collection.provider,
        kind: collection.kind,
        url: collection.url,
      });

      // Still the one being opened? A second click while the first was in
      // flight would otherwise fill this one with the other's tracks.
      if (this.opened?.collection.url !== collection.url) return;

      // The page's own description of itself replaces the row that led here.
      // That row is whatever a search happened to say — and for an artist
      // reached from a track it is a bare name with no picture at all.
      //
      // Field by field rather than wholesale: a provider that omits something
      // should leave what the search already knew, not blank it. A search
      // result carrying artwork the page does not is common, and losing it
      // would be a visible regression from the row the user just clicked.
      this.opened = {
        collection: {
          ...expansion.collection,
          title: expansion.collection.title || collection.title,
          uploader: expansion.collection.uploader ?? collection.uploader,
          itemCount: expansion.collection.itemCount ?? collection.itemCount,
          followerCount:
            expansion.collection.followerCount ?? collection.followerCount,
          thumbnailUrl:
            expansion.collection.thumbnailUrl ?? collection.thumbnailUrl,
        },
        tracks: expansion.tracks,
      };

      void this.#markFiled(expansion.tracks);
    } catch (e) {
      if (this.opened?.collection.url !== collection.url) return;
      this.error = String(e);
      this.opened = null;
    } finally {
      this.expanding = false;
    }
  }

  /** Opens the artist behind a track, without needing artist search. */
  async openArtistOf(result: SearchResult) {
    if (!result.channelUrl) return;

    await this.openCollection({
      provider: result.provider,
      kind: "artist",
      url: result.channelUrl,
      title: result.channel ?? "Artist",
      uploader: null,
      itemCount: null,
      followerCount: null,
      // All unknown from a track row — the artist page fills them in from
      // the page itself the moment it opens.
      thumbnailUrl: null,
    });
  }

  back() {
    this.opened = null;
    this.error = null;
  }

  /**
   * Plays everything in the open collection, in order.
   *
   * One bulk save, then one queue — the tracks become ordinary rows first, and
   * from that point the player cannot tell this from a local playlist. Shuffle
   * and repeat therefore work on it without knowing it is remote, because they
   * act on the context and the context is just track ids.
   */
  async playAll(startIndex = 0) {
    const opened = this.opened;
    if (!opened || opened.tracks.length === 0) return;

    this.importing = true;
    try {
      const ids = await invoke<number[]>("save_remote_tracks", {
        results: opened.tracks,
      });
      await player.playQueue(ids, startIndex, opened.collection.title);
    } catch (e) {
      toast.error(String(e));
    } finally {
      this.importing = false;
    }
  }

  /**
   * Plays the collection shuffled.
   *
   * Shuffle is turned on *before* the queue exists, because the coordinator
   * re-derives the permutation over whatever a new context contains and keeps
   * the starting track at the front. Doing it afterwards would leave the first
   * track being the list's head, which is not what the button says.
   *
   * The start is picked at random for the same reason: "shuffle" that always
   * opens with track one is not shuffled in the only place the user can see.
   */
  async shuffleAll() {
    const opened = this.opened;
    if (!opened || opened.tracks.length === 0) return;

    if (!player.shuffle) await player.toggleShuffle();
    await this.playAll(Math.floor(Math.random() * opened.tracks.length));
  }

  /**
   * Copies the open collection into a playlist of the user's own.
   *
   * The tracks are saved but deliberately not filed in the library: importing
   * a fifty-track album says "keep this list", not "put fifty tracks in my
   * library". Each one can still be added individually from inside it.
   */
  async importCollection(): Promise<number | null> {
    const opened = this.opened;
    if (!opened || opened.tracks.length === 0) return null;

    this.importing = true;
    try {
      const trackIds = await invoke<number[]>("save_remote_tracks", {
        results: opened.tracks,
      });

      const playlist = await invoke<{ id: number; name: string }>(
        "import_playlist",
        {
          name: opened.collection.title,
          source: opened.collection.provider,
          sourceUrl: opened.collection.url,
          trackIds,
        },
      );

      await playlistStore.load();
      toast.success(
        `Saved “${playlist.name}” — ${trackIds.length} tracks, in your playlists.`,
      );
      return playlist.id;
    } catch (e) {
      toast.error(String(e));
      return null;
    } finally {
      this.importing = false;
    }
  }
}

export const providerSearch = new ProviderSearchStore();
