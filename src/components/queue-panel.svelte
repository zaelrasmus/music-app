<script lang="ts">
    import { queueStore, type QueueEntry } from "$lib/queue.svelte";
    import QueueRow from "$components/queue-row.svelte";
    import VirtualList from "$components/virtual-list.svelte";
    import { player } from "$lib/player.svelte";
    import XIcon from "@lucide/svelte/icons/x";
    import ListRestartIcon from "@lucide/svelte/icons/list-restart";
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

    const contextHeading = $derived(
        queueStore.contextName
            ? `Next from ${queueStore.contextName}`
            : "Next up",
    );

    /**
     * One flat list, because there is one scroller.
     *
     * The panel reads as three sections, but virtualising three lists inside a
     * single scrolling element means telling each one how far down the content
     * it begins — an offset that has to be re-measured every time anything
     * above it changes height, and that is silently wrong when it is not.
     *
     * Flattening the sections into one sequence of typed rows removes the
     * question entirely: one virtualizer, one coordinate space, and the
     * headings scroll with the rows they head because they *are* rows.
     */
    type PanelRow =
        | { kind: "heading"; key: string; text: string; clearable?: boolean }
        | { kind: "note"; key: string; text: string }
        | { kind: "entry"; key: string; entry: QueueEntry; current: true }
        | {
              kind: "entry";
              key: string;
              entry: QueueEntry;
              current?: false;
              /** Where the click goes, and which list the row belongs to. */
              play: () => void;
              /** Manual rows only — the queue is the reorderable tier. */
              index?: number;
          };

    const rows = $derived.by(() => {
        const built: PanelRow[] = [];

        if (queueStore.current) {
            built.push({ kind: "heading", key: "h-now", text: "Now playing" });
            built.push({
                kind: "entry",
                key: "now",
                entry: queueStore.current,
                current: true,
            });
        }

        built.push({
            kind: "heading",
            key: "h-queue",
            text: player.loopQueue ? "Queue · on a loop" : "Queue",
            clearable: queueStore.manual.length > 0,
        });

        if (queueStore.manual.length === 0) {
            built.push({
                kind: "note",
                key: "queue-empty",
                text: "Nothing queued. Use “Play next” or “Add to queue” on any track and it jumps ahead of whatever is playing next.",
            });
        } else {
            queueStore.manual.forEach((entry, index) => {
                built.push({
                    kind: "entry",
                    key: `m-${entry.entryId}`,
                    entry,
                    index,
                    play: () =>
                        entry.entryId !== null &&
                        queueStore.playEntry(entry.entryId),
                });
            });
        }

        built.push({ kind: "heading", key: "h-next", text: contextHeading });

        if (queueStore.upNext.length === 0) {
            built.push({
                kind: "note",
                key: "next-empty",
                text: "Nothing follows — the list ends here.",
            });
        } else {
            queueStore.upNext.forEach((entry, index) => {
                built.push({
                    kind: "entry",
                    key: `u-${entry.trackId}-${index}`,
                    entry,
                    play: () => queueStore.playUpcoming(index),
                });
            });

            if (queueStore.contextRemaining > 0) {
                built.push({
                    kind: "note",
                    key: "more",
                    text: `and ${queueStore.contextRemaining} more`,
                });
            }
        }

        if (player.shuffle) {
            built.push({
                kind: "note",
                key: "shuffled",
                text: "Shuffled — this is the order it will actually play.",
            });
        }

        return built;
    });

    /** Rough, and corrected by measurement the moment a row is drawn. */
    function estimate(index: number) {
        const row = rows[index];
        if (row?.kind === "heading") return 28;
        if (row?.kind === "note") return 34;
        return 48;
    }

    /** The panel's own scroller, since it is not inside a `PageShell`. */
    let scroller = $state<HTMLElement>();
</script>

