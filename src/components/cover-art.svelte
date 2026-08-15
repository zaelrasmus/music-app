<script lang="ts">
    import { coverGradient } from "$lib/cover";
    import MusicIcon from "@lucide/svelte/icons/music";

    interface Props {
        /** Text the generated art is derived from — usually artist + title. */
        seed: string;
        /** A real thumbnail, when one exists. Search results have these. */
        src?: string | null;
        /** Tailwind size classes; the caller owns the dimensions. */
        class?: string;
        /** Hides the note glyph on small tiles, where it is just clutter. */
        glyph?: boolean;
    }

    let { seed, src = null, class: className = "size-10", glyph = true }: Props = $props();

    const gradient = $derived(coverGradient(seed));
</script>

<!--
  The gradient stays behind the image rather than being replaced by it: a
  remote thumbnail can fail to load or arrive late, and a coloured tile is a
  better placeholder for that moment than an empty box that then pops.
-->
<div
    class="relative shrink-0 overflow-hidden rounded-md {className}"
    style="background-image: {gradient}"
>
    {#if glyph}
        <MusicIcon
            class="absolute top-1/2 left-1/2 size-[42%] -translate-x-1/2 -translate-y-1/2 text-white/45"
        />
    {/if}

    {#if src}
        <img {src} alt="" loading="lazy" class="relative size-full object-cover" />
    {/if}
</div>
