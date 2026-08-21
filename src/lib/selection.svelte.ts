/**
 * Which tracks the user has picked out of a list.
 *
 * One selection at a time, app-wide. Several would mean deciding what a bulk
 * action applies to when two lists both hold one, and there is no answer to
 * that a user would guess — so moving to another list clears it, which is what
 * everyone expects anyway.
 *
 * Deliberately holds ids and nothing else. Every action it feeds takes ids, and
 * keeping rows here would mean a second copy of the track that could disagree
 * with the list it came from.
 */
class SelectionStore {
  ids = $state<number[]>([]);

  /**
   * Where the last plain pick was, so a shift-click has something to reach
   * from.
   *
   * Null after any action that invalidates the run — clearing, or selecting
   * everything — because a range from a track that is no longer the reference
   * point lands somewhere arbitrary.
   */
  #anchor: number | null = null;

  get count() {
    return this.ids.length;
  }

  get active() {
    return this.ids.length > 0;
  }

  has(trackId: number) {
    return this.ids.includes(trackId);
  }

  clear() {
    this.ids = [];
    this.#anchor = null;
  }

  /**
   * The checkbox, and ctrl-click: this one track, on or off.
   *
   * Sets the anchor either way, so a shift-click afterwards reaches from the
   * row last touched rather than from wherever a run happened to start.
   */
  toggle(trackId: number) {
    this.ids = this.has(trackId)
      ? this.ids.filter((id) => id !== trackId)
      : [...this.ids, trackId];
    this.#anchor = trackId;
  }

  /**
   * Shift-click: everything between the anchor and here.
   *
   * `order` is the list as displayed, because a range means what the eye sees
   * — the rows between these two — and that depends on the current sort, not
   * on any order stored anywhere.
   *
   * Adds rather than replaces, so several ranges can be built up. With no
   * anchor it degrades to picking one track, which is what a shift-click on an
   * empty selection looks like to the person doing it.
   */
  extendTo(trackId: number, order: number[]) {
    if (this.#anchor === null) {
      this.toggle(trackId);
      return;
    }

    const from = order.indexOf(this.#anchor);
    const to = order.indexOf(trackId);

    if (from === -1 || to === -1) {
      this.toggle(trackId);
      return;
    }

    const [start, end] = from <= to ? [from, to] : [to, from];
    const run = order.slice(start, end + 1);

    this.ids = [...new Set([...this.ids, ...run])];
    // The anchor stays put: dragging a shift-click up and down should grow and
    // shrink from the same end, not walk away from it.
  }

  selectAll(order: number[]) {
    this.ids = [...order];
    this.#anchor = null;
  }

  /**
   * Drops ids that are no longer on screen.
   *
   * Called when a list reloads — after a bulk edit, a filter, a rescan — so
   * that "12 selected" can never mean twelve rows nobody can see. A stale id
   * would otherwise sit in the selection and be included in the next action.
   */
  retain(order: number[]) {
    const visible = new Set(order);
    const kept = this.ids.filter((id) => visible.has(id));
    if (kept.length !== this.ids.length) this.ids = kept;
  }
}

export const selection = new SelectionStore();
