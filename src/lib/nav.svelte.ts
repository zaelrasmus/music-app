import { selection } from "$lib/selection.svelte";

/**
 * Which view is on screen.
 *
 * Deliberately a store rather than SvelteKit routes. Every view here owns
 * expensive, long-lived state -- a page of search results, an open playlist, a
 * scroll position -- and routing would either throw that away on each
 * navigation or force every store to re-fetch to rebuild it. Nothing in this
 * app is linkable or back-buttonable, so routing would buy nothing in return.
 */
export type View =
  | "library"
  | "search"
  | "playlists"
  | "history"
  | "settings"
  /** Reviewing what the app can work out about untagged tracks. Reached from
   *  settings rather than the sidebar: it is a job, not a place. */
  | "details";

class NavStore {
  view = $state<View>("library");

  go(view: View) {
    // A selection belongs to the list it was made in. Carrying it across
    // would leave a bulk action pointing at rows nobody can see.
    selection.clear();

    this.view = view;
  }
}

export const nav = new NavStore();
