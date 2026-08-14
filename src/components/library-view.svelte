<script lang="ts">
    import { Button } from "$components/ui/button";
    import { Input } from "$components/ui/input";
    import TrackRow from "$components/track-row.svelte";
    import { libraryView } from "$lib/library-view.svelte";
    import { tagStore } from "$lib/tags.svelte";
    import SearchIcon from "@lucide/svelte/icons/search";
    import LibraryIcon from "@lucide/svelte/icons/library";
    import UsersIcon from "@lucide/svelte/icons/users";
    import ListIcon from "@lucide/svelte/icons/list";
    import XIcon from "@lucide/svelte/icons/x";

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

    const total = $derived(
        libraryView.groupByArtist ? groupedIds.length : libraryView.results.length,
    );
</script>

<section class="flex flex-col gap-3">
    <div class="flex items-center justify-between gap-3">
        <div class="flex flex-col gap-1">
            <h2 class="flex items-center gap-2 text-lg font-semibold">
                <LibraryIcon class="size-4" />
                Your library
                <span class="text-muted-foreground text-sm font-normal">
                    ({total})
                </span>
            </h2>
            <p class="text-muted-foreground text-sm">
                Searches the tracks you already have — not YouTube.
            </p>
        </div>

        <Button
            variant="outline"
            aria-pressed={libraryView.groupByArtist}
            onclick={() => libraryView.toggleGrouping()}
        >
            {#if libraryView.groupByArtist}
                <ListIcon data-icon="inline-start" />
                Flat
            {:else}
                <UsersIcon data-icon="inline-start" />
                By artist
            {/if}
        </Button>
    </div>

    {#if !libraryView.groupByArtist}
        <div class="relative">
            <SearchIcon
                class="text-muted-foreground pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2"
            />
            <Input
                value={libraryView.query}
                placeholder="Search your library…"
                class="pl-9"
                oninput={(e) => libraryView.setQuery(e.currentTarget.value)}
            />
        </div>

        {#if tagStore.tags.length > 0}
            <div class="flex flex-wrap items-center gap-1">
                {#each tagStore.tags as tag (tag.id)}
                    {@const selected = libraryView.selectedTagIds.includes(tag.id)}
                    <button
                        type="button"
                        class="rounded-full border px-2 py-0.5 text-xs"
                        class:bg-primary={selected}
                        class:text-primary-foreground={selected}
                        class:border-primary={selected}
                        class:text-muted-foreground={!selected}
                        aria-pressed={selected}
                        onclick={() => libraryView.toggleTag(tag.id)}
                    >
                        {tag.name}
                        <span class="opacity-60">{tag.trackCount}</span>
                    </button>
                {/each}

                {#if libraryView.selectedTagIds.length > 1}
                    <button
                        type="button"
                        class="text-muted-foreground ml-1 text-xs underline"
                        onclick={() =>
                            libraryView.setMode(
                                libraryView.mode === "all" ? "any" : "all",
                            )}
                    >
                        {libraryView.mode === "all" ? "matching all" : "matching any"}
                    </button>
                {/if}

                {#if libraryView.filtering}
                    <Button
                        variant="ghost"
                        size="icon"
                        aria-label="Clear filters"
                        onclick={() => libraryView.clearFilters()}
                    >
                        <XIcon />
                    </Button>
                {/if}
            </div>
        {/if}
    {/if}

    {#if libraryView.loading}
        <p class="text-muted-foreground text-sm">Loading…</p>
    {:else if libraryView.groupByArtist}
        {#if libraryView.groups.length === 0}
            <p class="text-muted-foreground text-sm">No tracks yet.</p>
        {:else}
            <div class="flex flex-col gap-4">
                {#each libraryView.groups as group, groupIndex (group.artist)}
                    <div class="flex flex-col gap-1">
                        <h3 class="text-muted-foreground text-xs font-medium tracking-wide uppercase">
                            {group.artist}
                            <span class="opacity-60">({group.tracks.length})</span>
                        </h3>
                        <ul class="flex flex-col">
                            {#each group.tracks as track, trackIndex (track.id)}
                                <TrackRow
                                    {track}
                                    queueIds={groupedIds}
                                    index={groupOffsets[groupIndex] + trackIndex}
                                />
                            {/each}
                        </ul>
                    </div>
                {/each}
            </div>
        {/if}
    {:else if libraryView.results.length === 0}
        <p class="text-muted-foreground text-sm">
            {libraryView.filtering
                ? "Nothing matches."
                : "No tracks yet. Add a folder or press Rescan."}
        </p>
    {:else}
        <ul class="flex flex-col">
            {#each libraryView.results as track, index (track.id)}
                <TrackRow {track} queueIds={flatIds} {index} />
            {/each}
        </ul>
    {/if}
</section>
