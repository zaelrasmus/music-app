<script lang="ts">
    import { Input } from "$components/ui/input";
    import { writeSetting } from "$lib/settings.svelte";
    import { Button } from "$components/ui/button";
    import TrackMenu from "$components/track-menu.svelte";
    import SourceBadge from "$components/source-badge.svelte";
    import TagChip from "$components/tag-chip.svelte";
    import CoverArt from "$components/cover-art.svelte";
    import PlayingBars from "$components/playing-bars.svelte";
    import { coverSeed } from "$lib/cover";
    import { cacheStore } from "$lib/cache.svelte";
    import { player } from "$lib/player.svelte";
    import { trackStore, type Track } from "$lib/tracks.svelte";
    import { tagStore } from "$lib/tags.svelte";
    import { libraryView } from "$lib/library-view.svelte";
    import { historyStore } from "$lib/history.svelte";
    import PlayIcon from "@lucide/svelte/icons/play";
    import PauseIcon from "@lucide/svelte/icons/pause";
    import CheckIcon from "@lucide/svelte/icons/check";
    import XIcon from "@lucide/svelte/icons/x";
    import GripVerticalIcon from "@lucide/svelte/icons/grip-vertical";
    import type { Snippet } from "svelte";

    /**
     * One track.
     *
     * The anatomy is the demo's -- artwork, title, duration -- with four
     * changes, all of which came out of reviewing it:
     *
     *  - No clock icon before the duration. Fifty-six identical glyphs saying
     *    what the `m:ss` format already says.
     *  - Every row has two lines, always. The demo showed an artist line only
     *    on rows that had one, so titles did not share a baseline down the list
     *    and the eye had to re-find the text on every row.
     *  - The artwork is the play control. It was decorative in the demo, taking
     *    width and row height and returning nothing; putting the control where
     *    the eye already is means the row needs no separate play column.
     *  - The playing row is marked. The demo had a track playing, in the list,
     *    unmarked -- the one piece of state a list like this has to show.
     */

    /** How a row participates in drag-to-reorder, when its list supports it. */
    export type Reorder = {
        /** False while filtered, when a display index is not a stored one. */
        enabled: boolean;
        over: boolean;
        onStart: () => void;
        onOver: () => void;
        onLeave: () => void;
        onDrop: () => void;
        onEnd: () => void;
    };

    interface Props {
        track: Track;
        /** Ids forming the context when this row is played, in display order. */
        queueIds?: number[];
        index: number;
        /** Shown as "Next from …" in the queue panel. */
        contextName?: string;
        /** Overrides the default "play this list from here". */
        onPlay?: () => void;
        reorder?: Reorder;
        /** Extra menu items for this list, e.g. "Remove from playlist". */
        extra?: Snippet;
    }

    let {
        track,
        queueIds = [],
        index,
        contextName = "your library",
        onPlay,
        reorder,
        extra,
    }: Props = $props();

    const isCurrent = $derived(player.isCurrent(track.id));
    const isPlaying = $derived(isCurrent && player.state === "playing");
    const tags = $derived(tagStore.forTrack(track.id));
    const missing = $derived(track.state === "missing");

    /** Two fit before the duration column starts losing to them. */
    const shownTags = $derived(tags.slice(0, 2));
    const hiddenTags = $derived(tags.slice(2));

    // TEMPORARY, with the window listeners below.
    let probed: string[] = [];
    let probeTimer: ReturnType<typeof setTimeout> | null = null;

    function dragProbe(kind: string, event: DragEvent) {
        const target = event.target as HTMLElement | null;
        const draggableAncestor = target?.closest?.("[draggable='true']");

        probed.push(
            [
                kind,
                `target=${target?.tagName ?? "?"}`,
                `role=${target?.getAttribute?.("role") ?? "-"}`,
                `draggableAncestor=${draggableAncestor ? "yes" : "NO"}`,
                `defaultPrevented=${event.defaultPrevented}`,
                `types=${event.dataTransfer?.types?.join("|") || "none"}`,
                `effect=${event.dataTransfer?.dropEffect ?? "-"}`,
            ].join(" "),
        );

        // Batched: `dragover` fires continuously, and one write per event
        // would hammer the store and drown the interesting lines.
        if (probeTimer) clearTimeout(probeTimer);
        probeTimer = setTimeout(() => {
            const seen = probed.slice(0, 4).concat(probed.slice(-4));
            probed = [];
            void writeSetting("__dragProbe", seen);
        }, 400);
    }

    /** The row element, used as the drag preview when the handle is grabbed. */
    let rowElement = $state<HTMLDivElement | null>(null);

    let editing = $state(false);
    let editTitle = $state("");
    let editArtist = $state("");

    let tagging = $state(false);
    let newTag = $state("");
    let tagInput = $state<HTMLInputElement | null>(null);

    function play() {
        if (isCurrent) {
            void player.togglePlayPause();
        } else if (onPlay) {
            onPlay();
        } else {
            void player.playQueue(queueIds, index, contextName);
        }
    }

    function startEdit() {
        editTitle = track.title;
        editArtist = track.artist ?? "";
        editing = true;
    }

    async function saveEdit() {
        // An empty artist means "unknown", stored as NULL.
        const artist = editArtist.trim() === "" ? null : editArtist;
        if (await trackStore.updateMetadata(track.id, editTitle, artist)) {
            editing = false;
        }
    }

    function startTagging() {
        tagging = true;
        // The menu that opened this is still closing and will pull focus back
        // to its trigger; the next frame is after that.
        requestAnimationFrame(() => tagInput?.focus());
    }

    async function commitTag() {
        if (newTag.trim() === "") {
            tagging = false;
            return;
        }
        await tagStore.assign(track.id, newTag);
        newTag = "";
        tagging = false;
    }

    /**
     * Reloads the two lists that library membership changes the contents of.
     *
     * Done here rather than pushed onto every call site: a row appears in four
     * different lists and all of them would need the same three lines. History
     * is included because the menu's verb ("Add" vs "Remove") is read from the
     * row, so a stale row would offer to add something twice.
     */
    function refreshLists() {
        void libraryView.refresh();
        void historyStore.load();
    }

    function formatDuration(secs: number | null) {
        if (secs === null) return "—";
        const m = Math.floor(secs / 60);
        const s = secs % 60;
        return `${m}:${String(s).padStart(2, "0")}`;
    }
