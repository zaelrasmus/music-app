<script lang="ts">
    import { Button } from "$components/ui/button";
    import { Input } from "$components/ui/input";
    import {
        providerSearch,
        looksLikePreview,
        type SearchResult,
    } from "$lib/provider-search.svelte";
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
    function formatCount(count: number | null, noun: string) {
        if (count === null) return null;
        if (count < 1_000) return `${count} ${noun}`;
        if (count < 1_000_000) return `${(count / 1_000).toFixed(1)}K ${noun}`;
        if (count < 1_000_000_000)
            return `${(count / 1_000_000).toFixed(1)}M ${noun}`;
        return `${(count / 1_000_000_000).toFixed(1)}B ${noun}`;
    }

    /** SoundCloud counts plays, not views. */
    const countNoun = $derived(
        providerSearch.provider === "soundcloud" ? "plays" : "views",
    );

    /** SoundCloud artwork is square; YouTube thumbnails are 16:9. */
    const artClass = $derived(
        providerSearch.provider === "soundcloud"
            ? "aspect-square w-20"
            : "aspect-video w-32",
    );

    function uploaderLabel(result: SearchResult) {
        return (
            result.channel ??
            (result.provider === "soundcloud"
                ? "Unknown uploader"
                : "Unknown channel")
        );
    }
</script>

<section class="flex flex-col gap-3">
    <div class="flex flex-col gap-1">
        <h2 class="text-lg font-semibold">
            Search {providerSearch.providerName}
        </h2>
        <p class="text-muted-foreground text-sm">
            Raw results, exactly as uploaded — the duration and uploader are how
            you tell a song from a ten-hour loop.
        </p>
    </div>

    {#if providerSearch.providers.length > 1}
        <div class="flex items-center gap-1">
            {#each providerSearch.providers as provider (provider.id)}
                {@const selected = providerSearch.provider === provider.id}
                <button
                    type="button"
                    class="rounded-full border px-3 py-1 text-xs"
                    class:bg-primary={selected}
                    class:text-primary-foreground={selected}
                    class:border-primary={selected}
                    class:text-muted-foreground={!selected}
                    aria-pressed={selected}
                    onclick={() => providerSearch.setProvider(provider.id)}
                >
                    {provider.name}
                </button>
            {/each}
        </div>
    {/if}

    <div class="relative">
        <SearchIcon
            class="text-muted-foreground pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2"
        />
        <Input
            value={providerSearch.query}
            placeholder="Search for a song…"
            class="pl-9"
            oninput={(e) => providerSearch.queueSearch(e.currentTarget.value)}
            onkeydown={(e) => {
                if (e.key === "Enter") providerSearch.searchNow();
            }}
        />
    </div>

    {#if providerSearch.error}
        <p
            class="border-destructive/50 text-destructive rounded-md border px-3 py-2 text-sm"
            role="alert"
        >
            {providerSearch.error}
        </p>
    {/if}

    {#if providerSearch.searching}
        <p class="text-muted-foreground text-sm">Searching…</p>
    {:else if providerSearch.searched && providerSearch.results.length === 0}
        <p class="text-muted-foreground text-sm">No results.</p>
    {/if}

    {#if providerSearch.results.length > 0}
        <ul
            class="flex flex-col gap-2"
            class:opacity-50={providerSearch.searching}
        >
            {#each providerSearch.results as result (result.remoteId)}
                {@const duration = formatDuration(result.durationSecs)}
                {@const count = formatCount(result.viewCount, countNoun)}
                {@const busy = providerSearch.saving === result.remoteId}
                {@const preview = looksLikePreview(result)}
                <li class="bg-card flex items-start gap-3 rounded-lg border p-2">
                    <div
                        class="bg-muted relative shrink-0 overflow-hidden rounded {artClass}"
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
                            {uploaderLabel(result)}
                            {#if count}
                                · {count}
                            {/if}
                        </span>

                        {#if preview}
                            <!-- Worth saying before the click, not after: this
                                 saves and plays fine, it just stops at 0:30. -->
                            <span
                                class="text-muted-foreground border-muted-foreground/40 mt-0.5 w-fit rounded border px-1 text-[10px]"
                                title="SoundCloud only serves a 30-second snippet for this upload (Go+ gated). Another upload of the same song may be full length."
                            >
                                likely a 30s preview
                            </span>
                        {/if}
                    </div>

                    <!-- Queueing saves the result first: the queue holds track
                         ids, and a search result is not a track yet. -->
                    <TrackActions
                        resolveTrackId={() => providerSearch.saveResult(result)}
                        label="Queue {result.title}"
                    />

                    <AddToPlaylist
                        resolveTrackId={() => providerSearch.saveResult(result)}
                        label="Add {result.title} to a playlist"
                    />

                    <Button
                        variant="ghost"
                        size="icon"
                        class="shrink-0"
                        aria-label="Play {result.title}"
                        disabled={busy}
                        onclick={() => providerSearch.playResult(result)}
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
