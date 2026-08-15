<script lang="ts">
    import { Button } from "$components/ui/button";
    import PageShell from "$components/page-shell.svelte";
    import EmptyState from "$components/empty-state.svelte";
    import SearchField from "$components/search-field.svelte";
    import TrackRow from "$components/track-row.svelte";
    import TagFilter from "$components/tag-filter.svelte";
    import { libraryView } from "$lib/library-view.svelte";
    import { library } from "$lib/library.svelte";
    import { trackStore } from "$lib/tracks.svelte";
    import { player } from "$lib/player.svelte";
    import { nav } from "$lib/nav.svelte";
    import LibraryIcon from "@lucide/svelte/icons/library";
    import UsersIcon from "@lucide/svelte/icons/users";
    import ListIcon from "@lucide/svelte/icons/list";
    import PlayIcon from "@lucide/svelte/icons/play";
    import ShuffleIcon from "@lucide/svelte/icons/shuffle";
    import FolderPlusIcon from "@lucide/svelte/icons/folder-plus";
    import SearchIcon from "@lucide/svelte/icons/search";

    /** Flat view: the visible list is the queue. */
    const flatIds = $derived(libraryView.results.map((t) => t.id));

    /**
     * Grouped view: the queue is every track in display order, so playing from
     * inside one artist continues into the next rather than stopping.
     */
    const groupedIds = $derived(
        libraryView.groups.flatMap((g) => g.tracks.map((t) => t.id)),
    );

    /** Where each group starts within that flattened order. */
    const groupOffsets = $derived.by(() => {
        const offsets: number[] = [];
        let running = 0;
        for (const group of libraryView.groups) {
            offsets.push(running);
            running += group.tracks.length;
        }
        return offsets;
    });

    const grouped = $derived(libraryView.groupByArtist);
    const ids = $derived(grouped ? groupedIds : flatIds);
    const total = $derived(ids.length);

    /**
     * Shuffle starts from a random point as well as shuffling what follows.
     *
     * Turning shuffle on and starting at track 1 every time is the classic
     * version of this that nobody wants.
     */
    async function shuffleAll() {
        if (total === 0) return;
        if (!player.shuffle) await player.toggleShuffle();
        await player.playQueue(
            ids,
            Math.floor(Math.random() * total),
            "your library",
        );
    }
</script>

<PageShell
    title="Library"
    badge={total > 0 ? `${total}` : null}
    subtitle="Everything you have saved, local files and streams together."
>
    {#snippet actions()}
        <Button
            variant="ghost"
            size="sm"
            disabled={total === 0}
            onclick={shuffleAll}
        >
            <ShuffleIcon data-icon="inline-start" />
            Shuffle
        </Button>
        <Button
            size="sm"
            disabled={total === 0}
            onclick={() => player.playQueue(ids, 0, "your library")}
        >
            <PlayIcon data-icon="inline-start" />
            Play
        </Button>
    {/snippet}

    {#snippet toolbar()}
        <div class="flex items-center gap-2">
            <SearchField
                class="max-w-md flex-1"
                value={libraryView.query}
                placeholder="Search titles and artists…"
                oninput={(v) => libraryView.setQuery(v)}
            />

            <!-- Grouping is a different backend query that takes no filters, so
                 it is a mode rather than another control alongside them. -->
            <Button
                variant="outline"
                size="sm"
                aria-pressed={grouped}
                title={grouped
                    ? "Show one flat, filterable list"
                    : "Group the library by artist"}
                onclick={() => libraryView.toggleGrouping()}
            >
                {#if grouped}
                    <ListIcon data-icon="inline-start" />
                    Flat list
                {:else}
                    <UsersIcon data-icon="inline-start" />
                    By artist
                {/if}
            </Button>
        </div>

        {#if grouped}
            <p class="text-muted-foreground text-xs">
                Grouped by artist — searching and tag filters apply to the flat
                list.
            </p>
        {:else}
            <TagFilter
                selectedIds={libraryView.selectedTagIds}
                mode={libraryView.mode}
                active={libraryView.filtering}
                onToggle={(id) => libraryView.toggleTag(id)}
                onModeChange={(m) => libraryView.setMode(m)}
                onClear={() => libraryView.clearFilters()}
            />
        {/if}
    {/snippet}

    {#if libraryView.loading && total === 0}
        <p class="text-muted-foreground px-2 py-6 text-sm">Loading…</p>
    {:else if grouped}
        {#if libraryView.groups.length === 0}
            <EmptyState
                icon={LibraryIcon}
                title="Nothing to group yet"
                hint="Add a folder of music, or save something from Search."
            />
        {:else}
            <div class="flex flex-col gap-5">
                {#each libraryView.groups as group, groupIndex (group.artist)}
                    <section class="flex flex-col gap-1">
                        <h2
                            class="text-muted-foreground bg-background/85 sticky top-0 z-[1] px-2 py-1 text-[11px] font-semibold tracking-[0.07em] uppercase backdrop-blur-sm"
                        >
                            {group.artist}
                            <span class="opacity-60">({group.tracks.length})</span>
                        </h2>
                        <ul class="flex flex-col">
                            {#each group.tracks as track, trackIndex (track.id)}
                                <TrackRow
                                    {track}
                                    queueIds={groupedIds}
                                    index={groupOffsets[groupIndex] + trackIndex}
                                />
                            {/each}
                        </ul>
                    </section>
                {/each}
            </div>
        {/if}
    {:else if libraryView.results.length === 0}
        {#if libraryView.filtering}
            <EmptyState
                icon={SearchIcon}
                title="Nothing matches"
                hint="Try a shorter search, or clear the tag filters."
            >
                <Button
                    variant="outline"
                    size="sm"
                    onclick={() => libraryView.clearFilters()}
                >
                    Clear filters
                </Button>
            </EmptyState>
        {:else if library.folders.length === 0}
            <EmptyState
                icon={FolderPlusIcon}
                title="Your library is empty"
                hint="Point the app at a folder of music, or search YouTube and SoundCloud for something to save."
            >
                <div class="flex items-center gap-2">
                    <Button size="sm" onclick={() => library.addFromPicker()}>
                        <FolderPlusIcon data-icon="inline-start" />
                        Add a folder
                    </Button>
                    <Button variant="outline" size="sm" onclick={() => nav.go("search")}>
                        <SearchIcon data-icon="inline-start" />
                        Search online
                    </Button>
                </div>
            </EmptyState>
        {:else}
            <EmptyState
                icon={LibraryIcon}
                title="No tracks found in your folders"
                hint="The folders are registered but nothing readable turned up. A rescan will pick up anything added since."
            >
                <Button
                    variant="outline"
                    size="sm"
                    disabled={trackStore.scanning}
                    onclick={() => trackStore.rescan()}
                >
                    {trackStore.scanning ? "Scanning…" : "Rescan"}
                </Button>
            </EmptyState>
        {/if}
    {:else}
        <ul class="flex flex-col" class:opacity-60={libraryView.loading}>
            {#each libraryView.results as track, index (track.id)}
                <TrackRow {track} queueIds={flatIds} {index} />
            {/each}
        </ul>
    {/if}
</PageShell>
