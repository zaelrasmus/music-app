import { getCurrentWindow } from "@tauri-apps/api/window";

/**
 * The window frame, which this app draws itself.
 *
 * `decorations: false` buys a titlebar that matches the app instead of one
 * that matches the OS, at the price of having to reimplement minimise,
 * maximise and close -- and, more easily forgotten, of having to *know* when
 * the window is maximised. A maximised window sits flush against the screen
 * edges, so its rounded corners have to go; without tracking the state the
 * corners would stay rounded and leak the desktop through four small gaps.
 *
 * The state is tracked by subscribing to the window's own resize event rather
 * than by assuming our own button worked: the user can also maximise by
 * double-clicking the titlebar, dragging to the top edge, or pressing
 * Win+Up, and none of those pass through here.
 */
class ChromeStore {
  maximized = $state(false);
  /** False until the window has been told to show itself. */
  ready = $state(false);

  #window = getCurrentWindow();

  /**
   * Reveals the window and starts tracking its state.
   *
   * The window is configured `visible: false` so that the first frame the user
   * sees is already themed and laid out -- otherwise an undecorated window
   * flashes as an unstyled white rectangle while the bundle boots.
   */
  async start() {
    try {
      this.maximized = await this.#window.isMaximized();
    } catch (e) {
      console.debug("could not read window state", e);
    }

    let unlisten: (() => void) | undefined;
    try {
      unlisten = await this.#window.onResized(async () => {
        try {
          this.maximized = await this.#window.isMaximized();
        } catch {
          // A resize during teardown; the answer no longer matters.
        }
      });
    } catch (e) {
      console.debug("could not watch window size", e);
    }

    await this.show();

    return () => unlisten?.();
  }

  /**
   * Best-effort, and separated out so it can never be skipped by an earlier
   * failure: a window that stays hidden because a listener would not attach is
   * an app that looks like it did not start.
   */
  async show() {
    try {
      await this.#window.show();
      await this.#window.setFocus();
    } catch (e) {
      console.debug("could not show the window", e);
    }
    this.ready = true;
  }

  async minimize() {
    await this.#window.minimize();
  }

  async toggleMaximize() {
    await this.#window.toggleMaximize();
    this.maximized = await this.#window.isMaximized();
  }

  async close() {
    await this.#window.close();
  }
}

export const chrome = new ChromeStore();