{#snippet heading(text: string)}
    <h3
        class="text-muted-foreground px-1 text-[10px] font-semibold tracking-[0.08em] uppercase"
    >
        {text}
    </h3>
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
            <div class="flex shrink-0 items-center gap-0.5">
                <!--
                  Loop the queue, not the playlist.

                  Repeat lives on the player bar and acts on the context -- a
                  whole album or library view. This acts on the handful of
                  tracks picked out by hand, which is why it lives here, beside
                  them, rather than as a fourth repeat mode meaning something
                  different from the other three.

                  Shown only when there is a queue to loop: an unlit control for
                  an empty list is a question nobody asked.
                -->
                {#if queueStore.manual.length > 0}
                    <!--
                      Its own icon, not the transport's repeat glyph.
                      
                      The two controls used the same one, which is most of why
                      they were confusing: they look like the same promise and
                      act on different things. This loops the handful of tracks
                      listed below it; Repeat in the player bar loops the
                      playlist they were queued from.
                    -->
                    <button
                        type="button"
                        class="grid size-7 place-items-center rounded-md transition-colors {player.loopQueue
                            ? 'text-primary bg-accent'
                            : 'text-muted-foreground hover:bg-accent hover:text-foreground'}"
                        aria-label={player.loopQueue
                            ? "Stop looping the queued tracks"
                            : "Loop the queued tracks"}
                        title={player.loopQueue
                            ? "Looping these queued tracks — the playlist is paused behind them"
                            : "Loop these queued tracks only. To repeat the playlist, use Repeat in the player bar."}
                        aria-pressed={player.loopQueue}
                        onclick={() => player.toggleLoopQueue()}
                    >
                        <ListRestartIcon class="size-4" />
                    </button>
                {/if}

                <button
                    type="button"
                    class="text-muted-foreground hover:bg-accent hover:text-foreground grid size-7 place-items-center rounded-md transition-colors"
                    aria-label="Close queue"
                    onclick={() => queueStore.toggle()}
                >
                    <XIcon class="size-4" />
                </button>
            </div>
        </header>

        <div
            bind:this={scroller}
            class="min-h-0 flex-1 overflow-y-auto px-2 pb-4"
        >
            <VirtualList
                {rows}
                estimateSize={estimate}
                scrollElement={scroller}
                key={(item) => item.key}
            >
                {#snippet row(item)}
                    {#if item.kind === "heading"}
                        <div
                            class="flex items-center justify-between gap-2 pt-3 pb-1"
                        >
                            {@render heading(item.text)}
                            {#if item.clearable}
                                <button
                                    type="button"
                                    class="text-muted-foreground hover:text-foreground px-1 text-[11px] underline underline-offset-2 transition-colors"
                                    onclick={() => queueStore.clear()}
                                >
                                    Clear
                                </button>
                            {/if}
                        </div>
                    {:else if item.kind === "note"}
                        <p class="text-muted-foreground px-1 pb-1 text-[11px] leading-relaxed">
                            {item.text}
                        </p>
                    {:else if item.current}
                        <QueueRow entry={item.entry} current />
                    {:else if item.index !== undefined}
                        <!--
                          A queued row: reorderable, removable, and the only
                          tier where dragging means anything. Drag handlers sit
                          on the wrapper rather than inside `QueueRow`, because
                          being draggable is a fact about this list, not about
                          what a queued track looks like.
                        -->
                        <div
                            class="rounded-md {dragOver === item.index
                                ? 'bg-accent ring-primary/40 ring-1'
                                : ''}"
                            draggable="true"
                            ondragstart={(e) => {
                                // A drag with no payload is refused by every
                                // drop target. See `track-row.svelte`.
                                e.dataTransfer?.setData("text/plain", "row");
                                if (e.dataTransfer)
                                    e.dataTransfer.effectAllowed = "move";
                                dragFrom = item.index ?? null;
                            }}
                            ondragover={(e) => {
                                e.preventDefault();
                                if (e.dataTransfer)
                                    e.dataTransfer.dropEffect = "move";
                                dragOver = item.index ?? null;
                            }}
                            ondragleave={() => {
                                if (dragOver === item.index) dragOver = null;
                            }}
                            ondrop={(e) => {
                                e.preventDefault();
                                if (item.index !== undefined) drop(item.index);
                            }}
                            ondragend={() => {
                                dragFrom = null;
                                dragOver = null;
                            }}
                        >
                            <QueueRow entry={item.entry} onplay={item.play}>
                                {#snippet leading()}
                                    <GripVerticalIcon
                                        class="text-muted-foreground size-3.5 cursor-grab opacity-40 transition-opacity group-hover/queue-row:opacity-80"
                                    />
                                {/snippet}

                                {#snippet trailing()}
                                    <button
                                        type="button"
                                        class="text-muted-foreground hover:bg-background hover:text-foreground hidden size-6 place-items-center rounded-md transition-colors group-hover/queue-row:grid"
                                        aria-label="Remove {item.entry
                                            .title} from the queue"
                                        onclick={() =>
                                            item.entry.entryId !== null &&
                                            queueStore.remove(
                                                item.entry.entryId,
                                            )}
                                    >
                                        <XIcon class="size-3.5" />
                                    </button>
                                {/snippet}
                            </QueueRow>
                        </div>
                    {:else}
                        <QueueRow entry={item.entry} onplay={item.play} />
                    {/if}
                {/snippet}
            </VirtualList>
        </div>
    </div>
</aside>
