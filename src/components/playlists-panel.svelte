<script lang="ts">
    import { Button } from "$components/ui/button";
    import * as DropdownMenu from "$components/ui/dropdown-menu";
    import PageShell from "$components/page-shell.svelte";
    import ListHeader from "$components/list-header.svelte";
    import EmptyState from "$components/empty-state.svelte";
    import SearchField from "$components/search-field.svelte";
    import TagFilter from "$components/tag-filter.svelte";
    import TrackRow from "$components/track-row.svelte";
    import VirtualList from "$components/virtual-list.svelte";
    import { ROW_HEIGHT } from "$lib/virtual.svelte";
    import CoverArt from "$components/cover-art.svelte";
    import { playlistStore } from "$lib/playlists.svelte";
    import { downloads } from "$lib/downloads.svelte";
    import { player } from "$lib/player.svelte";
    import { nav } from "$lib/nav.svelte";
    import { promptFor } from "$lib/prompt.svelte";
    import { open } from "@tauri-apps/plugin-dialog";
    import { cacheStore } from "$lib/cache.svelte";
    import { formatTotal } from "$lib/duration";
    import PlusIcon from "@lucide/svelte/icons/plus";
    import Trash2Icon from "@lucide/svelte/icons/trash-2";
    import PencilIcon from "@lucide/svelte/icons/pencil";
    import XIcon from "@lucide/svelte/icons/x";
    import ChevronLeftIcon from "@lucide/svelte/icons/chevron-left";
    import ListMusicIcon from "@lucide/svelte/icons/list-music";
    import MoreHorizontalIcon from "@lucide/svelte/icons/more-horizontal";
    import ImageIcon from "@lucide/svelte/icons/image";
    import ImageOffIcon from "@lucide/svelte/icons/image-off";
    import DownloadIcon from "@lucide/svelte/icons/download";

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

    /**
     * Picks an image for the open playlist.
     *
     * The file is copied into the cover store by the backend, not referenced
     * where it sits — a path into someone's pictures folder would break the
     * first time they moved it.
     */
    async function pickCover() {
        if (!detail) return;

        const picked = await open({
            multiple: false,
            directory: false,
            filters: [
                {
                    name: "Images",
                    extensions: ["jpg", "jpeg", "png", "webp", "gif", "bmp"],
                },
            ],
        });
        if (typeof picked !== "string") return;

        await playlistStore.setCover(detail.playlist.id, picked);
    }

    const totalSecs = $derived(
        (detail?.tracks ?? []).reduce((sum, t) => sum + (t.durationSecs ?? 0), 0),
    );

    const offline = $derived(
        (detail?.tracks ?? []).filter(
            (t) =>
                t.state === "downloaded" ||
                t.state === "present" ||
                cacheStore.isCached(t.id),
        ).length,
    );
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
            <Button size="sm" class="rounded-full" onclick={create}>
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
            <!--
              Art above the name, as in the demo's Library grid — but every
              tile is the same rounded square. The demo mixed a circle and a
              square in one grid, and those shapes already mean something
              (a circle is a person, a square is a collection), so the mix read
              as two different kinds of thing rather than as two playlists.
            -->
            <ul
                class="grid gap-x-5 gap-y-6 px-3 [grid-template-columns:repeat(auto-fill,minmax(9.5rem,1fr))]"
            >
                {#each playlistStore.playlists as playlist (playlist.id)}
                    <li class="group/card relative">
                        <button
                            type="button"
                            class="flex w-full flex-col gap-2.5 text-left"
                            onclick={() => playlistStore.openPlaylist(playlist.id)}
                        >
                            <!-- The art is generated from the playlist's name,
                                 so each one is recognisable at a glance in a
                                 grid of otherwise identical cards. -->
                            <CoverArt
                                seed={`playlist::${playlist.name}`}
                                coverKey={playlist.coverKey}
                                class="aspect-square w-full rounded-xl transition-transform group-hover/card:scale-[1.02]"
                                glyph={false}
                            />
                            <span class="flex min-w-0 flex-col gap-0.5">
                                <span class="truncate text-[13px] font-medium">
                                    {playlist.name}
                                </span>
                                <span class="text-muted-foreground text-xs">
                                    {playlist.trackCount}
                                    {playlist.trackCount === 1 ? "song" : "songs"}
                                </span>
                            </span>
                        </button>

                        <button
                            type="button"
                            class="text-white/80 hover:bg-black/70 hover:text-white absolute top-2 right-2 grid size-7 place-items-center rounded-full bg-black/50 opacity-0 backdrop-blur-sm transition-opacity group-hover/card:opacity-100 focus-visible:opacity-100"
                            aria-label="Delete {playlist.name}"
                            title="Delete {playlist.name}"
                            onclick={() => playlistStore.remove(playlist.id)}
                        >
                            <Trash2Icon class="size-3.5" />
                        </button>
                    </li>
                {/each}
            </ul>
        {/if}
    </PageShell>
{:else}
    <PageShell>
        {#snippet hero()}
            <ListHeader
                eyebrow="Playlist"
                title={detail.playlist.name}
                cover={`playlist::${detail.playlist.name}`}
                coverKey={detail.playlist.coverKey}
                empty={detail.tracks.length === 0}
                meta={[
                    playlistStore.filtering
                        ? `${detail.tracks.length} of ${detail.playlist.trackCount} shown`
                        : `${detail.playlist.trackCount} ${
                              detail.playlist.trackCount === 1 ? "song" : "songs"
                          }`,
                    formatTotal(totalSecs),
                    detail.tracks.length > 0 ? `${offline} playable offline` : null,
                ]}
                onPlay={() => playlistStore.play(0)}
                onShuffle={shuffle}
            >
                {#snippet leading()}
                    <button
                        type="button"
                        class="text-muted-foreground hover:bg-accent hover:text-foreground grid size-8 place-items-center rounded-full transition-colors"
                        aria-label="Back to playlists"
                        onclick={() => playlistStore.close()}
                    >
                        <ChevronLeftIcon class="size-5" />
                    </button>
                {/snippet}

                {#snippet actions()}
                    <DropdownMenu.Root>
                        <DropdownMenu.Trigger>
                            {#snippet child({ props })}
                                <button
                                    {...props}
                                    type="button"
                                    class="text-muted-foreground hover:bg-accent hover:text-foreground grid size-9 place-items-center rounded-full border transition-colors"
                                    aria-label="Playlist options"
                                >
                                    <MoreHorizontalIcon class="size-4" />
                                </button>
                            {/snippet}
                        </DropdownMenu.Trigger>
                        <DropdownMenu.Content align="end" class="w-52">
                            <DropdownMenu.Item onSelect={rename}>
                                <PencilIcon />
                                Rename
                            </DropdownMenu.Item>
                            <DropdownMenu.Item onSelect={pickCover}>
                                <ImageIcon />
                                {detail.playlist.coverKey
                                    ? "Change cover…"
                                    : "Choose a cover…"}
                            </DropdownMenu.Item>
                            {#if detail.playlist.coverKey}
                                <DropdownMenu.Item
                                    onSelect={() =>
                                        playlistStore.clearCover(detail.playlist.id)}
                                >
                                    <ImageOffIcon />
                                    Use generated art
                                </DropdownMenu.Item>
                            {/if}
                            <DropdownMenu.Separator />
                            <!--
                              Queues the whole playlist for offline play.
                              Tracks already on this device are skipped rather
                              than refused: "have all of it offline" is still
                              the request when half of it already is.
                            -->
                            <DropdownMenu.Item
                                onSelect={() =>
                                    downloads.queuePlaylist(detail.playlist.id)}
                            >
                                <DownloadIcon />
                                Download for offline
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

                    {#if playlistStore.filtering}
                        <p class="text-muted-foreground text-xs">
                            Play uses the filtered list. Reordering is off while
                            filtered, because a row's position here is not its
                            position in the playlist.
                        </p>
                    {/if}
                {/snippet}
            </ListHeader>
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
            <VirtualList rows={detail.tracks} estimateSize={ROW_HEIGHT}>
                {#snippet row(track, index)}
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
                {/snippet}
            </VirtualList>
        {/if}
    </PageShell>
{/if}
