<script lang="ts">
    import type { Snippet } from "svelte";
    import CoverArt from "$components/cover-art.svelte";
    import PlayingBars from "$components/playing-bars.svelte";
    import SourceBadge from "$components/source-badge.svelte";
    import { coverSeed } from "$lib/cover";
    import { cacheStore } from "$lib/cache.svelte";
    import { player } from "$lib/player.svelte";
    import type { QueueEntry } from "$lib/queue.svelte";
    import PlayIcon from "@lucide/svelte/icons/play";

    interface Props {
        entry: QueueEntry;
        /** The row that is audible right now. */
        current?: boolean;
        /** Runs when the row is clicked. Omit for a row that cannot be played. */
        onplay?: () => void;
        /** Drag handle and controls, supplied by the list that owns them. */
        leading?: Snippet;
        trailing?: Snippet;
    }

    let {
        entry,
        current = false,
        onplay,
        leading,
        trailing,
    }: Props = $props();

    function formatDuration(secs: number | null) {
        if (secs === null) return "";
        const m = Math.floor(secs / 60);
        const s = secs % 60;
        return `${m}:${String(s).padStart(2, "0")}`;
    }

    const missing = $derived(entry.state === "missing");
</script>

<!--
  The same row the library shows, at sidebar scale.

  It used to be two lines of text and a duration — readable, but nothing like
  the lists everywhere else in the app, and there was no way to say "play that
  one". Artwork is what makes a queue scannable: by the time a track is queued
  you have usually already seen its cover once, and recognising it is faster
  than reading its title.
-->
<div
    class="group/queue-row relative flex items-center gap-2.5 rounded-md px-1.5 py-1.5 transition-colors
           {current ? 'bg-accent/60' : 'hover:bg-accent/60'}"
    class:opacity-50={missing}
>
    <!--
      The click target, stretched across the row and rendered *first*.

      Order is the whole trick. It is absolutely positioned, so it covers the
      artwork and the text — which are static and therefore paint beneath it —
      while the controls after it are positioned themselves and stay on top,
      clickable. Wrapping the row in a `<button>` instead would swallow the
      drag handle and the remove button; putting this last would swallow them
      just the same.
    -->
    {#if onplay && !missing && !current}
        <button
            type="button"
            class="focus-visible:ring-ring absolute inset-0 rounded-md focus-visible:ring-2 focus-visible:outline-none"
            aria-label="Play {entry.title}"
            onclick={onplay}
        ></button>
    {/if}

    {#if leading}
        <span class="relative flex shrink-0 items-center">
            {@render leading()}
        </span>
    {/if}

    <div class="relative shrink-0">
        <CoverArt
            seed={coverSeed(entry)}
            coverKey={entry.coverKey}
            src={entry.remoteThumbnailUrl}
            class="size-9"
            glyph={false}
        />

        {#if current}
            <span
                class="absolute inset-0 grid place-items-center rounded-md bg-black/55 text-white"
                aria-hidden="true"
            >
                <PlayingBars animate={player.state === "playing"} />
            </span>
        {:else if onplay && !missing}
            <!--
              The play affordance sits on the artwork, exactly as it does in
              search results, so the same gesture means the same thing in both
              places. The whole row is the button; this only shows where to
              aim.
            -->
            <span
                class="absolute inset-0 grid place-items-center rounded-md bg-black/45 opacity-0 transition-opacity group-hover/queue-row:opacity-100"
                aria-hidden="true"
            >
                <PlayIcon class="size-4 fill-white text-white" />
            </span>
        {/if}
    </div>

    <div class="flex min-w-0 flex-1 flex-col gap-0.5">
        <span
            class="truncate text-xs leading-tight {current
                ? 'text-foreground font-medium'
                : ''}"
        >
            {entry.title}
        </span>
        <span
            class="text-muted-foreground flex items-center gap-1.5 text-[11px] leading-tight"
        >
            <span class="truncate">{entry.artist ?? "Unknown artist"}</span>
            <SourceBadge
                source={entry.source}
                state={entry.state}
                durationSecs={entry.durationSecs}
                cached={cacheStore.isCached(entry.trackId)}
                compact
            />
        </span>
    </div>

    <span
        class="text-muted-foreground shrink-0 text-[11px] tabular-nums {trailing
            ? 'group-hover/queue-row:hidden'
            : ''}"
    >
        {formatDuration(entry.durationSecs)}
    </span>

    {#if trailing}
        <span class="relative flex shrink-0 items-center">
            {@render trailing()}
        </span>
    {/if}
</div>
