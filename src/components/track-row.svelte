<script lang="ts">
    import { Input } from "$components/ui/input";
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
    import PlayIcon from "@lucide/svelte/icons/play";
    import PauseIcon from "@lucide/svelte/icons/pause";
    import CheckIcon from "@lucide/svelte/icons/check";
    import XIcon from "@lucide/svelte/icons/x";
    import GripVerticalIcon from "@lucide/svelte/icons/grip-vertical";
    import type { Snippet } from "svelte";

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
        showIndex?: boolean;
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
        showIndex = true,
        reorder,
        extra,
    }: Props = $props();

    const isCurrent = $derived(player.isCurrent(track.id));
    const isPlaying = $derived(isCurrent && player.state === "playing");
    const tags = $derived(tagStore.forTrack(track.id));
    const missing = $derived(track.state === "missing");

    /** Three fit before the duration column starts losing to them. */
    const shownTags = $derived(tags.slice(0, 3));
    const hiddenTags = $derived(tags.slice(3));

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

    function formatDuration(secs: number | null) {
        if (secs === null) return "—";
        const m = Math.floor(secs / 60);
        const s = secs % 60;
        return `${m}:${String(s).padStart(2, "0")}`;
    }
</script>

<li
    class="group/row relative rounded-md transition-colors
           {reorder?.over ? 'bg-accent ring-primary/40 ring-1' : ''}
           {isCurrent ? 'bg-accent/50' : 'hover:bg-accent/50'}"
    class:opacity-55={missing}
    draggable={reorder?.enabled ? "true" : "false"}
    ondragstart={() => reorder?.onStart()}
    ondragover={(e) => {
        if (!reorder?.enabled) return;
        e.preventDefault();
        reorder.onOver();
    }}
    ondragleave={() => reorder?.onLeave()}
    ondrop={(e) => {
        if (!reorder?.enabled) return;
        e.preventDefault();
        reorder.onDrop();
    }}
    ondragend={() => reorder?.onEnd()}
    ondblclick={play}
>
    <div class="flex items-center gap-3 px-2 py-1.5">
        {#if reorder}
            <GripVerticalIcon
                class="text-muted-foreground size-4 shrink-0 {reorder.enabled
                    ? 'cursor-grab opacity-0 transition-opacity group-hover/row:opacity-70'
                    : 'opacity-20'}"
            />
        {/if}

        {#if showIndex}
            <!--
              One 28px cell holding three mutually exclusive things: the
              position, the play control, and the playing indicator. Keeping
              them in one place is what lets the row stay a single line while
              still having a real play target.
            -->
            <div class="relative size-7 shrink-0">
                <span
                    class="text-muted-foreground absolute inset-0 grid place-items-center text-xs tabular-nums transition-opacity
                           {isCurrent ? 'opacity-0' : 'group-hover/row:opacity-0'}"
                    aria-hidden="true"
                >
                    {index + 1}
                </span>

                {#if isCurrent && !isPlaying}
                    <!-- Paused: the bars stay, frozen. -->
                    <span
                        class="text-primary absolute inset-0 grid place-items-center group-hover/row:opacity-0"
                        aria-hidden="true"
                    >
                        <PlayingBars animate={false} />
                    </span>
                {:else if isCurrent}
                    <span
                        class="text-primary absolute inset-0 grid place-items-center group-hover/row:opacity-0"
                        aria-hidden="true"
                    >
                        <PlayingBars />
                    </span>
                {/if}

                <button
                    type="button"
                    class="text-foreground absolute inset-0 grid place-items-center rounded-md opacity-0 transition-opacity group-hover/row:opacity-100 focus-visible:opacity-100 focus-visible:outline-none"
                    aria-label={isPlaying ? `Pause ${track.title}` : `Play ${track.title}`}
                    onclick={play}
                >
                    {#if isPlaying}
                        <PauseIcon class="size-4" />
                    {:else}
                        <PlayIcon class="size-4 fill-current" />
                    {/if}
                </button>
            </div>
        {/if}

        <CoverArt seed={coverSeed(track)} class="size-10" />

        {#if editing}
            <div class="flex min-w-0 flex-1 items-center gap-2">
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
            <div class="flex min-w-0 flex-1 flex-col gap-0.5">
                <span
                    class="selectable truncate text-sm leading-tight {isCurrent
                        ? 'text-primary font-medium'
                        : ''}"
                    title={track.title}
                >
                    {track.title}
                </span>
                <span class="text-muted-foreground flex items-center gap-1.5 text-xs leading-tight">
                    <span class="selectable truncate">
                        {track.artist ?? "Unknown artist"}
                    </span>
                    <SourceBadge
                        source={track.source}
                        state={track.state}
                        durationSecs={track.durationSecs}
                        cached={cacheStore.isCached(track.id)}
                    />
                </span>
            </div>

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
                trigger="opacity-0 transition-opacity group-hover/row:opacity-100 focus-visible:opacity-100 data-open:opacity-100"
            />
        {/if}
    </div>

    {#if tagging}
        <!-- Sits under the row rather than replacing part of it, so nothing
             the user was reading moves while they type. -->
        <div class="flex items-center gap-2 px-2 pb-2 pl-14">
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
</li>
