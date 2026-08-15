<script lang="ts">
    import { Button } from "$components/ui/button";
    import PageShell from "$components/page-shell.svelte";
    import EmptyState from "$components/empty-state.svelte";
    import TrackRow from "$components/track-row.svelte";
    import { historyStore } from "$lib/history.svelte";
    import { cacheStore } from "$lib/cache.svelte";
    import { player } from "$lib/player.svelte";
    import HistoryIcon from "@lucide/svelte/icons/history";
    import PlayIcon from "@lucide/svelte/icons/play";
    import WifiOffIcon from "@lucide/svelte/icons/wifi-off";

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
</script>

<PageShell
    title="Recently played"
    badge={total > 0 ? `${total}` : null}
    subtitle="Tracks appear here once you have listened to a fair part of them — not everything you skipped past."
>
    {#snippet actions()}
        <Button
            size="sm"
            disabled={total === 0}
            onclick={() => player.playQueue(queueIds, 0, "recently played")}
        >
            <PlayIcon data-icon="inline-start" />
            Play
        </Button>
    {/snippet}

    {#snippet toolbar()}
        {#if total > 0}
            <p class="text-muted-foreground flex items-center gap-1.5 text-xs">
                <WifiOffIcon class="text-signal size-3.5" />
                <span>
                    <span class="text-foreground font-medium tabular-nums">
                        {offline} of {total}
                    </span>
                    would still play with no connection.
                </span>
            </p>
        {/if}
    {/snippet}

    {#if total === 0}
        <EmptyState
            icon={HistoryIcon}
            title="Nothing here yet"
            hint="Play something for half a minute, or to the end, and it will show up."
        />
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
</PageShell>
