<script lang="ts">
    import { Button } from "$components/ui/button";
    import { Input } from "$components/ui/input";
    import { youtubeSearch } from "$lib/youtube.svelte";
    import SearchIcon from "@lucide/svelte/icons/search";
    import PlayIcon from "@lucide/svelte/icons/play";
    import LoaderIcon from "@lucide/svelte/icons/loader-circle";
    import AddToPlaylist from "$components/add-to-playlist.svelte";
    import TrackActions from "$components/track-actions.svelte";

    /** Whole minutes and seconds; hours only when there are hours. */
    function formatDuration(secs: number | null) {
        if (secs === null) return null;

        const total = Math.round(secs);
        const hours = Math.floor(total / 3600);
        const minutes = Math.floor((total % 3600) / 60);
        const seconds = total % 60;

        const pad = (n: number) => String(n).padStart(2, "0");
        return hours > 0
            ? `${hours}:${pad(minutes)}:${pad(seconds)}`
            : `${minutes}:${pad(seconds)}`;
    }

    /** 1.5M rather than 1535000969 — the magnitude is what matters. */
    function formatViews(views: number | null) {
        if (views === null) return null;
        if (views < 1_000) return `${views} views`;
        if (views < 1_000_000) return `${(views / 1_000).toFixed(1)}K views`;
        if (views < 1_000_000_000) return `${(views / 1_000_000).toFixed(1)}M views`;
        return `${(views / 1_000_000_000).toFixed(1)}B views`;
    }
</script>

<section class="flex flex-col gap-3">
    <div class="flex flex-col gap-1">
        <h2 class="text-lg font-semibold">Search YouTube</h2>
        <p class="text-muted-foreground text-sm">
            Raw results, exactly as uploaded — the duration and channel are how
            you tell a song from a ten-hour loop.
        </p>
    </div>

    <div class="relative">
        <SearchIcon
            class="text-muted-foreground pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2"
        />
        <Input
            value={youtubeSearch.query}
            placeholder="Search for a song…"
            class="pl-9"
            oninput={(e) =>
                youtubeSearch.queueSearch(e.currentTarget.value)}
            onkeydown={(e) => {
                if (e.key === "Enter") youtubeSearch.searchNow();
            }}
        />
    </div>

    {#if youtubeSearch.error}
        <p
            class="border-destructive/50 text-destructive rounded-md border px-3 py-2 text-sm"
            role="alert"
        >
            {youtubeSearch.error}
        </p>
    {/if}

    {#if youtubeSearch.searching}
        <p class="text-muted-foreground text-sm">Searching…</p>
    {:else if youtubeSearch.searched && youtubeSearch.results.length === 0}
        <p class="text-muted-foreground text-sm">No results.</p>
    {/if}

    {#if youtubeSearch.results.length > 0}
        <ul class="flex flex-col gap-2" class:opacity-50={youtubeSearch.searching}>
            {#each youtubeSearch.results as result (result.videoId)}
                {@const duration = formatDuration(result.durationSecs)}
                {@const views = formatViews(result.viewCount)}
                {@const busy = youtubeSearch.saving === result.videoId}
                <li
                    class="bg-card flex items-start gap-3 rounded-lg border p-2"
                >
                    <div
                        class="bg-muted relative aspect-video w-32 shrink-0 overflow-hidden rounded"
                    >
                        {#if result.thumbnailUrl}
                            <img
                                src={result.thumbnailUrl}
                                alt=""
                                loading="lazy"
                                class="size-full object-cover"
                            />
                        {/if}
                        {#if result.isLive}
                            <span
                                class="bg-destructive text-destructive-foreground absolute right-1 bottom-1 rounded px-1 text-[10px] font-medium"
                            >
                                LIVE
                            </span>
                        {:else if duration}
                            <span
                                class="absolute right-1 bottom-1 rounded bg-black/80 px-1 text-[10px] font-medium text-white tabular-nums"
                            >
                                {duration}
                            </span>
                        {/if}
                    </div>

                    <div class="flex min-w-0 flex-1 flex-col gap-0.5">
                        <!-- Full title, not truncated to one line: the tail is
                             often what distinguishes an edit from the original. -->
                        <span class="text-sm leading-snug">{result.title}</span>
                        <span class="text-muted-foreground truncate text-xs">
                            {result.channel ?? "Unknown channel"}
                            {#if views}
                                · {views}
                            {/if}
                        </span>
                    </div>

                    <!-- Queueing saves the result first: the queue holds track
                         ids, and a search result is not a track yet. -->
                    <TrackActions
                        resolveTrackId={() => youtubeSearch.saveResult(result)}
                        label="Queue {result.title}"
                    />

                    <AddToPlaylist
                        resolveTrackId={() => youtubeSearch.saveResult(result)}
                        label="Add {result.title} to a playlist"
                    />

                    <Button
                        variant="ghost"
                        size="icon"
                        class="shrink-0"
                        aria-label="Play {result.title}"
                        disabled={busy}
                        onclick={() => youtubeSearch.playResult(result)}
                    >
                        {#if busy}
                            <LoaderIcon class="animate-spin" />
                        {:else}
                            <PlayIcon />
                        {/if}
                    </Button>
                </li>
            {/each}
        </ul>
    {/if}
</section>
