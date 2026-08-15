<script lang="ts">
    import TrackRow from "$components/track-row.svelte";
    import { historyStore } from "$lib/history.svelte";
    import { cacheStore } from "$lib/cache.svelte";
    import HistoryIcon from "@lucide/svelte/icons/history";

    /**
     * Playing from history queues the whole list in the order shown, so it
     * carries on into what you played before rather than stopping dead.
     */
    const queueIds = $derived(historyStore.tracks.map((t) => t.id));

    /**
     * How many of these would survive going offline.
     *
     * Counted from the live cache rather than assumed: an entry can be evicted
     * between one render and the next, and a stale count here would promise
     * something the player cannot deliver.
     */
    const offline = $derived(
        historyStore.tracks.filter(
            (t) =>
                t.state === "downloaded" ||
                t.state === "present" ||
                cacheStore.isCached(t.id),
        ).length,
    );
</script>

<section class="flex flex-col gap-3">
    <div class="flex flex-col gap-1">
        <h2 class="flex items-center gap-2 text-lg font-semibold">
            <HistoryIcon class="size-4" />
            Recently played
            {#if historyStore.tracks.length > 0}
                <span class="text-muted-foreground text-sm font-normal">
                    ({historyStore.tracks.length})
                </span>
            {/if}
        </h2>
        {#if historyStore.tracks.length > 0}
            <p class="text-muted-foreground text-sm">
                {offline} of {historyStore.tracks.length} would still play offline.
            </p>
        {/if}
    </div>

    {#if historyStore.tracks.length === 0}
        <div
            class="text-muted-foreground rounded-lg border border-dashed px-6 py-8 text-center text-sm"
        >
            Nothing yet. Tracks appear here once you have listened to a fair
            part of them.
        </div>
    {:else}
        <ul class="flex flex-col">
            {#each historyStore.tracks as track, index (track.id)}
                <TrackRow
                    {track}
                    {queueIds}
                    {index}
                    contextName="recently played"
                />
            {/each}
        </ul>
    {/if}
</section>