</script>

<!--
  A plain element, not an `<li>`.

  The list belongs to whatever is listing: a virtualised list has to own the
  positioned wrapper itself, and an `<li>` inside an `<li>` is not valid HTML.
  Every caller supplies the list element and its item, which is where that
  decision belongs anyway — this component knows what a track looks like, not
  what it is one of.
-->
<!--
  TEMPORARY: drag diagnostics.

  Two fixes have not landed, and guessing a third is worse than measuring. A
  `dragstart` that never fires and one that fires and is then refused look
  identical from outside, so this records which actually happens -- into the
  settings store, because the webview console is not readable from outside the
  window. Remove once the cause is known.
-->
<svelte:window
    ondragstart={(e) => dragProbe("dragstart", e)}
    ondragover={(e) => dragProbe("dragover", e)}
    ondrop={(e) => dragProbe("drop", e)}
    ondragend={(e) => dragProbe("dragend", e)}
/>

<div
    bind:this={rowElement}
    class="group/row has-[:focus-visible]:ring-ring relative rounded-lg transition-colors has-[:focus-visible]:ring-2
           {reorder?.over ? 'bg-accent ring-foreground/25 ring-1' : ''}
           {isCurrent ? 'bg-foreground/[0.055]' : 'hover:bg-foreground/[0.04]'}"
    class:opacity-50={missing}
    draggable={reorder?.enabled ? "true" : "false"}
    ondragstart={(e) => {
        if (!reorder?.enabled) return;
        // The three lines that make a drag a *move*.
        //
        // Without a payload the drag carries nothing, and a drag carrying
        // nothing is refused by every drop target -- which is the prohibited
        // cursor, with no `drop` event ever firing. The value is unused; that
        // there *is* one is the whole point. `effectAllowed` then has to agree
        // with the `dropEffect` set below, or the browser refuses on the
        // mismatch instead.
        e.dataTransfer?.setData("text/plain", "row");
        if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
        reorder.onStart();
    }}
    ondragover={(e) => {
        if (!reorder) return;
        // Unconditionally, and *before* any other test. Not calling this is
        // how a target says "no", so a guarded version made a row that was
        // merely not reorderable look broken instead of inert.
        e.preventDefault();
        if (!reorder.enabled) return;
        if (e.dataTransfer) e.dataTransfer.dropEffect = "move";
        reorder.onOver();
    }}
    ondragleave={() => reorder?.onLeave()}
    ondrop={(e) => {
        if (!reorder) return;
        e.preventDefault();
        if (reorder.enabled) reorder.onDrop();
    }}
    ondragend={() => reorder?.onEnd()}
