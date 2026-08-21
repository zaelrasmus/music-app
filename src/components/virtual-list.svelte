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
        /** Extra classes for the list element. */
        class?: string;
        row: Snippet<[Row, number]>;
    }

    let {
        rows,
        estimateSize,
        scrollElement = undefined,
        key = undefined,
        class: className = "",
        row,
    }: Props = $props();

    const fromContext = scrollContainer();

    const virtualizer = virtualRows({
        count: () => rows.length,
        scrollElement: () => scrollElement ?? fromContext?.element,
        estimateSize,
        getItemKey: key ? (index) => key(rows[index], index) : undefined,
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
<ul class={cn("relative", className)} style="height: {virtualizer.totalSize}px">
    {#each virtualizer.items as item (item.key)}
        <li
            class="absolute top-0 left-0 w-full"
            style="transform: translateY({item.start}px)"
            data-index={item.index}
            {@attach (node) => virtualizer.measure(node)}
        >
            {@render row(rows[item.index], item.index)}
        </li>
    {/each}
</ul>
