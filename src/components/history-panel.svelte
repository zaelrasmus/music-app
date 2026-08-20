<script lang="ts">
    import PageShell from "$components/page-shell.svelte";
    import ListHeader from "$components/list-header.svelte";
    import EmptyState from "$components/empty-state.svelte";
    import TrackRow from "$components/track-row.svelte";
    import VirtualList from "$components/virtual-list.svelte";
    import { ROW_HEIGHT } from "$lib/virtual.svelte";
    import { historyStore } from "$lib/history.svelte";
    import { cacheStore } from "$lib/cache.svelte";
    import { player } from "$lib/player.svelte";
    import { formatTotal } from "$lib/duration";
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

    const total = $derived(historyStore.tracks.length);

    const totalSecs = $derived(
        historyStore.tracks.reduce((sum, t) => sum + (t.durationSecs ?? 0), 0),
    );

    async function shuffle() {
        if (total === 0) return;
        if (!player.shuffle) await player.toggleShuffle();
        await player.playQueue(
            queueIds,
            Math.floor(Math.random() * total),
            "recently played",
        );
    }
</script>

<PageShell>
    {#snippet hero()}
        <ListHeader
            eyebrow="History"
            title="Recently played"
            empty={total === 0}
            meta={[
                `${total} ${total === 1 ? "song" : "songs"}`,
                formatTotal(totalSecs),
                total > 0 ? `${offline} playable offline` : null,
            ]}
            onPlay={() => player.playQueue(queueIds, 0, "recently played")}
            onShuffle={shuffle}
        >
            {#snippet toolbar()}
                <p class="text-muted-foreground text-xs">
                    Tracks appear here once you have listened to a fair part of
                    them — not everything you skipped past.
                </p>
            {/snippet}
        </ListHeader>
    {/snippet}

    {#if total === 0}
        <EmptyState
            icon={HistoryIcon}
            title="Nothing here yet"
            hint="Play something for half a minute, or to the end, and it will show up."
        />
    {:else}
        <VirtualList rows={historyStore.tracks} estimateSize={ROW_HEIGHT}>
            {#snippet row(track, index)}
                <TrackRow
                    {track}
                    {queueIds}
                    {index}
                    contextName="recently played"
                />
            {/snippet}
        </VirtualList>
    {/if}
</PageShell>
