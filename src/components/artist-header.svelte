<script lang="ts">
    import CoverArt from "$components/cover-art.svelte";
    import { Button } from "$components/ui/button";
    import type { Collection } from "$lib/provider-search.svelte";
    import PlayIcon from "@lucide/svelte/icons/play";
    import ShuffleIcon from "@lucide/svelte/icons/shuffle";
    import BookmarkIcon from "@lucide/svelte/icons/bookmark";
    import LoaderIcon from "@lucide/svelte/icons/loader-circle";

    interface Props {
        collection: Collection;
        trackCount: number;
        busy: boolean;
        importing: boolean;
        onplay: () => void;
        onshuffle: () => void;
        onsave: () => void;
    }

    let {
        collection,
        trackCount,
        busy,
        importing,
        onplay,
        onshuffle,
        onsave,
    }: Props = $props();

    /** 7.16M rather than 7160000 — the magnitude is what a follower count is for. */
    function formatCount(count: number) {
        if (count < 1_000) return `${count}`;
        if (count < 1_000_000) return `${(count / 1_000).toFixed(1)}K`;
        return `${(count / 1_000_000).toFixed(1)}M`;
    }

    const providerName = $derived(
        collection.provider === "soundcloud" ? "SoundCloud" : "YouTube",
    );
</script>

<!--
  An artist is a person, not a track list, and the page says so before it says
  anything else: their picture large and round, their name at heading size, and
  the numbers underneath in the order someone actually asks for them.

  Deliberately unlike the playlist header. A playlist is defined by what is in
  it, so its art is a card and its contents start immediately; an artist is
  defined by who they are, so this takes the width and lets the songs begin
  below a rule of their own.
-->
<header class="mb-6 flex flex-col gap-5">
    <div class="flex items-end gap-5 px-2">
        <CoverArt
            seed={collection.title}
            src={collection.thumbnailUrl}
            class="size-28 shrink-0 rounded-full shadow-sm sm:size-36"
            glyph={false}
        />

        <div class="flex min-w-0 flex-1 flex-col gap-2 pb-1">
            <span class="text-muted-foreground text-xs font-medium tracking-wide uppercase">
                Artist
            </span>

            <h2 class="selectable truncate text-3xl leading-tight font-bold sm:text-4xl">
                {collection.title}
            </h2>

            <div class="text-muted-foreground flex flex-wrap items-center gap-x-1.5 text-xs">
                <span>{providerName}</span>
                {#if collection.followerCount !== null}
                    <span aria-hidden="true">·</span>
                    <span class="tabular-nums">
                        {formatCount(collection.followerCount)} followers
                    </span>
                {/if}
                {#if trackCount > 0}
                    <span aria-hidden="true">·</span>
                    <span class="tabular-nums">
                        {trackCount}
                        {trackCount === 1 ? "song" : "songs"}
                    </span>
                {/if}
            </div>
        </div>
    </div>

    <!--
      The actions sit under the whole header rather than beside the name, so
      they line up with the songs they act on.
    -->
    <div class="flex flex-wrap items-center gap-2 px-2">
        <Button size="sm" disabled={busy} onclick={onplay}>
            <PlayIcon data-icon="inline-start" />
            Play
        </Button>
        <Button variant="outline" size="sm" disabled={busy} onclick={onshuffle}>
            <ShuffleIcon data-icon="inline-start" />
            Shuffle
        </Button>
        <Button variant="outline" size="sm" disabled={busy} onclick={onsave}>
            {#if importing}
                <LoaderIcon data-icon="inline-start" class="animate-spin" />
            {:else}
                <BookmarkIcon data-icon="inline-start" />
            {/if}
            Save as playlist
        </Button>
    </div>
</header>