>
    <div class="flex items-center gap-3 pr-2 pl-3">
        {#if reorder}
            <!--
              The drag starts here, not on the row.

              Most of a row is a `<button>` -- the whole title block is the play
              control -- and a drag gesture beginning inside a form control does
              not start the draggable ancestor's drag. It is refused instead,
              which is the prohibited cursor with no `drop` ever firing. Since
              that is exactly where anyone would grab a song, dragging the row
              looked broken while nothing was wrong with the handlers.

              A dedicated handle is the conventional answer and the robust one:
              it is never inside a control, and it says where to grab.
            -->
            <span
                role="application"
                aria-label="Reorder {track.title}"
                draggable={reorder.enabled ? "true" : "false"}
                class="-ml-1.5 shrink-0 {reorder.enabled
                    ? 'cursor-grab opacity-40 transition-opacity group-hover/row:opacity-80'
                    : 'opacity-20'}"
                ondragstart={(e) => {
                    if (!reorder?.enabled) return;
                    e.dataTransfer?.setData("text/plain", "row");
                    if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
                    // The whole row as the preview, rather than a lone grip.
                    if (e.dataTransfer && rowElement) {
                        e.dataTransfer.setDragImage(rowElement, 24, 20);
                    }
                    reorder.onStart();
                }}
                ondragend={() => reorder?.onEnd()}
            >
                <GripVerticalIcon class="text-muted-foreground size-4" />
            </span>
        {/if}

        {#if editing}
            <div class="flex min-w-0 flex-1 items-center gap-2 py-2">
                <Input
                    bind:value={editTitle}
                    placeholder="Title"
                    class="h-8"
                    onkeydown={(e) => {
                        if (e.key === "Enter") saveEdit();
                        if (e.key === "Escape") editing = false;
                    }}
                />
                <Input
                    bind:value={editArtist}
                    placeholder="Artist"
                    class="h-8"
                    onkeydown={(e) => {
                        if (e.key === "Enter") saveEdit();
                        if (e.key === "Escape") editing = false;
                    }}
                />
                <Button variant="ghost" size="icon" aria-label="Save" onclick={saveEdit}>
                    <CheckIcon />
                </Button>
                <Button
                    variant="ghost"
                    size="icon"
                    aria-label="Cancel"
                    onclick={() => (editing = false)}
                >
                    <XIcon />
                </Button>
            </div>
        {:else}
            <!--
              The whole title block is the play control. A real button, so it
              is reachable by keyboard and announces itself, rather than a
              click handler on the row that a screen reader never finds.
            -->
            <button
                type="button"
                class="flex min-w-0 flex-1 items-center gap-3 py-2 text-left focus-visible:outline-none"
                aria-label={isPlaying ? `Pause ${track.title}` : `Play ${track.title}`}
                onclick={play}
            >
                <span class="relative shrink-0">
                    <CoverArt
                        seed={coverSeed(track)}
                        coverKey={track.coverKey}
                        src={track.remoteThumbnailUrl}
                        class="size-10"
                        glyph={false}
                    />

                    {#if isCurrent}
                        <span
                            class="absolute inset-0 grid place-items-center rounded-md bg-black/55 text-white"
                            aria-hidden="true"
                        >
                            <PlayingBars animate={isPlaying} />
                        </span>
                    {:else}
                        <span
                            class="absolute inset-0 grid place-items-center rounded-md bg-black/55 text-white opacity-0 transition-opacity group-hover/row:opacity-100 group-has-[:focus-visible]/row:opacity-100"
                            aria-hidden="true"
                        >
                            <PlayIcon class="size-4 fill-current" />
                        </span>
                    {/if}
                </span>

                <span class="flex min-w-0 flex-1 flex-col gap-[3px]">
                    <span
                        class="truncate text-[13px] leading-tight {isCurrent
                            ? 'font-medium'
                            : ''}"
                        title={track.title}
                    >
                        {track.title}
                    </span>
                    <!-- Always present, even when there is no artist. A row
                         that sometimes has one line and sometimes two makes
                         the titles above them stop lining up. -->
                    <span
                        class="text-muted-foreground flex items-center gap-1.5 text-[11px] leading-tight"
                    >
                        <span class="truncate">
                            {track.artist ?? "Unknown artist"}
                        </span>
                        <SourceBadge
                            source={track.source}
                            state={track.state}
                            durationSecs={track.durationSecs}
                            cached={cacheStore.isCached(track.id)}
                        />
                    </span>
                </span>
            </button>

            {#if tags.length > 0}
                <div class="hidden shrink-0 items-center gap-1 sm:flex">
                    {#each shownTags as tag (tag.id)}
                        <TagChip id={tag.id} name={tag.name} color={tag.color} />
                    {/each}
                    {#if hiddenTags.length > 0}
                        <span
                            class="text-muted-foreground text-[11px]"
                            title={hiddenTags.map((t) => t.name).join(", ")}
                        >
                            +{hiddenTags.length}
                        </span>
                    {/if}
                </div>
            {/if}

            <span
                class="text-muted-foreground w-11 shrink-0 text-right text-xs tabular-nums"
            >
                {formatDuration(track.durationSecs)}
            </span>

            <TrackMenu
                resolveTrackId={async () => track.id}
                label="More actions for {track.title}"
                {track}
                {extra}
                onEdit={startEdit}
                onTag={startTagging}
                onLibraryChange={refreshLists}
                trigger="opacity-0 transition-opacity group-hover/row:opacity-100 focus-visible:opacity-100 data-open:opacity-100"
            />
        {/if}
    </div>

    {#if tagging}
        <!-- Sits under the row rather than replacing part of it, so nothing
             the user was reading moves while they type. -->
        <div class="flex flex-wrap items-center gap-2 px-3 pb-2 pl-16">
            <Input
                bind:ref={tagInput}
                bind:value={newTag}
                placeholder="Tag name, then Enter"
                class="h-7 w-48 text-xs"
                aria-label="New tag for {track.title}"
                onblur={() => {
                    if (newTag.trim() === "") tagging = false;
                }}
                onkeydown={(e) => {
                    if (e.key === "Enter") commitTag();
                    if (e.key === "Escape") {
                        tagging = false;
                        newTag = "";
                    }
                }}
            />
            {#each tags as tag (tag.id)}
                <TagChip
                    id={tag.id}
                    name={tag.name}
                    color={tag.color}
                    onremove={() => tagStore.remove(track.id, tag.id)}
                />
            {/each}
        </div>
    {/if}
</div>
