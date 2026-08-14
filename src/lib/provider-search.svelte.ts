import { invoke } from "@tauri-apps/api/core";
import { toast } from "svelte-sonner";
import { player } from "$lib/player.svelte";
import { trackStore } from "$lib/tracks.svelte";

export type Provider = "youtube" | "soundcloud";

/** One entry in the provider picker, as the backend lists them. */
export type ProviderInfo = {
  id: Provider;
  name: string;
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
  provider = $state<Provider>("youtube");

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

  get providerName() {
    return (
      this.providers.find((p) => p.id === this.provider)?.name ?? "YouTube"
    );
  }

  /**
   * Switches service and re-runs the current query.
   *
   * Results are cleared first: they belong to the old provider, and leaving
   * them on screen under the new one's label for the several seconds a search
   * takes would be actively misleading.
   */
  async setProvider(provider: Provider) {
    if (provider === this.provider) return;

    this.provider = provider;
    this.results = [];
    this.searched = false;
    this.error = null;

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
   * Saves a result as a `saved` track and plays it.
   *
   * Saving is idempotent per provider, so picking the same result twice reuses
   * the existing track rather than duplicating it — which also means play
   * counts and any edited title survive.
   */
  async playResult(result: SearchResult) {
    this.saving = result.remoteId;

    try {
      const trackId = await invoke<number>("save_remote_track", { result });
      // It is a library track now, so the list below should show it.
      await trackStore.load();
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
      const results = await invoke<SearchResult[]>("search_provider", {
        provider: this.provider,
        query,
        limit: 15,
      });

      if (request !== this.#latestRequest) return;
      this.results = results;
      this.searched = true;
    } catch (e) {
      if (request !== this.#latestRequest) return;
      this.error = String(e);
      this.results = [];
    } finally {
      // Only the newest request owns the spinner, or a superseded one would
      // switch it off while a newer search is still running.
      if (request === this.#latestRequest) this.searching = false;
    }
  }
}

export const providerSearch = new ProviderSearchStore();
