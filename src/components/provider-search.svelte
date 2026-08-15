<script lang="ts">
    import PageShell from "$components/page-shell.svelte";
    import EmptyState from "$components/empty-state.svelte";
    import SearchField from "$components/search-field.svelte";
    import CoverArt from "$components/cover-art.svelte";
    import TrackMenu from "$components/track-menu.svelte";
    import {
        providerSearch,
        looksLikePreview,
        type SearchResult,
    } from "$lib/provider-search.svelte";
    import PlayIcon from "@lucide/svelte/icons/play";
    import LoaderIcon from "@lucide/svelte/icons/loader-circle";
    import SearchIcon from "@lucide/svelte/icons/search";
    import RadioIcon from "@lucide/svelte/icons/radio";
    import PlusIcon from "@lucide/svelte/icons/plus";
    import CheckIcon from "@lucide/svelte/icons/check";

    /** Whole minutes and seconds; hours only when there are hours. */
    function formatDuration(secs: number | null) {
        if (secs === null) return null;

        const total = Math.round(secs);
        const hours = Math.floor(total / 3600);
        const minutes = Math.floor((total % 3600) / 60);
        const seconds = total % 60;

        const pad = (n: number) => String(n).padStart(2, "0");
        return hours > 0
            ? `${hours}:${pad(minutes)}:${pad(seconds)}`
            : `${minutes}:${pad(seconds)}`;
    }

    /** 1.5M rather than 1535000969 — the magnitude is what matters. */
    function formatCount(count: number | null, noun: string) {
        if (count === null) return null;
        if (count < 1_000) return `${count} ${noun}`;
        if (count < 1_000_000) return `${(count / 1_000).toFixed(1)}K ${noun}`;
        if (count < 1_000_000_000)
            return `${(count / 1_000_000).toFixed(1)}M ${noun}`;
        return `${(count / 1_000_000_000).toFixed(1)}B ${noun}`;
    }

    /** SoundCloud counts plays, not views. */
    const countNoun = $derived(
        providerSearch.provider === "soundcloud" ? "plays" : "views",
    );

    /** SoundCloud artwork is square; YouTube thumbnails are 16:9. */
    const artClass = $derived(
        providerSearch.provider === "soundcloud"
            ? "aspect-square w-16"
            : "aspect-video w-[7rem]",
    );

    function uploaderLabel(result: SearchResult) {
        return (
            result.channel ??
            (result.provider === "soundcloud"
                ? "Unknown uploader"
                : "Unknown channel")
        );
    }
</script>

<PageShell
    title="Search"
    badge={providerSearch.results.length > 0
        ? `${providerSearch.results.length}`
        : null}
    subtitle="Raw results, exactly as uploaded — the duration and uploader are how you tell a song from a ten-hour loop."
