<script lang="ts">
    import { Button } from "$components/ui/button";
    import { Input } from "$components/ui/input";
    import { playlistStore } from "$lib/playlists.svelte";
    import { player } from "$lib/player.svelte";
    import PlayIcon from "@lucide/svelte/icons/play";
    import PauseIcon from "@lucide/svelte/icons/pause";
    import PlusIcon from "@lucide/svelte/icons/plus";
    import Trash2Icon from "@lucide/svelte/icons/trash-2";
    import PencilIcon from "@lucide/svelte/icons/pencil";
    import CheckIcon from "@lucide/svelte/icons/check";
    import XIcon from "@lucide/svelte/icons/x";
    import ChevronLeftIcon from "@lucide/svelte/icons/chevron-left";
    import GripVerticalIcon from "@lucide/svelte/icons/grip-vertical";
    import SearchIcon from "@lucide/svelte/icons/search";
    import TagFilter from "$components/tag-filter.svelte";
    import TrackActions from "$components/track-actions.svelte";

    let newName = $state("");
    let renaming = $state(false);
    let renameValue = $state("");

    /** Index currently being dragged, and the index it is hovering over. */
    let dragFrom = $state<number | null>(null);
    let dragOver = $state<number | null>(null);

    const detail = $derived(playlistStore.open);

    async function create() {
        if (newName.trim() === "") return;
        await playlistStore.create(newName);
        newName = "";
    }

    function startRename() {
        if (!detail) return;
        renameValue = detail.playlist.name;
        renaming = true;
    }

    async function commitRename() {
        if (!detail) return;
        if (await playlistStore.rename(detail.playlist.id, renameValue)) {
            renaming = false;
        }
    }

    /**
     * Reordering is only meaningful on the unfiltered list.
     *
     * A displayed index equals a stored position only when every track is
     * shown. Filter the list and index 2 might be position 17, so a drop would
     * move the track somewhere the user did not point at.
     */
    const reorderable = $derived(!playlistStore.filtering);

    async function drop(toIndex: number) {
        const from = dragFrom;
        dragFrom = null;
        dragOver = null;

        if (!detail || !reorderable || from === null || from === toIndex) return;
        // Unfiltered and positions are dense, so a list index is the position.
        await playlistStore.reorder(
            detail.playlist.id,
            detail.tracks[from].id,
            toIndex,
        );
    }

    function formatDuration(secs: number | null) {
        if (secs === null) return "";
        const m = Math.floor(secs / 60);
        const s = secs % 60;
        return `${m}:${String(s).padStart(2, "0")}`;
    }

    /** Tracks that cannot play right now still belong in the list, marked. */
    function unavailable(state: string) {
        return state === "missing";
    }
</script>

