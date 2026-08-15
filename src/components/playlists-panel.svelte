<script lang="ts">
    import { Button } from "$components/ui/button";
    import * as DropdownMenu from "$components/ui/dropdown-menu";
    import PageShell from "$components/page-shell.svelte";
    import EmptyState from "$components/empty-state.svelte";
    import SearchField from "$components/search-field.svelte";
    import TagFilter from "$components/tag-filter.svelte";
    import TrackRow from "$components/track-row.svelte";
    import CoverArt from "$components/cover-art.svelte";
    import { playlistStore } from "$lib/playlists.svelte";
    import { player } from "$lib/player.svelte";
    import { nav } from "$lib/nav.svelte";
    import { promptFor } from "$lib/prompt.svelte";
    import PlayIcon from "@lucide/svelte/icons/play";
    import PlusIcon from "@lucide/svelte/icons/plus";
    import Trash2Icon from "@lucide/svelte/icons/trash-2";
    import PencilIcon from "@lucide/svelte/icons/pencil";
    import XIcon from "@lucide/svelte/icons/x";
    import ChevronLeftIcon from "@lucide/svelte/icons/chevron-left";
    import ListMusicIcon from "@lucide/svelte/icons/list-music";
    import MoreHorizontalIcon from "@lucide/svelte/icons/more-horizontal";
    import ShuffleIcon from "@lucide/svelte/icons/shuffle";

    /** Index currently being dragged, and the index it is hovering over. */
    let dragFrom = $state<number | null>(null);
    let dragOver = $state<number | null>(null);

    const detail = $derived(playlistStore.open);

    async function create() {
        const name = await promptFor("New playlist", {
            label: "What should it be called?",
            placeholder: "Playlist name",
            confirmLabel: "Create",
        });
        if (name !== null) await playlistStore.create(name);
    }

    async function rename() {
        if (!detail) return;
        const name = await promptFor("Rename playlist", {
            label: "A new name for this playlist",
            initial: detail.playlist.name,
            confirmLabel: "Rename",
        });
        if (name !== null) await playlistStore.rename(detail.playlist.id, name);
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

    async function shuffle() {
        if (!detail || detail.tracks.length === 0) return;
        if (!player.shuffle) await player.toggleShuffle();
        await playlistStore.play(
            Math.floor(Math.random() * detail.tracks.length),
        );
    }
</script>

{#if !detail}
    <PageShell
        title="Playlists"
        badge={playlistStore.playlists.length > 0
            ? `${playlistStore.playlists.length}`
            : null}
        subtitle="Local files and streamed tracks in the same list."
    >
        {#snippet actions()}
            <Button size="sm" onclick={create}>
                <PlusIcon data-icon="inline-start" />
                New playlist
            </Button>
        {/snippet}

        {#if playlistStore.playlists.length === 0}
            <EmptyState
                icon={ListMusicIcon}
                title="No playlists yet"
                hint="Make one, then add tracks to it from the ⋯ menu on any row."
            >
                <Button size="sm" onclick={create}>
                    <PlusIcon data-icon="inline-start" />
                    New playlist
                </Button>
            </EmptyState>
        {:else}
            <ul
                class="grid gap-2 px-2 [grid-template-columns:repeat(auto-fill,minmax(15rem,1fr))]"
            >
                {#each playlistStore.playlists as playlist (playlist.id)}
                    <li class="group/card relative">
                        <button
                            type="button"
                            class="bg-card hover:border-primary/40 hover:bg-accent/40 flex w-full items-center gap-3 rounded-lg border p-2.5 text-left transition-colors"
                            onclick={() => playlistStore.openPlaylist(playlist.id)}
                        >
                            <!-- The art is generated from the playlist's name,
                                 so each one is recognisable at a glance in a
                                 grid of otherwise identical cards. -->
                            <CoverArt
                                seed={`playlist::${playlist.name}`}
                                class="size-11"
                                glyph={false}
                            />
                            <span class="flex min-w-0 flex-1 flex-col">
                                <span class="truncate text-sm font-medium">
                                    {playlist.name}
                                </span>
                                <span class="text-muted-foreground text-xs">
                                    {playlist.trackCount}
                                    {playlist.trackCount === 1 ? "track" : "tracks"}
                                </span>
                            </span>
                        </button>

                        <button
                            type="button"
                            class="text-muted-foreground hover:bg-accent hover:text-destructive absolute top-2 right-2 grid size-7 place-items-center rounded-md opacity-0 transition-opacity group-hover/card:opacity-100 focus-visible:opacity-100"
                            aria-label="Delete {playlist.name}"
                            title="Delete {playlist.name}"
                            onclick={() => playlistStore.remove(playlist.id)}
                        >
                            <Trash2Icon class="size-4" />
                        </button>
                    </li>
                {/each}
            </ul>
        {/if}
    </PageShell>
{:else}
    <PageShell
        title={detail.playlist.name}
        badge={playlistStore.filtering
            ? `${detail.tracks.length} of ${detail.playlist.trackCount}`
            : `${detail.playlist.trackCount}`}
        subtitle={playlistStore.filtering
            ? "Play uses the filtered list. Reordering is off while filtered, because a row's position here is not its position in the playlist."
            : undefined}
    >
        {#snippet leading()}
            <button
                type="button"
                class="text-muted-foreground hover:bg-accent hover:text-foreground grid size-8 place-items-center rounded-md transition-colors"
                aria-label="Back to playlists"
                onclick={() => playlistStore.close()}
            >
                <ChevronLeftIcon class="size-5" />
            </button>
        {/snippet}

        {#snippet actions()}
            <Button
                variant="ghost"
                size="sm"
                disabled={detail.tracks.length === 0}
                onclick={shuffle}
            >
                <ShuffleIcon data-icon="inline-start" />
                Shuffle
            </Button>
            <Button
                size="sm"
                disabled={detail.tracks.length === 0}
                onclick={() => playlistStore.play(0)}
            >
                <PlayIcon data-icon="inline-start" />
                Play
            </Button>

            <DropdownMenu.Root>
                <DropdownMenu.Trigger>
                    {#snippet child({ props })}
                        <button
                            {...props}
                            type="button"
                            class="text-muted-foreground hover:bg-accent hover:text-foreground grid size-8 place-items-center rounded-md transition-colors"
                            aria-label="Playlist options"
                        >
                            <MoreHorizontalIcon class="size-4" />
                        </button>
                    {/snippet}
                </DropdownMenu.Trigger>
                <DropdownMenu.Content align="end" class="w-48">
                    <DropdownMenu.Item onSelect={rename}>
                        <PencilIcon />
                        Rename
                    </DropdownMenu.Item>
                    <DropdownMenu.Separator />
                    <DropdownMenu.Item
                        onSelect={async () => {
                            await playlistStore.remove(detail.playlist.id);
                            playlistStore.close();
                        }}
                    >
                        <Trash2Icon />
                        Delete playlist
                    </DropdownMenu.Item>
                </DropdownMenu.Content>
            </DropdownMenu.Root>
        {/snippet}

        {#snippet toolbar()}
            <!-- Filters this playlist only; the library keeps its own. -->
            <SearchField
                class="max-w-md"
                value={playlistStore.query}
                placeholder="Filter this playlist…"
                oninput={(v) => playlistStore.setQuery(v)}
            />

            <TagFilter
                selectedIds={playlistStore.selectedTagIds}
                mode={playlistStore.mode}
                active={playlistStore.filtering}
                onToggle={(id) => playlistStore.toggleTag(id)}
                onModeChange={(m) => playlistStore.setMode(m)}
                onClear={() => playlistStore.clearFilters()}
                showCounts={false}
            />
        {/snippet}

        {#if detail.tracks.length === 0}
            <EmptyState
                icon={ListMusicIcon}
                title={playlistStore.filtering
                    ? "Nothing in this playlist matches"
                    : "This playlist is empty"}
                hint={playlistStore.filtering
                    ? "Clear the filter to see the rest."
                    : "Add tracks from your library or from a search, using the ⋯ menu on any row."}
            >
                {#if playlistStore.filtering}
                    <Button
                        variant="outline"
                        size="sm"
                        onclick={() => playlistStore.clearFilters()}
                    >
                        Clear filters
                    </Button>
                {:else}
                    <Button variant="outline" size="sm" onclick={() => nav.go("library")}>
                        Go to library
                    </Button>
                {/if}
            </EmptyState>
        {:else}
            <ul class="flex flex-col">
                {#each detail.tracks as track, index (track.id)}
                    <TrackRow
                        {track}
                        {index}
                        onPlay={() => playlistStore.play(index)}
                        reorder={{
                            enabled: reorderable,
                            over: dragOver === index,
                            onStart: () => (dragFrom = index),
                            onOver: () => (dragOver = index),
                            onLeave: () => {
                                if (dragOver === index) dragOver = null;
                            },
                            onDrop: () => drop(index),
                            onEnd: () => {
                                dragFrom = null;
                                dragOver = null;
                            },
                        }}
                    >
                        {#snippet extra()}
                            <DropdownMenu.Item
                                onSelect={() =>
                                    playlistStore.removeTrack(
                                        detail.playlist.id,
                                        track.id,
                                    )}
                            >
                                <XIcon />
                                Remove from this playlist
                            </DropdownMenu.Item>
                        {/snippet}
                    </TrackRow>
                {/each}
            </ul>
        {/if}
    </PageShell>
{/if}
