import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Fired when a saved remote track's artwork has finished downloading. */
export type CoverReady = { trackId: number; coverKey: string };

/**
 * Resolves a cover key to something an `<img>` can load.
 *
 * The directory is fetched once and every URL is built from it here, rather
 * than asking the backend per image. A list of five hundred rows would
 * otherwise be five hundred IPC round trips, each returning a base64 copy of a
 * file the webview could have read directly.
 *
 * `convertFileSrc` produces an `asset:` URL, which only resolves for paths
 * inside the scope declared in `tauri.conf.json` -- the covers directory and
 * nothing else. That scope is the security boundary; this file is only
 * convenience on top of it.
 */
class CoverStore {
  /** Absolute path to the covers directory. Empty until `load` resolves. */
  #dir = $state("");

  async load() {
    if (this.#dir !== "") return;
    try {
      this.#dir = await invoke<string>("cover_dir");
    } catch (e) {
      // Not fatal: every caller falls back to generated artwork, so the app
      // simply looks the way it did before covers existed.
      console.debug("could not resolve the cover directory", e);
    }
  }

  /**
   * The URL for a key, or null when there is nothing to show.
   *
   * Null is the normal case for a track with no embedded art, so callers treat
   * it as "draw the generated cover" rather than as an error.
   */
  url(key: string | null | undefined): string | null {
    if (!key || this.#dir === "") return null;
    // A forward slash even on Windows: the OS accepts it, and it keeps this
    // from being the one line that would have to change elsewhere.
    return convertFileSrc(`${this.#dir}/${key}`);
  }

  /**
   * Notifies when a track's artwork lands.
   *
   * Saving a remote track returns before its thumbnail has been fetched --
   * deliberately, because saving is on the path to *playing* and a picture is
   * not worth waiting for. This is how the row catches up afterwards.
   *
   * The caller decides what to refresh rather than this store reaching into
   * four list stores it has no other reason to know about.
   */
  async listenForCovers(onReady: (event: CoverReady) => void) {
    return listen<CoverReady>("cover-ready", (e) => onReady(e.payload));
  }
}

export const covers = new CoverStore();
