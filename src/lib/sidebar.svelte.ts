import { readSetting, writeSetting } from "$lib/settings.svelte";

/**
 * `expanded` — icons and labels, and the width is the user's to set.
 * `icons`    — a narrow rail; labels appear on hover.
 * `hidden`   — gone entirely, for when the track list is the only thing wanted.
 */
export type SidebarMode = "expanded" | "icons" | "hidden";

const ORDER: SidebarMode[] = ["expanded", "icons", "hidden"];

/** The rail is sized by its icons, so this is a constant, not a preference. */
export const ICON_WIDTH = 60;

/**
 * Narrower than this and labels start truncating mid-word, which looks broken
 * rather than compact -- the rail already exists for people who want compact.
 */
export const MIN_WIDTH = 176;
export const MAX_WIDTH = 380;
export const DEFAULT_WIDTH = 236;

/**
 * How far below the minimum a drag has to go before it means "rail".
 *
 * Dragging past the minimum otherwise just stops dead, which feels like the
 * app has jammed. Collapsing instead treats the overshoot as intent -- but
 * only once it is unambiguous, or a slightly overshot resize would collapse a
 * sidebar the user was only trying to make small.
 */
const COLLAPSE_AT = MIN_WIDTH - 44;

class SidebarStore {
  mode = $state<SidebarMode>("expanded");
  width = $state(DEFAULT_WIDTH);
  /** True while the edge is being dragged, so transitions can be suppressed. */
  resizing = $state(false);

  /** The width the layout should actually use, whatever the mode. */
  get effectiveWidth() {
    if (this.mode === "hidden") return 0;
    if (this.mode === "icons") return ICON_WIDTH;
    return this.width;
  }

  async restore() {
    const [mode, width] = await Promise.all([
      readSetting<SidebarMode>("sidebarMode", "expanded"),
      readSetting<number>("sidebarWidth", DEFAULT_WIDTH),
    ]);

    // Validated rather than trusted: this file is on disk and editable, and a
    // bad value here is a sidebar the user cannot see or cannot shrink.
    this.mode = ORDER.includes(mode) ? mode : "expanded";
    this.width = clamp(Number.isFinite(width) ? width : DEFAULT_WIDTH);
  }

  setMode(mode: SidebarMode) {
    this.mode = mode;
    void writeSetting("sidebarMode", mode);
  }

  /** Cycles expanded → icons → hidden → expanded. */
  cycle() {
    const next = ORDER[(ORDER.indexOf(this.mode) + 1) % ORDER.length];
    this.setMode(next);
  }

  /**
   * Brings the sidebar back from wherever it is.
   *
   * `hidden` is the one mode with no visible affordance of its own, so
   * something outside it has to be able to undo it.
   */
  reveal() {
    if (this.mode === "hidden") this.setMode("expanded");
  }

  /**
   * Live during a drag: clamped, and not yet a decision.
   *
   * The width is not persisted until the drag ends, but a *mode* change is
   * written immediately. A collapse mid-drag would otherwise be lost if the
   * drag ended in a way that never reached `commit` -- and "the sidebar came
   * back after a restart" is a worse bug than one redundant write.
   */
  drag(width: number) {
    if (width < COLLAPSE_AT) {
      if (this.mode !== "icons") this.setMode("icons");
      return;
    }
    // Dragging back out of the rail restores the sidebar rather than requiring
    // the user to let go and reach for the toggle.
    if (this.mode !== "expanded") this.setMode("expanded");
    this.width = clamp(width);
  }

  /** The drag ended. Now it is worth writing down. */
  commit() {
    this.resizing = false;
    void writeSetting("sidebarMode", this.mode);
    void writeSetting("sidebarWidth", this.width);
  }

  reset() {
    this.width = DEFAULT_WIDTH;
    this.mode = "expanded";
    void writeSetting("sidebarMode", this.mode);
    void writeSetting("sidebarWidth", this.width);
  }
}

function clamp(width: number) {
  return Math.min(Math.max(Math.round(width), MIN_WIDTH), MAX_WIDTH);
}

export const sidebar = new SidebarStore();
