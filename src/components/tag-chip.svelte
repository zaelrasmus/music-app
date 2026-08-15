<script lang="ts">
    import { tagHue } from "$lib/tag-colors";
    import XIcon from "@lucide/svelte/icons/x";
    import type { Snippet } from "svelte";

    interface Props {
        id: number;
        name: string;
        color?: string | null;
        /** A count, shown quietly after the name. */
        count?: number | null;
        /** Filter chips: pressed means "this tag is narrowing the list". */
        selected?: boolean;
        onclick?: () => void;
        /** Supplying this adds the × — omit it and the chip is not removable. */
        onremove?: () => void;
        size?: "sm" | "md";
        children?: Snippet;
    }

    let {
        id,
        name,
        color = null,
        count = null,
        selected = false,
        onclick,
        onremove,
        size = "sm",
    }: Props = $props();

    /**
     * The only thing that varies between colours.
     *
     * Lightness and chroma come from the theme, so a chip cannot end up
     * unreadable in one mode because it was picked in the other.
     */
    const hue = $derived(tagHue(id, color));

    const padding = $derived(size === "sm" ? "px-2 py-[1px] text-[11px]" : "px-2.5 py-0.5 text-xs");
    const interactive = $derived(onclick !== undefined);
</script>

<span
    class="tag-chip inline-flex max-w-[14rem] items-center gap-1 rounded-full border font-medium transition-[filter,opacity,box-shadow] {padding}
           {interactive ? 'cursor-pointer hover:brightness-105 dark:hover:brightness-125' : ''}
           {interactive && !selected ? 'opacity-70 hover:opacity-100' : ''}"
    style="--tag-h: {hue}{selected
        ? '; box-shadow: 0 0 0 1.5px oklch(var(--tag-fg-l) var(--tag-fg-c) var(--tag-h) / 0.5)'
        : ''}"
>
    {#if interactive}
        <button
            type="button"
            class="min-w-0 truncate focus-visible:outline-none"
            aria-pressed={selected}
            {onclick}
        >
            {name}
        </button>
    {:else}
        <span class="min-w-0 truncate">{name}</span>
    {/if}

    {#if count !== null}
        <span class="shrink-0 opacity-60 tabular-nums">{count}</span>
    {/if}

    {#if onremove}
        <button
            type="button"
            class="-mr-0.5 shrink-0 rounded-full opacity-60 transition-opacity hover:opacity-100 focus-visible:opacity-100 focus-visible:outline-none"
            aria-label="Remove tag {name}"
            onclick={onremove}
        >
            <XIcon class="size-3" />
        </button>
    {/if}
</span>
