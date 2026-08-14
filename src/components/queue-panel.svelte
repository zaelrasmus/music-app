<script lang="ts">
    import { Button } from "$components/ui/button";
    import { queueStore, type QueueEntry } from "$lib/queue.svelte";
    import SourceBadge from "$components/source-badge.svelte";
    import { player } from "$lib/player.svelte";
    import XIcon from "@lucide/svelte/icons/x";
    import GripVerticalIcon from "@lucide/svelte/icons/grip-vertical";
    import ListMusicIcon from "@lucide/svelte/icons/list-music";

    /** Index being dragged, and the index it is hovering over. */
    let dragFrom = $state<number | null>(null);
    let dragOver = $state<number | null>(null);

    async function drop(toIndex: number) {
        const from = dragFrom;
        dragFrom = null;
        dragOver = null;

        if (from === null || from === toIndex) return;

        // Addressed by entry id, not by index: the front of the queue pops
        // whenever a track ends, so the row under the cursor may not be the
        // row this index meant by the time the drop lands.
        const entryId = queueStore.manual[from]?.entryId;
        if (entryId === null || entryId === undefined) return;

        await queueStore.reorder(entryId, toIndex);
    }

    function formatDuration(secs: number | null) {
        if (secs === null) return "";
        const m = Math.floor(secs / 60);
        const s = secs % 60;
        return `${m}:${String(s).padStart(2, "0")}`;
    }

    const contextHeading = $derived(
        queueStore.contextName
            ? `Next from ${queueStore.contextName}`
            : "Next up",
    );
</script>

<!-- Unavailable tracks stay listed, marked. Hiding them would make the panel
     disagree with what actually plays. -->
{#snippet badge(entry: QueueEntry)}
    <SourceBadge
        source={entry.source}
        state={entry.state}
        durationSecs={entry.durationSecs}
        compact
    />
{/snippet}

{#if queueStore.open}
    <aside
        class="bg-card fixed right-4 bottom-24 z-20 flex max-h-[60vh] w-80 flex-col rounded-lg border shadow-lg"
        aria-label="Play queue"
    >
        <header class="flex items-center justify-between gap-2 border-b px-3 py-2">
            <h2 class="flex items-center gap-2 text-sm font-semibold">
                <ListMusicIcon class="size-4" />
                Up next
            </h2>
            <Button
                variant="ghost"
                size="icon"
                aria-label="Close queue"
                onclick={() => queueStore.toggle()}
            >
                <XIcon />
            </Button>
        </header>

        <div class="flex flex-col gap-3 overflow-y-auto px-3 py-2">
            {#if queueStore.current}
                <section class="flex flex-col gap-1">
                    <h3
                        class="text-muted-foreground text-[11px] font-medium tracking-wide uppercase"
                    >
                        Now playing
                    </h3>
                    <div class="flex items-center gap-2 text-sm">
                        <span class="min-w-0 flex-1 truncate">
                            <span class="font-medium">{queueStore.current.title}</span>
                            <span class="text-muted-foreground">
                                — {queueStore.current.artist ?? "Unknown artist"}
                            </span>
                        </span>
                        {@render badge(queueStore.current)}
                    </div>
                </section>
            {/if}

            <!-- The manual queue: what the user explicitly asked for. -->
            <section class="flex flex-col gap-1">
                <div class="flex items-center justify-between gap-2">
                    <h3
                        class="text-muted-foreground text-[11px] font-medium tracking-wide uppercase"
                    >
                        Queue
                    </h3>
                    {#if queueStore.manual.length > 0}
                        <button
                            type="button"
                            class="text-muted-foreground hover:text-foreground text-[11px] underline"
                            onclick={() => queueStore.clear()}
                        >
                            Clear
                        </button>
                    {/if}
                </div>

                {#if queueStore.manual.length === 0}
                    <p class="text-muted-foreground text-xs">
                        Nothing queued. Use “Play next” or “Add to queue” on any
                        track.
                    </p>
                {:else}
                    <ul class="flex flex-col">
                        {#each queueStore.manual as entry, index (entry.entryId)}
                            <li
                                class="flex items-center gap-1.5 border-b py-1 text-sm last:border-b-0"
                                class:opacity-50={entry.state === "missing"}
                                class:bg-muted={dragOver === index}
                                draggable="true"
                                ondragstart={() => (dragFrom = index)}
                                ondragover={(e) => {
                                    e.preventDefault();
                                    dragOver = index;
                                }}
                                ondragleave={() => {
                                    if (dragOver === index) dragOver = null;
                                }}
                                ondrop={(e) => {
                                    e.preventDefault();
                                    drop(index);
                                }}
                                ondragend={() => {
                                    dragFrom = null;
                                    dragOver = null;
                                }}
                            >
                                <GripVerticalIcon
                                    class="text-muted-foreground size-3.5 shrink-0 cursor-grab"
                                />

                                <span class="min-w-0 flex-1 truncate text-xs">
                                    {entry.title}
                                    <span class="text-muted-foreground">
                                        — {entry.artist ?? "Unknown artist"}
                                    </span>
                                </span>

                                {@render badge(entry)}

                                <span
                                    class="text-muted-foreground shrink-0 text-[11px] tabular-nums"
                                >
                                    {formatDuration(entry.durationSecs)}
                                </span>

                                <Button
                                    variant="ghost"
                                    size="icon"
                                    class="size-6"
                                    aria-label="Remove {entry.title} from the queue"
                                    onclick={() =>
                                        entry.entryId !== null &&
                                        queueStore.remove(entry.entryId)}
                                >
                                    <XIcon class="size-3" />
                                </Button>
                            </li>
                        {/each}
                    </ul>
                {/if}
            </section>

            <!-- The context continuation. Read-only: a displayed row maps to a
                 shuffled permutation index, so removing one here is a larger
                 change than it looks. -->
            <section class="flex flex-col gap-1">
                <h3
                    class="text-muted-foreground text-[11px] font-medium tracking-wide uppercase"
                >
                    {contextHeading}
                </h3>

                {#if queueStore.upNext.length === 0}
                    <p class="text-muted-foreground text-xs">
                        Nothing follows — the list ends here.
                    </p>
                {:else}
                    <ul class="flex flex-col">
                        {#each queueStore.upNext as entry, index (`${entry.trackId}-${index}`)}
                            <li
                                class="flex items-center gap-1.5 border-b py-1 text-xs last:border-b-0"
                                class:opacity-50={entry.state === "missing"}
                            >
                                <span class="min-w-0 flex-1 truncate">
                                    {entry.title}
                                    <span class="text-muted-foreground">
                                        — {entry.artist ?? "Unknown artist"}
                                    </span>
                                </span>

                                {@render badge(entry)}

                                <span
                                    class="text-muted-foreground shrink-0 tabular-nums"
                                >
                                    {formatDuration(entry.durationSecs)}
                                </span>
                            </li>
                        {/each}
                    </ul>

                    {#if queueStore.contextRemaining > 0}
                        <p class="text-muted-foreground text-[11px]">
                            and {queueStore.contextRemaining} more
                        </p>
                    {/if}
                {/if}

                {#if player.shuffle}
                    <p class="text-muted-foreground text-[11px]">
                        Shuffled — this is the order it will actually play.
                    </p>
                {/if}
            </section>
        </div>
    </aside>
{/if}