<section class="flex flex-col gap-3">
    {#if !detail}
        <div class="flex flex-col gap-1">
            <h2 class="text-lg font-semibold">Playlists</h2>
            <p class="text-muted-foreground text-sm">
                Local files and YouTube tracks in the same list.
            </p>
        </div>

        <div class="flex items-center gap-2">
            <Input
                bind:value={newName}
                placeholder="New playlist name"
                class="h-9"
                onkeydown={(e) => {
                    if (e.key === "Enter") create();
                }}
            />
            <Button onclick={create} disabled={newName.trim() === ""}>
                <PlusIcon data-icon="inline-start" />
                Create
            </Button>
        </div>

        {#if playlistStore.playlists.length === 0}
            <div
                class="text-muted-foreground rounded-lg border border-dashed px-6 py-8 text-center text-sm"
            >
                No playlists yet.
            </div>
        {:else}
            <ul class="flex flex-col gap-2">
                {#each playlistStore.playlists as playlist (playlist.id)}
                    <li
                        class="bg-card flex items-center justify-between gap-3 rounded-lg border px-3 py-2"
                    >
                        <button
                            type="button"
                            class="flex min-w-0 flex-1 flex-col items-start text-left"
                            onclick={() => playlistStore.openPlaylist(playlist.id)}
                        >
                            <span class="truncate text-sm font-medium">
                                {playlist.name}
                            </span>
                            <span class="text-muted-foreground text-xs">
                                {playlist.trackCount}
                                {playlist.trackCount === 1 ? "track" : "tracks"}
                            </span>
                        </button>
                        <Button
                            variant="ghost"
                            size="icon"
                            aria-label="Delete {playlist.name}"
                            onclick={() => playlistStore.remove(playlist.id)}
                        >
                            <Trash2Icon />
                        </Button>
                    </li>
                {/each}
            </ul>
        {/if}
    {:else}
        <!-- Detail view -->
        <div class="flex items-center gap-2">
            <Button
                variant="ghost"
                size="icon"
                aria-label="Back to playlists"
                onclick={() => playlistStore.close()}
            >
                <ChevronLeftIcon />
            </Button>

            {#if renaming}
                <Input
                    bind:value={renameValue}
                    class="h-9"
                    onkeydown={(e) => {
                        if (e.key === "Enter") commitRename();
                        if (e.key === "Escape") renaming = false;
                    }}
                />
                <Button variant="ghost" size="icon" aria-label="Save name" onclick={commitRename}>
                    <CheckIcon />
                </Button>
                <Button
                    variant="ghost"
                    size="icon"
                    aria-label="Cancel rename"
                    onclick={() => (renaming = false)}
                >
                    <XIcon />
                </Button>
            {:else}
                <h2 class="min-w-0 flex-1 truncate text-lg font-semibold">
                    {detail.playlist.name}
                </h2>
                <Button variant="ghost" size="icon" aria-label="Rename" onclick={startRename}>
                    <PencilIcon />
                </Button>
                <Button
                    disabled={detail.tracks.length === 0}
                    onclick={() => playlistStore.play(0)}
                >
                    <PlayIcon data-icon="inline-start" />
                    Play
                </Button>
            {/if}
        </div>

        <!-- Filters this playlist only; the library keeps its own. -->
        <div class="relative">
            <SearchIcon
                class="text-muted-foreground pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2"
            />
            <Input
                value={playlistStore.query}
                placeholder="Filter this playlist…"
                class="pl-9"
                oninput={(e) => playlistStore.setQuery(e.currentTarget.value)}
            />
        </div>

        <TagFilter
            selectedIds={playlistStore.selectedTagIds}
            mode={playlistStore.mode}
            active={playlistStore.filtering}
            onToggle={(id) => playlistStore.toggleTag(id)}
            onModeChange={(m) => playlistStore.setMode(m)}
            onClear={() => playlistStore.clearFilters()}
            showCounts={false}
        />

        {#if playlistStore.filtering}
            <p class="text-muted-foreground text-xs">
                Showing {detail.tracks.length} of {detail.playlist.trackCount}.
                Play uses the filtered list; reordering is off while filtered.
            </p>
        {/if}

        {#if detail.tracks.length === 0}
            <div
                class="text-muted-foreground rounded-lg border border-dashed px-6 py-8 text-center text-sm"
            >
                {playlistStore.filtering
                    ? "Nothing in this playlist matches."
                    : "Empty. Add tracks from the library or a YouTube search."}
            </div>
        {:else}
            <ul class="flex flex-col">
                {#each detail.tracks as track, index (track.id)}
                    {@const isCurrent = player.isCurrent(track.id)}
                    {@const isPlaying = isCurrent && player.state === "playing"}
                    <li
                        class="flex items-center gap-2 border-b py-1 text-sm last:border-b-0"
                        class:opacity-50={unavailable(track.state)}
                        class:bg-muted={dragOver === index}
                        draggable={reorderable}
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
                            class="text-muted-foreground size-4 shrink-0 {reorderable
                                ? 'cursor-grab'
                                : 'opacity-30'}"
                        />

                        <Button
                            variant="ghost"
                            size="icon"
                            aria-label={isPlaying
                                ? `Pause ${track.title}`
                                : `Play ${track.title} from here`}
                            onclick={() =>
                                isCurrent
                                    ? player.togglePlayPause()
                                    : playlistStore.play(index)}
                        >
                            {#if isPlaying}
                                <PauseIcon />
                            {:else}
                                <PlayIcon />
                            {/if}
                        </Button>

                        <span class="min-w-0 flex-1 truncate">
                            <span class:font-medium={isCurrent}>{track.title}</span>
                            <span class="text-muted-foreground">
                                — {track.artist ?? "Unknown artist"}
                            </span>
                            {#if track.state === "missing"}
                                <span class="text-muted-foreground text-xs">(missing)</span>
                            {:else if track.state === "saved"}
                                <span
                                    class="text-muted-foreground border-muted-foreground/40 ml-1 rounded border px-1 text-[10px]"
                                    title="Streams from YouTube; needs internet"
                                >
                                    streaming
                                </span>
                            {:else if track.state === "downloaded"}
                                <span
                                    class="text-primary border-primary/40 ml-1 rounded border px-1 text-[10px]"
                                    title="Saved to disk; plays offline"
                                >
                                    offline
                                </span>
                            {/if}
                        </span>

                        <span class="text-muted-foreground shrink-0 text-xs">
                            {formatDuration(track.durationSecs)}
                        </span>

                        <TrackActions
                            resolveTrackId={async () => track.id}
                            label="Queue {track.title}"
                        />

                        <Button
                            variant="ghost"
                            size="icon"
                            aria-label="Remove {track.title} from playlist"
                            onclick={() =>
                                playlistStore.removeTrack(detail.playlist.id, track.id)}
                        >
                            <XIcon />
                        </Button>
                    </li>
                {/each}
            </ul>
        {/if}
    {/if}
</section>
