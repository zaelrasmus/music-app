<script lang="ts">
    import { coverGradient } from "$lib/cover";
    import { covers } from "$lib/covers.svelte";
    import { cn } from "$lib/utils";
    import MusicIcon from "@lucide/svelte/icons/music";

    interface Props {
        /** Text the generated art is derived from — usually artist + title. */
        seed: string;
        /**
         * A key into the cover store: embedded artwork, a fetched thumbnail,
         * or a playlist image the user picked. Null means there is none.
         */
        coverKey?: string | null;
        /** A remote URL, for search results that are not saved tracks yet. */
        src?: string | null;
        /** Tailwind size classes; the caller owns the dimensions. */
        class?: string;
        /** Hides the note glyph on small tiles, where it is just clutter. */
        glyph?: boolean;
    }

    let {
        seed,
        coverKey = null,
        src = null,
        class: className = "size-10",
        glyph = true,
    }: Props = $props();

    const gradient = $derived(coverGradient(seed));

    /**
     * Stored artwork wins over a remote URL.
     *
     * Both can be present on a saved track — the thumbnail URL stays on the
     * row so the cover can be refetched — and the stored copy is the one that
     * works offline.
     */
    const source = $derived(covers.url(coverKey) ?? src);

    /**
     * Set when an image fails to load, so the gradient is all that shows.
     *
     * A cover can be swept out from under a row that is already on screen;
     * without this the browser would paint its own broken-image glyph over
     * perfectly good generated art.
     */
    let failed = $state(false);
    // Reset when the source changes, or one failure would poison the tile for
    // every track that later reuses this component instance.
    $effect(() => {
        source;
        failed = false;
    });
</script>

<!--
  The gradient stays behind the image rather than being replaced by it: a
  remote thumbnail can fail to load or arrive late, and a coloured tile is a
  better placeholder for that moment than an empty box that then pops.
-->
<div
    class={cn("relative shrink-0 overflow-hidden rounded-md", className)}
    style="background-image: {gradient}"
>
    {#if glyph}
        <MusicIcon
            class="absolute top-1/2 left-1/2 size-[42%] -translate-x-1/2 -translate-y-1/2 text-white/45"
        />
    {/if}

    {#if source && !failed}
        <img
            src={source}
            alt=""
            loading="lazy"
            decoding="async"
            class="relative size-full object-cover"
            onerror={() => (failed = true)}
        />
    {/if}
</div>
