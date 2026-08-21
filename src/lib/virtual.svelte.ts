import {
  Virtualizer,
  elementScroll,
  observeElementOffset,
  observeElementRect,
  type VirtualItem,
} from "@tanstack/svelte-virtual";
import { untrack } from "svelte";

export type { VirtualItem };

/**
 * Row virtualisation, driven by runes.
 *
 * ## Why not `createVirtualizer`
 *
 * `@tanstack/svelte-virtual` ships a Svelte adapter, and it is not the right
 * fit here. It exposes the virtualizer as a Svelte *store*, which was the only
 * option when it was written — its own devDependency is Svelte 4. Consuming it
 * from a runes component means `$virtualizer` auto-subscription, and updating
 * its options as a list grows means an `$effect` that both reads the store and
 * calls `setOptions` on it. `setOptions` fires `onChange`, `onChange` writes
 * the store, and the effect that read the store runs again: a loop that has to
 * be broken by hand with untracked reads.
 *
 * The adapter is a thin wrapper over `@tanstack/virtual-core` — which the same
 * package re-exports in full — so this takes the core directly and holds the
 * instance in a plain closure variable that nothing tracks. The only reactive
 * values are the two the template actually needs.
 *
 * `_didMount` and `_willUpdate` are underscore-prefixed but are the core's
 * intended integration points: the official adapter calls exactly these, and
 * they are what every framework adapter is built on.
 */
export type RowVirtualizer = {
  /** The rows to render right now, with their offsets. */
  readonly items: VirtualItem[];
  /** The height the spacer must have for the scrollbar to be honest. */
  readonly totalSize: number;
  /**
   * Attach to each rendered row to measure its real height.
   *
   * Rows here are not a fixed height — a search result's title wraps to two
   * lines when it is long — and an estimate that is wrong by a few pixels per
   * row compounds into a scrollbar that lies by a screenful over a thousand
   * tracks.
   */
  measure: (node: HTMLElement) => void;
  scrollToIndex: (index: number, options?: { align?: "start" | "center" | "end" | "auto" }) => void;
};

export function virtualRows(config: {
  /** How many rows there are. Read reactively. */
  count: () => number;
  /** The scrolling element. Read reactively — it is bound after init. */
  scrollElement: () => HTMLElement | null | undefined;
  /**
   * A row's height before it has been measured.
   *
   * A number when every row is the same shape. A function when one list holds
   * several — the queue panel puts headings, tracks and notes in a single
   * list, and a heading estimated as a track leaves the scrollbar noticeably
   * wrong until enough of it has been measured.
   */
  estimateSize: number | ((index: number) => number);
  /**
   * A stable identity for the row at `index`.
   *
   * Worth supplying whenever rows differ in height and the list can shift.
   * Measured heights are cached against whatever identifies a row, and by
   * default that is its position — so when the queue pops a track and
   * everything moves up one, the height measured for a heading gets applied
   * to the track that took its place, and the list jumps.
   */
  getItemKey?: (index: number) => string | number;
  /**
   * Rows kept mounted beyond the viewport.
   *
   * Enough that a flick of the wheel lands on rendered rows rather than on
   * blanks, and small enough that the DOM stays short.
   */
  overscan?: number;
}): RowVirtualizer {
  let items = $state<VirtualItem[]>([]);
  let totalSize = $state(0);

  // Deliberately not `$state`: nothing renders the instance, and making it
  // reactive is what creates the feedback loop described above.
  let instance: Virtualizer<HTMLElement, HTMLElement> | null = null;

  const publish = (virtualizer: Virtualizer<HTMLElement, HTMLElement>) => {
    items = virtualizer.getVirtualItems();
    totalSize = virtualizer.getTotalSize();
  };

  $effect(() => {
    const element = config.scrollElement();
    if (!element) return;

    const virtualizer = new Virtualizer<HTMLElement, HTMLElement>({
      // Untracked, or this effect depends on the row count and the teardown
      // below runs on every list change -- discarding every measured height
      // and resetting the scroll position, which is the exact outcome the
      // second effect exists to avoid. The count is kept current there.
      count: untrack(config.count),
      getScrollElement: () => element,
      estimateSize: (index) =>
        typeof config.estimateSize === "function"
          ? config.estimateSize(index)
          : config.estimateSize,
      overscan: config.overscan ?? 6,
      ...(config.getItemKey ? { getItemKey: config.getItemKey } : {}),
      observeElementRect,
      observeElementOffset,
      scrollToFn: elementScroll,
      onChange: (virtualizer) => publish(virtualizer),
    });

    instance = virtualizer;
    const unmount = virtualizer._didMount();
    virtualizer._willUpdate();
    publish(virtualizer);

    return () => {
      unmount();
      instance = null;
    };
  });

  // Kept separate from the effect above so a list growing does not tear down
  // and rebuild the virtualizer — which would lose every measured row height
  // and reset the scroll position.
  $effect(() => {
    const count = config.count();
    const virtualizer = instance;
    if (!virtualizer) return;

    virtualizer.setOptions({ ...virtualizer.options, count });
    virtualizer._willUpdate();
    publish(virtualizer);
  });

  return {
    get items() {
      return items;
    },
    get totalSize() {
      return totalSize;
    },
    measure: (node: HTMLElement) => instance?.measureElement(node),
    scrollToIndex: (index, options) => instance?.scrollToIndex(index, options),
  };
}

/**
 * A track row's height before it is measured, in pixels.
 *
 * One number shared by every list that shows tracks, because they all render
 * the same row: 40px of artwork inside 12px of padding, plus a hairline. Each
 * rendered row still reports its real height — a title that wraps to two lines
 * is taller — so this only has to be close, and only matters for the rows
 * nobody has scrolled to yet.
 */
export const ROW_HEIGHT = 64;

/**
 * A search result row's height before it is measured, in pixels.
 *
 * Taller than a library row: the artwork is a 16:9 thumbnail rather than a
 * square, and the title is shown in full rather than truncated, because the
 * tail of an upload's name is often the only thing separating an edit from the
 * original.
 */
export const SEARCH_ROW_HEIGHT = 88;
