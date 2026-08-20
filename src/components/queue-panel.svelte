<script lang="ts">
    import { queueStore, type QueueEntry } from "$lib/queue.svelte";
    import SourceBadge from "$components/source-badge.svelte";
    import CoverArt from "$components/cover-art.svelte";
    import PlayingBars from "$components/playing-bars.svelte";
    import { coverSeed } from "$lib/cover";
    import { cacheStore } from "$lib/cache.svelte";
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

{#snippet heading(text: string)}
    <h3
        class="text-muted-foreground px-1 text-[10px] font-semibold tracking-[0.08em] uppercase"
    >
        {text}
    </h3>
{/snippet}

<!-- Unavailable tracks stay listed, marked. Hiding them would make the panel
     disagree with what actually plays. -->
{#snippet badge(entry: QueueEntry)}
    <SourceBadge
        source={entry.source}
        state={entry.state}
        durationSecs={entry.durationSecs}
        cached={cacheStore.isCached(entry.trackId)}
        compact
    />
{/snippet}

<!--
  Docked rather than floating.

  The old panel hovered over the track list, which meant reordering the queue
  hid the list you were reordering it against. Taking real width instead makes
  the two readable at once, and the main view simply gets narrower.
-->
<aside
    class="bg-sidebar flex shrink-0 flex-col overflow-hidden transition-[width] duration-200 ease-out
           {queueStore.open ? 'border-border/70 border-l' : ''}"
    style="width: {queueStore.open ? '19rem' : '0px'}"
    aria-label="Play queue"
    aria-hidden={!queueStore.open}
    inert={!queueStore.open}
>
    <div class="flex w-[19rem] min-h-0 flex-1 flex-col">
        <header class="flex shrink-0 items-center justify-between gap-2 px-3 py-3">
            <h2 class="flex items-center gap-2 text-sm font-semibold">
                <ListMusicIcon class="size-4" />
                Up next
            </h2>
            <button
                type="button"
                class="text-muted-foreground hover:bg-accent hover:text-foreground grid size-7 place-items-center rounded-md transition-colors"
                aria-label="Close queue"
                onclick={() => queueStore.toggle()}
            >
                <XIcon class="size-4" />
            </button>
        </header>

        <div class="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto px-2 pb-4">
            {#if queueStore.current}
                <section class="flex flex-col gap-1.5">
                    {@render heading("Now playing")}
                    <div
                        class="bg-accent/60 flex items-center gap-2.5 rounded-lg px-2 py-2"
                    >
                        <CoverArt
                            seed={coverSeed(queueStore.current)}
                            coverKey={queueStore.current.coverKey}
                            src={queueStore.current.remoteThumbnailUrl}
                            class="size-9"
                            glyph={false}
                        />
                        <div class="flex min-w-0 flex-1 flex-col gap-0.5">
                            <span class="truncate text-[13px] leading-tight font-medium">
                                {queueStore.current.title}
                            </span>
                            <span
                                class="text-muted-foreground flex items-center gap-1.5 text-[11px] leading-tight"
                            >
                                <span class="truncate">
                                    {queueStore.current.artist ?? "Unknown artist"}
                                </span>
                                {@render badge(queueStore.current)}
                            </span>
                        </div>
                        <span class="text-primary shrink-0">
                            <PlayingBars animate={player.state === "playing"} />
                        </span>
                    </div>
                </section>
            {/if}

            <!-- The manual queue: what the user explicitly asked for. -->
            <section class="flex flex-col gap-1.5">
                <div class="flex items-center justify-between gap-2">
                    {@render heading("Queue")}
                    {#if queueStore.manual.length > 0}
                        <button
                            type="button"
                            class="text-muted-foreground hover:text-foreground px-1 text-[11px] underline underline-offset-2 transition-colors"
                            onclick={() => queueStore.clear()}
                        >
                            Clear
                        </button>
                    {/if}
                </div>

                {#if queueStore.manual.length === 0}
                    <p class="text-muted-foreground px-1 text-xs leading-relaxed">
                        Nothing queued. Use “Play next” or “Add to queue” on any
                        track and it jumps ahead of whatever is playing next.
                    </p>
                {:else}
                    <ul class="flex flex-col gap-px">
                        {#each queueStore.manual as entry, index (entry.entryId)}
                            <li
                                class="group/entry hover:bg-accent/60 flex items-center gap-1.5 rounded-md px-1.5 py-1.5 transition-colors
                                       {dragOver === index
                                    ? 'bg-accent ring-primary/40 ring-1'
                                    : ''}"
                                class:opacity-50={entry.state === "missing"}
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
                                    class="text-muted-foreground size-3.5 shrink-0 cursor-grab opacity-40 transition-opacity group-hover/entry:opacity-80"
                                />

                                <div class="flex min-w-0 flex-1 flex-col">
                                    <span class="truncate text-xs leading-tight">
                                        {entry.title}
                                    </span>
                                    <span
                                        class="text-muted-foreground flex items-center gap-1.5 text-[11px] leading-tight"
                                    >
                                        <span class="truncate">
                                            {entry.artist ?? "Unknown artist"}
                                        </span>
                                        {@render badge(entry)}
                                    </span>
                                </div>

                                <span
                                    class="text-muted-foreground shrink-0 text-[11px] tabular-nums group-hover/entry:hidden"
                                >
                                    {formatDuration(entry.durationSecs)}
                                </span>

                                <button
                                    type="button"
                                    class="text-muted-foreground hover:bg-background hover:text-foreground hidden size-6 shrink-0 place-items-center rounded-md transition-colors group-hover/entry:grid"
                                    aria-label="Remove {entry.title} from the queue"
                                    onclick={() =>
                                        entry.entryId !== null &&
                                        queueStore.remove(entry.entryId)}
                                >
                                    <XIcon class="size-3.5" />
                                </button>
                            </li>
                        {/each}
                    </ul>
                {/if}
            </section>

            <!-- The context continuation. Read-only: a displayed row maps to a
                 shuffled permutation index, so removing one here is a larger
                 change than it looks. -->
            <section class="flex flex-col gap-1.5">
                {@render heading(contextHeading)}

                {#if queueStore.upNext.length === 0}
                    <p class="text-muted-foreground px-1 text-xs">
                        Nothing follows — the list ends here.
                    </p>
                {:else}
                    <ul class="flex flex-col gap-px">
                        {#each queueStore.upNext as entry, index (`${entry.trackId}-${index}`)}
                            <li
                                class="flex items-center gap-1.5 rounded-md px-1.5 py-1"
                                class:opacity-50={entry.state === "missing"}
                            >
                                <div class="flex min-w-0 flex-1 flex-col">
                                    <span class="truncate text-xs leading-tight">
                                        {entry.title}
                                    </span>
                                    <span
                                        class="text-muted-foreground flex items-center gap-1.5 text-[11px] leading-tight"
                                    >
                                        <span class="truncate">
                                            {entry.artist ?? "Unknown artist"}
                                        </span>
                                        {@render badge(entry)}
                                    </span>
                                </div>
                                <span
                                    class="text-muted-foreground shrink-0 text-[11px] tabular-nums"
                                >
                                    {formatDuration(entry.durationSecs)}
                                </span>
                            </li>
                        {/each}
                    </ul>

                    {#if queueStore.contextRemaining > 0}
                        <p class="text-muted-foreground px-1 text-[11px]">
                            and {queueStore.contextRemaining} more
                        </p>
                    {/if}
                {/if}

                {#if player.shuffle}
                    <p class="text-muted-foreground px-1 text-[11px]">
                        Shuffled — this is the order it will actually play.
                    </p>
                {/if}
            </section>
        </div>
    </div>
</aside>