>
    {#snippet toolbar()}
        <div class="flex items-center gap-2">
            <SearchField
                class="max-w-lg flex-1"
                value={providerSearch.query}
                placeholder="Search {providerSearch.providerName} for a song…"
                oninput={(v) => providerSearch.queueSearch(v)}
                onenter={() => providerSearch.searchNow()}
            />

            {#if providerSearch.providers.length > 1}
                <!-- A segmented control rather than chips: these are mutually
                     exclusive and one is always on, which chips do not say. -->
                <div
                    class="bg-muted flex items-center gap-0.5 rounded-lg p-0.5"
                    role="tablist"
                    aria-label="Search provider"
                >
                    {#each providerSearch.providers as provider (provider.id)}
                        {@const selected = providerSearch.provider === provider.id}
                        <button
                            type="button"
                            role="tab"
                            aria-selected={selected}
                            class="rounded-md px-3 py-1.5 text-xs font-medium transition-colors
                                   {selected
                                ? 'bg-background text-foreground shadow-sm'
                                : 'text-muted-foreground hover:text-foreground'}"
                            onclick={() => providerSearch.setProvider(provider.id)}
                        >
                            {provider.name}
                        </button>
                    {/each}
                </div>
            {/if}
        </div>

        {#if providerSearch.error}
            <p
                class="border-destructive/50 bg-destructive/5 text-destructive selectable rounded-md border px-3 py-2 text-sm"
                role="alert"
            >
                {providerSearch.error}
            </p>
        {/if}
    {/snippet}

    {#if providerSearch.searching && providerSearch.results.length === 0}
        <p class="text-muted-foreground px-2 py-6 text-sm">Searching…</p>
    {:else if providerSearch.searched && providerSearch.results.length === 0}
        <EmptyState
            icon={SearchIcon}
            title="No results"
            hint="Try fewer words, or switch provider — the catalogues barely overlap."
        />
    {:else if providerSearch.results.length === 0}
        <EmptyState
            icon={SearchIcon}
            title="Search {providerSearch.providerName}"
            hint="Anything you play from here is saved to your library and streams; download it later if you want to keep it."
        />
    {:else}
        <ul
            class="flex flex-col gap-1"
            class:opacity-60={providerSearch.searching}
        >
            {#each providerSearch.results as result (result.remoteId)}
                {@const duration = formatDuration(result.durationSecs)}
                {@const count = formatCount(result.viewCount, countNoun)}
                {@const busy = providerSearch.saving === result.remoteId}
                {@const preview = looksLikePreview(result)}
                {@const inLibrary = providerSearch.added.has(result.remoteId)}
                <li
                    class="group/result hover:bg-accent/50 flex items-start gap-3 rounded-lg px-2 py-2 transition-colors"
                >
                    <div class="relative shrink-0 {artClass}">
                        <CoverArt
                            seed={`${result.channel ?? ""}::${result.title}`}
                            src={result.thumbnailUrl}
                            class="size-full"
                            glyph={false}
                        />
                        {#if result.isLive}
                            <span
                                class="bg-destructive text-destructive-foreground absolute right-1 bottom-1 flex items-center gap-1 rounded px-1 text-[10px] font-medium"
                            >
                                <RadioIcon class="size-2.5" />
                                LIVE
                            </span>
                        {:else if duration}
                            <span
                                class="absolute right-1 bottom-1 rounded bg-black/75 px-1 text-[10px] font-medium text-white tabular-nums"
                            >
                                {duration}
                            </span>
                        {/if}

                        <!-- The play control sits on the art, where the eye
                             already is when deciding between two results. -->
                        <button
                            type="button"
                            class="absolute inset-0 grid place-items-center rounded-md bg-black/45 opacity-0 backdrop-blur-[1px] transition-opacity group-hover/result:opacity-100 focus-visible:opacity-100 focus-visible:outline-none"
                            aria-label="Play {result.title}"
                            disabled={busy}
                            onclick={() => providerSearch.playResult(result)}
                        >
                            {#if busy}
                                <LoaderIcon class="size-6 animate-spin text-white" />
                            {:else}
                                <PlayIcon class="size-6 fill-white text-white" />
                            {/if}
                        </button>
                    </div>

                    <div class="flex min-w-0 flex-1 flex-col gap-0.5 pt-0.5">
                        <!-- Full title, not truncated to one line: the tail is
                             often what distinguishes an edit from the original. -->
                        <span class="selectable text-sm leading-snug">
                            {result.title}
                        </span>
                        <span class="text-muted-foreground truncate text-xs">
                            {uploaderLabel(result)}
                            {#if count}
                                · {count}
                            {/if}
                        </span>

                        {#if preview}
                            <!-- Worth saying before the click, not after: this
                                 saves and plays fine, it just stops at 0:30. -->
                            <span
                                class="text-muted-foreground border-border mt-0.5 w-fit rounded-full border px-1.5 text-[10px]"
                                title="SoundCloud only serves a 30-second snippet for this upload (Go+ gated). Another upload of the same song may be full length."
                            >
                                likely a 30s preview
                            </span>
                        {/if}
                    </div>

                    <!--
                      The explicit gesture.

                      Playing, queueing and adding to a playlist all create the
                      track row, but none of them file it in the library —
                      auditioning ten songs to find one should not leave nine
                      behind. This button is the only thing here that says
                      "keep it", which is why it is a button and not a menu
                      item three clicks in.
                    -->
                    <button
                        type="button"
                        class="inline-flex h-8 shrink-0 items-center gap-1.5 rounded-full border px-3 text-xs font-medium transition-colors
                               {inLibrary
                            ? 'border-signal/40 text-signal cursor-default'
                            : 'border-border hover:bg-accent'}"
                        aria-label={inLibrary
                            ? `${result.title} is in your library`
                            : `Add ${result.title} to your library`}
                        title={inLibrary
                            ? "In your library"
                            : "Add to your library. Playing a track does not do this — history is how you find one you did not keep."}
                        disabled={inLibrary || busy}
                        onclick={() => providerSearch.addToLibrary(result)}
                    >
                        {#if inLibrary}
                            <CheckIcon class="size-3.5" />
                            In library
                        {:else}
                            <PlusIcon class="size-3.5" />
                            Add
                        {/if}
                    </button>

                    <!-- Queueing saves the result first: the queue holds track
                         ids, and a search result is not a track yet. -->
                    <TrackMenu
                        resolveTrackId={() => providerSearch.saveResult(result)}
                        label="More actions for {result.title}"
                        trigger="opacity-0 transition-opacity group-hover/result:opacity-100 focus-visible:opacity-100 data-open:opacity-100"
                    />
                </li>
            {/each}
        </ul>
    {/if}
</PageShell>
