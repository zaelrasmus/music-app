<script lang="ts" generics="Row">
    import type { Snippet } from "svelte";
    import { virtualRows } from "$lib/virtual.svelte";
    import { scrollContainer } from "$lib/scroll-container.svelte";
    import { cn } from "$lib/utils";

    interface Props {
        rows: Row[];
        /**
         * A row's height before measurement, in pixels.
         *
         * Only ever an opening bid — every rendered row reports its real
         * height back. Being close matters anyway, because it is what sizes
         * the scrollbar for the rows nobody has looked at yet.
         *
         * A function when the list holds more than one shape of row.
         */
        estimateSize: number | ((index: number) => number);
        /**
         * The scrolling element, when the caller owns one.
         *
         * Left off inside a `PageShell`, which owns the scroller for every
         * view and offers it through context.
         */
        scrollElement?: HTMLElement | null;
        /**
         * A stable identity for a row.
         *
         * Needed when rows differ in height and the list can shift — see
         * `getItemKey` in `virtualRows`.
         */
        key?: (row: Row, index: number) => string | number;
        /**
         * What makes this a *different* list rather than the same one changed.
         *
         * When it changes the list goes back to the top. Pass whatever decides
         * the contents — a search box, the chosen filters, the sort order.
         *
         * This exists because scroll position and row count are independent,
         * and nothing puts them back in step. Search a library of a thousand
         * songs while scrolled to row 800 and the list is still at 51,000px
         * while the results are 400 rows long: the browser pins you to the
         * bottom of them, and the best match — the one you typed — is at the
         * top, off screen. It reads as "the search found nothing".
         *
         * Deliberately not "whenever `rows` changes". A rescan, a cover
         * arriving, a track being renamed all replace the array with the same
         * list, and yanking someone back to the top for those would be its own
         * bug. Only the caller knows which is which.
         */
        resetKey?: unknown;
        /** Extra classes for the list element. */
        class?: string;
        row: Snippet<[Row, number]>;
    }

    let {
        rows,
        estimateSize,
        scrollElement = undefined,
        key = undefined,
        resetKey = undefined,
        class: className = "",
        row,
    }: Props = $props();

    const fromContext = scrollContainer();

    /**
     * Hoisted so the attachment keeps its identity.
     *
     * Written inline, `{@attach (node) => …}` builds a new function on every
     * render, which Svelte treats as a new attachment: it tears the old one
     * down and re-registers the row with the `ResizeObserver`. Re-observing
     * during a render is what produces "ResizeObserver loop completed with
     * undelivered notifications" once anything makes the rows re-render often.
     */
    const measure = (node: HTMLElement) => virtualizer.measure(node);

    const virtualizer = virtualRows({
        count: () => rows.length,
        scrollElement: () => scrollElement ?? fromContext?.element,
        estimateSize,
        getItemKey: key ? (index) => key(rows[index], index) : undefined,
    });

    /**
     * Back to the top when the caller says this is a different list.
     *
     * Declared after `virtualRows` so it runs after the effect that tells the
     * virtualizer the new row count — scrolling to the top of a list whose
     * length is still the old one would be corrected straight back.
     *
     * The first run is skipped. Mounting *is* a change of contents by this
     * measure, and a list that scrolls itself on mount would undo any position
     * restored for it.
     */
    /**
     * The items to render — never more of them than there are rows to fill.
     *
     * `virtualizer.items` is republished from an `$effect`, and effects run
     * *after* the DOM has been updated. So when a list shrinks there is one
     * render in which the old item indexes are read against the new, shorter
     * `rows`, and `rows[item.index]` is `undefined`.
     *
     * Row components dereference what they are handed — `TrackRow` reads
     * `track.state` in a `$derived` — so that render throws, and a throw part
     * way through an update leaves everything after it in the flush unapplied.
     * The visible symptom is not a missing row: it is the *rest of the screen*
     * freezing mid-change, which is how a list stuck at its dimmed "loading"
     * opacity happens. Typing quickly hits it because every keystroke is
     * another chance for the list to shrink.
     *
     * Dropped here rather than guarded inside the snippet: a row this
     * component invented is not one the caller should have to null-check.
     */
    const visible = $derived(
        virtualizer.items.filter((item) => item.index < rows.length),
    );

    let mounted = false;
    let previous: unknown = undefined;

    $effect(() => {
        const key = resetKey;

        if (!mounted) {
            mounted = true;
            previous = key;
            return;
        }
        // Compared rather than merely depended on, so an effect that re-runs
        // for some other reason does not throw the list back to the top. Keys
        // are strings for this reason -- an array or object would be a new
        // value every render and never compare equal.
        if (key === previous) return;

        previous = key;
        virtualizer.scrollToTop();
    });
</script>

<!--
  Two elements, and both are load-bearing.

  The outer one is the full height of every row there would be, so the
  scrollbar is the size it would be if the whole list were rendered — that is
  the entire illusion. The inner ones are lifted into place by transform
  rather than by `top`, which keeps them off the layout path and lets the
  browser move them on the compositor.
-->
<!--
  `data-scrolling` turns off transitions inside the list while it moves.

  Chromium recomputes `:hover` on every frame of a scroll, so with the pointer
  resting anywhere over the list a fast flick drags hover across dozens of rows
  in a second. Every one of them then spends 150ms fading its highlight back
  out, its duration back in and its menu button back out -- so at any instant
  roughly nine rows behind the cursor are caught mid-fade, and the list reads
  as smeared and half-transparent rather than as a list.

  Only the *animation* is suppressed, not the hover itself: making the rows
  `pointer-events: none` would be the usual trick and would also skip
  hit-testing, but a row being dragged to reorder auto-scrolls the list, and a
  row that cannot be hit is a row that cannot be dropped on.
-->
<ul
    class={cn("relative", className)}
    style="height: {virtualizer.totalSize}px"
    data-scrolling={virtualizer.scrolling ? "true" : undefined}
>
    {#each visible as item (item.key)}
        <li
            class="absolute top-0 left-0 w-full"
            style="transform: translateY({item.start}px)"
            data-index={item.index}
            {@attach measure}
        >
            {@render row(rows[item.index], item.index)}
        </li>
    {/each}
</ul>
