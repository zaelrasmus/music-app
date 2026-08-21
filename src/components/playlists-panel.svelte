<script lang="ts">
    import { onMount } from "svelte";
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
    import { drawsAsArtist } from "$lib/playlists.svelte";
    import { PLAYLIST_GRID_SORTS } from "$lib/playlists.svelte";
    import ArrowUpNarrowWideIcon from "@lucide/svelte/icons/arrow-up-narrow-wide";
    import ArrowDownWideNarrowIcon from "@lucide/svelte/icons/arrow-down-wide-narrow";
    import SortControl from "$components/sort-control.svelte";
    import { PLAYLIST_SORT_OPTIONS } from "$lib/sorting";
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
    import BookmarkPlusIcon from "@lucide/svelte/icons/bookmark-plus";
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
    // Dragging belongs to the playlist's own order and nothing else. Under a
    // filter or any other sort, a row's position on screen has stopped being
    // its position in the playlist, so moving it there would mean something
    // different from what it looks like.
    const reorderable = $derived(
        !playlistStore.filtering && !playlistStore.sorted,
    );

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

    /**
     * The artist behind a single-artist playlist, if the library knows them.
     *
     * Only used for artwork. An explicitly set cover always wins -- picking a
     * different artist's face and uploading your own image are then the same
     * gesture, and there is no second place for a playlist's picture to live.
     */
    function artistAvatar(playlist: import("$lib/playlists.svelte").Playlist) {
        if (!drawsAsArtist(playlist)) return null;
        return playlist.artistRules[0].avatarUrl;
    }

    // Refreshed on every visit rather than only at startup: saving a track
    // by a new artist between visits would otherwise leave them missing from
    // the picker until the app was restarted.
    onMount(() => {
        void playlistStore.loadArtists();
    });

    /**
     * Tracks here that are not in the library.
     *
     * An imported playlist is all of them: importing says "I want this list",
     * not "I want fifty tracks in my library". They stay invisible to anything
     * keyed on membership -- artist rules above all -- until they are claimed.
     */
    // Counted from the shown tracks, which is why the action is withheld while
    // a filter is on: the command files the *whole* playlist, so a label read
    // off a narrowed list would promise three and do thirty-seven. Reordering
    // is withheld for the same reason a row's position stops meaning what it
    // shows.
    const unclaimed = $derived(
        playlistStore.filtering
            ? 0
            : (detail?.tracks ?? []).filter((t) => t.inLibrary === false).length,
    );

    /** Whether the "fills itself from" picker is open. */
    let pickingArtist = $state(false);

    /** Artists not already named by this playlist, most tracks first. */
    const pickableArtists = $derived.by(() => {
        const already = new Set(
            (detail?.playlist.artistRules ?? []).map((r) => r.artistKey),
        );
        return playlistStore.artists.filter((a) => !already.has(a.artistKey));
    });

    /** What the flip will do next, in the words of the field it applies to. */
    const gridDirectionLabel = $derived.by(() => {
        const option = PLAYLIST_GRID_SORTS.find(
            (o) => o.id === playlistStore.gridSort,
        );
        if (!option) return "Reverse order";
        return playlistStore.gridDirection === "asc" ? option.asc : option.desc;
    });

    const kinds = [
        { id: "all", label: "All" },
        { id: "artists", label: "Artists" },
        { id: "other", label: "Playlists" },
    ] as const;
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

        <!--
          A name filter and the derived split.

          Both exist for the same reason: seventy playlists is a scroll, and
          the split is free -- a playlist is an artist because it fills itself
          from one, so nothing has to be tagged or kept up to date.

          Shown from the second playlist onwards. It was once gated at five, to
          spare a new library the clutter, which meant the person who asked for
          it could not find it -- a feature hidden until it is not needed is the
          same as one that is missing.
        -->
        {#if playlistStore.playlists.length > 1}
            <div class="mb-4 flex flex-wrap items-center gap-2 px-3">
                <SearchField
                    class="min-w-[12rem] max-w-xs flex-1"
                    value={playlistStore.listQuery}
                    placeholder="Find a playlist…"
                    oninput={(v) => (playlistStore.listQuery = v)}
                    onclear={() => (playlistStore.listQuery = "")}
                />

                <!--
                  Recently played by default, because with seventy of them that
                  is nearly always the one being looked for. Both orders start
                  empty on every playlist that existed before them, so they say
                  nothing useful until the app has been used for a while --
                  creation date breaks the tie until then.
                -->
                <div class="flex shrink-0 items-center gap-1">
                    <select
                        class="bg-muted text-foreground rounded-full px-3 py-1 text-xs"
                        aria-label="Order playlists by"
                        value={playlistStore.gridSort}
                        onchange={(e) =>
                            playlistStore.setGridSort(
                                e.currentTarget.value as (typeof PLAYLIST_GRID_SORTS)[number]["id"],
                            )}
                    >
                        {#each PLAYLIST_GRID_SORTS as option (option.id)}
                            <option value={option.id}>{option.label}</option>
                        {/each}
                    </select>

                    <!-- Labelled by what it will do, not by "asc": "Z – A" is
                         a thing you can want, "ascending" is a thing you have
                         to translate. -->
                    <button
                        type="button"
                        class="text-muted-foreground hover:bg-accent hover:text-foreground grid size-7 place-items-center rounded-full transition-colors"
                        aria-label={gridDirectionLabel}
                        title={gridDirectionLabel}
                        onclick={() => playlistStore.toggleGridDirection()}
                    >
                        {#if playlistStore.gridDirection === "asc"}
                            <ArrowUpNarrowWideIcon class="size-3.5" />
                        {:else}
                            <ArrowDownWideNarrowIcon class="size-3.5" />
                        {/if}
                    </button>
                </div>

                <div class="bg-muted flex shrink-0 items-center gap-0.5 rounded-full p-0.5">
                    {#each kinds as option (option.id)}
                        <button
                            type="button"
                            class="rounded-full px-3 py-1 text-xs transition-colors {playlistStore.kind ===
                            option.id
                                ? 'bg-background text-foreground shadow-sm'
                                : 'text-muted-foreground hover:text-foreground'}"
                            aria-pressed={playlistStore.kind === option.id}
                            onclick={() => (playlistStore.kind = option.id)}
                        >
                            {option.label}
                        </button>
                    {/each}
                </div>
            </div>
        {/if}

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
              Two shapes, and the mix is the point.

              This grid once used one shape deliberately, because a circle
              beside a square read as two different kinds of thing. There now
              *are* two kinds: a playlist that fills itself from an artist, and
              one whose contents somebody chose. A circle is a person and a
              square is a collection, which is the convention everywhere else,
              and at seventy items the shape separates them before a single
              name is read.

              Derived from the rules, never chosen. A shape the user could pick
              would stop being information the first time someone picked the
              one they liked the look of.
            -->
            <ul
                class="grid gap-x-5 gap-y-6 px-3 [grid-template-columns:repeat(auto-fill,minmax(9.5rem,1fr))]"
            >
                {#each playlistStore.visiblePlaylists as playlist (playlist.id)}
                    {@const avatar = artistAvatar(playlist)}
                    <li class="group/card relative">
                        <button
                            type="button"
                            class="flex w-full flex-col gap-2.5 text-left"
                            onclick={() => playlistStore.openPlaylist(playlist.id)}
                        >
                            <!-- The art is generated from the playlist's name,
                                 so each one is recognisable at a glance in a
                                 grid of otherwise identical cards. An artist
                                 playlist borrows the artist's own picture,
                                 unless a cover was set explicitly. -->
                            <CoverArt
                                seed={`playlist::${playlist.name}`}
                                coverKey={playlist.coverKey}
                                src={playlist.coverKey ? null : avatar}
                                class="aspect-square w-full transition-transform group-hover/card:scale-[1.02] {drawsAsArtist(
                                    playlist,
                                )
                                    ? 'rounded-full'
                                    : 'rounded-xl'}"
                                glyph={false}
                            />
                            <span class="flex min-w-0 flex-col gap-0.5">
                                <span class="truncate text-[13px] font-medium">
                                    {playlist.name}
                                </span>
                                <span class="text-muted-foreground text-xs">
                                    {playlist.trackCount}
                                    {playlist.trackCount === 1 ? "song" : "songs"}
                                    {#if drawsAsArtist(playlist)}
                                        · artist
                                    {/if}
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

            {#if playlistStore.visiblePlaylists.length === 0}
                <p class="text-muted-foreground px-3 py-8 text-center text-sm">
                    Nothing matches that.
                </p>
            {/if}
        {/if}
    </PageShell>
{:else}
    <PageShell>
        {#snippet hero()}
            <!--
              An artist playlist is that artist's page in this library, so it
              is drawn as one: round picture, their name, and the same eyebrow
              the provider's artist page uses. Looking like a different kind of
              thing than the page for the same person, one tab away, is the
              confusion worth spending a prop to avoid.
            -->
            <ListHeader
                eyebrow={drawsAsArtist(detail.playlist) ? "Artist" : "Playlist"}
                artist={drawsAsArtist(detail.playlist)}
                title={detail.playlist.name}
                cover={`playlist::${detail.playlist.name}`}
                coverKey={detail.playlist.coverKey}
                coverSrc={detail.playlist.coverKey
                    ? null
                    : artistAvatar(detail.playlist)}
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

                            <!--
                              Shown only when there is something to claim, and
                              it says how many. A count in the label beats a
                              confirmation dialog: the number is the whole
                              question, and reading it costs nothing.
                            -->
                            {#if unclaimed > 0}
                                <DropdownMenu.Item
                                    onSelect={() =>
                                        playlistStore.addAllToLibrary(
                                            detail.playlist.id,
                                        )}
                                >
                                    <BookmarkPlusIcon />
                                    Add {unclaimed} to library
                                </DropdownMenu.Item>
                            {/if}
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
                    <!--
                      One row, as in the library. The toolbar stacks whatever it
                      is given, so controls that belong side by side have to say
                      so -- otherwise the sort ends up below the search instead
                      of beside it.
                    -->
                    <div class="flex flex-wrap items-center gap-2">
                        <!-- Filters this playlist only; the library keeps its own. -->
                        <SearchField
                            class="min-w-[14rem] max-w-md flex-1"
                            value={playlistStore.query}
                            placeholder="Filter this playlist…"
                            oninput={(v) => playlistStore.setQuery(v)}
                        />

                        <SortControl
                            sort={playlistStore.sort}
                            direction={playlistStore.direction}
                            options={PLAYLIST_SORT_OPTIONS}
                            onChange={(sortId, dir) =>
                                playlistStore.setSort(sortId, dir)}
                            onToggleDirection={() =>
                                playlistStore.setSort(
                                    playlistStore.sort,
                                    playlistStore.direction === "asc" ? "desc" : "asc",
                                )}
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
                            Play uses the filtered list. Reordering is off while
                            filtered, because a row's position here is not its
                            position in the playlist.
                        </p>
                    {:else if playlistStore.sorted}
                        <p class="text-muted-foreground text-xs">
                            Sorted view. Switch to Custom order to drag rows —
                            this is a way of looking at the playlist, not the
                            order it is stored in.
                        </p>
                    {/if}

                    <!--
                      Where the tracks below come from.

                      Named artists are a standing statement -- everything by
                      them belongs here, including what is saved tomorrow --
                      which is the one thing an ordinary playlist cannot say.
                      Anything else in the list was added by hand and stays
                      exactly where it was put.
                    -->
                    <div class="flex w-full flex-wrap items-center gap-1.5">
                        {#if detail.playlist.artistRules.length > 0}
                            <span class="text-muted-foreground text-xs">
                                Fills itself from
                            </span>
                        {/if}

                        {#each detail.playlist.artistRules as artistRule (artistRule.artistKey)}
                            <span
                                class="bg-accent text-accent-foreground flex items-center gap-1 rounded-full py-0.5 pr-1 pl-2.5 text-xs"
                            >
                                {artistRule.label}
                                <button
                                    type="button"
                                    class="text-muted-foreground hover:bg-background hover:text-foreground grid size-4 place-items-center rounded-full transition-colors"
                                    aria-label="Stop filling from {artistRule.label}"
                                    onclick={() =>
                                        playlistStore.removeArtistRule(
                                            detail.playlist.id,
                                            artistRule.artistKey,
                                        )}
                                >
                                    <XIcon class="size-3" />
                                </button>
                            </span>
                        {/each}

                        <button
                            type="button"
                            class="text-muted-foreground hover:border-foreground/40 hover:text-foreground rounded-full border border-dashed px-2.5 py-0.5 text-xs transition-colors"
                            aria-expanded={pickingArtist}
                            onclick={() => (pickingArtist = !pickingArtist)}
                        >
                            {detail.playlist.artistRules.length === 0
                                ? "Fill from an artist…"
                                : "Add another name…"}
                        </button>
                    </div>

                    {#if pickingArtist}
                        <!--
                          The names in the library, offered rather than
                          guessed. One artist is routinely several names and no
                          rule can tell which -- so identity is asserted here,
                          by the only party who knows.
                        -->
                        <div
                            class="bg-muted/40 flex max-h-56 w-full flex-col gap-0.5 overflow-y-auto rounded-lg border p-1.5"
                        >
                            {#each pickableArtists as candidate (candidate.artistKey)}
                                <button
                                    type="button"
                                    class="hover:bg-accent flex items-center gap-2 rounded-md px-2 py-1.5 text-left transition-colors"
                                    onclick={async () => {
                                        await playlistStore.addArtistRule(
                                            detail.playlist.id,
                                            candidate.name,
                                        );
                                        pickingArtist = false;
                                    }}
                                >
                                    <!-- Generated art: no picture is known for
                                         an artist until a rule names them and
                                         the lookup comes back. -->
                                    <CoverArt
                                        seed={`artist::${candidate.name}`}
                                        class="size-7 rounded-full"
                                        glyph={false}
                                    />
                                    <span class="min-w-0 flex-1 truncate text-xs">
                                        {candidate.name}
                                    </span>
                                    <span class="text-muted-foreground shrink-0 text-[11px]">
                                        {candidate.trackCount}
                                    </span>
                                </button>
                            {:else}
                                <p class="text-muted-foreground px-2 py-1.5 text-xs">
                                    No other artists in your library yet.
                                </p>
                            {/each}
                        </div>
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
            <!--
              Keyed by track, not by position. These rows are dragged into
              new positions -- that is the whole feature -- and identifying
              them by the position they happen to occupy means the row you
              picked up is not the row that lands.
            -->
            <VirtualList
                rows={detail.tracks}
                estimateSize={ROW_HEIGHT}
                key={(track) => track.id}
            >
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
