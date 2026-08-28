<script lang="ts">
    import { onMount } from "svelte";
    import { SEARCH_ROW_HEIGHT } from "$lib/virtual.svelte";
    import { invoke } from "@tauri-apps/api/core";
    import PageShell from "$components/page-shell.svelte";
    import EmptyState from "$components/empty-state.svelte";
    import SearchField from "$components/search-field.svelte";
    import CoverArt from "$components/cover-art.svelte";
    import TrackMenu from "$components/track-menu.svelte";
    import CollectionRow from "$components/collection-row.svelte";
    import ArtistHeader from "$components/artist-header.svelte";
    import VirtualList from "$components/virtual-list.svelte";
    import { Button } from "$components/ui/button";
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
    import ArrowLeftIcon from "@lucide/svelte/icons/arrow-left";
    import ShuffleIcon from "@lucide/svelte/icons/shuffle";
    import BookmarkIcon from "@lucide/svelte/icons/bookmark";
    import ListMusicIcon from "@lucide/svelte/icons/list-music";
    import UserIcon from "@lucide/svelte/icons/user";

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

    /**
     * Cover art is square; video thumbnails are 16:9.
     *
     * Keyed on the *source* rather than the provider, which is what makes YT
     * Music right: it reaches YouTube, but what it returns is album art from
     * the music catalogue, not a frame from a video. Deciding by provider put
     * square art in a 16:9 box, which is what "the thumbnails look weird"
     * actually was.
     */
    const artClass = $derived(
        providerSearch.source === "youtube"
            ? "aspect-video w-[7rem]"
            : "aspect-square w-16",
    );

    function uploaderLabel(result: SearchResult) {
        return (
            result.channel ??
            (result.provider === "soundcloud"
                ? "Unknown uploader"
                : "Unknown channel")
        );
    }

    /** The collection being looked inside, if any. */
    const opened = $derived(providerSearch.opened);

    /** Whichever list is on screen, for the header count. */
    const shownCount = $derived(
        opened
            ? opened.tracks.length
            : providerSearch.kind === "track"
              ? providerSearch.results.length
              : providerSearch.collections.length,
    );

    /**
     * Whether the list was cut off.
     *
     * Compared against the backend's own cap rather than a number repeated
     * here, so the two cannot disagree about what "the first N" means.
     */
    const truncated = $derived(
        opened !== null && opened.tracks.length >= maxExpanded,
    );

    const busyWithCollection = $derived(
        providerSearch.importing ||
            providerSearch.expanding ||
            (opened?.tracks.length ?? 0) === 0,
    );

    const openedSubtitle = $derived(
        opened?.collection.kind === "artist"
            ? "Their uploads, newest first. Play the lot or keep the ones worth keeping."
            : "Everything in this playlist. Playing it queues the whole thing — shuffle and repeat work on it like any other.",
    );

    const subtitleForKind = $derived(
        providerSearch.kind === "track"
            ? "Raw results, exactly as uploaded — the duration and uploader are how you tell a song from a ten-hour loop."
            : providerSearch.kind === "playlist"
              ? "Playlists as they exist on the service. Open one to see inside before committing to it."
              : "Channels and users. Open one for their uploads.",
    );

    /**
     * The backend's cap on how many tracks a collection expands to.
     *
     * Asked for rather than hardcoded: the number is a judgement about how
     * long an expansion may take, and that judgement lives with the code that
     * pays for it.
     */
    let maxExpanded = $state(200);
    onMount(async () => {
        // Results survive leaving this page, and the library can change while
        // they are gone — a track removed from it elsewhere must not still be
        // wearing an "In library" badge on the way back.
        void providerSearch.refreshFiled();

        try {
            maxExpanded = await invoke<number>("max_expanded_tracks");
        } catch {
            // Cosmetic — it only decides whether one line of text appears.
        }
    });
</script>

<!--
  An artist replaces the page's identity rather than sitting under it.

  `hero` is the same seam the playlist detail view uses, and taking it is what
  makes this a page *about someone* instead of a list with a round picture on
  top: no "Search" heading above their name, no subtitle explaining the view,
  just them. A playlist keeps the ordinary title block, because a playlist is
  its contents and wants them to start immediately.
-->
{#snippet artistHero()}
    {#if opened}
        <div class="flex flex-col gap-4">
            <button
                type="button"
                class="text-muted-foreground hover:text-foreground -ml-1 flex w-fit items-center gap-1.5 rounded-md px-1 py-1 text-xs font-medium transition-colors"
                onclick={() => providerSearch.back()}
            >
                <ArrowLeftIcon class="size-3.5" />
                Back
            </button>

            <ArtistHeader
                collection={opened.collection}
                trackCount={opened.tracks.length}
                busy={busyWithCollection}
                importing={providerSearch.importing}
                onplay={() => providerSearch.playAll()}
                onshuffle={() => providerSearch.shuffleAll()}
                onsave={() => providerSearch.importCollection()}
            />
        </div>
    {/if}
{/snippet}

<PageShell
    hero={opened?.collection.kind === "artist" ? artistHero : undefined}
    title={opened ? opened.collection.title : "Search"}
    badge={shownCount > 0 ? `${shownCount}` : null}
    subtitle={opened ? openedSubtitle : subtitleForKind}
>
    {#snippet toolbar()}
        {#if opened && opened.collection.kind !== "artist"}
            <!--
              The way back, and the only one: this view has no route to be
              linked to, so the button *is* the navigation. Kept in the
              toolbar rather than floating over the list so it sits where the
              search box was, which is where the eye already is.
            -->
            <button
                type="button"
                class="text-muted-foreground hover:text-foreground -ml-1 flex w-fit items-center gap-1.5 rounded-md px-1 py-1 text-xs font-medium transition-colors"
                onclick={() => providerSearch.back()}
            >
                <ArrowLeftIcon class="size-3.5" />
                Back to playlists
            </button>
        {:else if opened}
            <!-- The artist page carries its own way back, inside the hero. -->
        {:else}
        <div class="flex items-center gap-2">
            <SearchField
                class="max-w-lg flex-1"
                value={providerSearch.query}
                placeholder="Search {providerSearch.providerName} for a song…"
                oninput={(v) => providerSearch.queueSearch(v)}
                onenter={() => providerSearch.searchNow()}
            />

            {#if providerSearch.sources.length > 1}
                <!-- A segmented control rather than chips: these are mutually
                     exclusive and one is always on, which chips do not say. -->
                <div
                    class="bg-muted flex items-center gap-0.5 rounded-lg p-0.5"
                    role="tablist"
                    aria-label="Search provider"
                >
                    {#each providerSearch.sources as provider (provider.id)}
                        {@const selected = providerSearch.source === provider.id}
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

        {#if providerSearch.kinds.length > 1}
            <!--
              What to search for, not where. Kept on its own line under the
              provider row because it is a narrower question than the one
              above it, and the tabs a provider offers come from the backend —
              SoundCloud has no playlist or artist search, so it shows none of
              this rather than tabs that always come back empty.
            -->
            <div
                class="bg-muted flex w-fit items-center gap-0.5 rounded-lg p-0.5"
                role="tablist"
                aria-label="What to search for"
            >
                {#each providerSearch.kinds as kind (kind)}
                    {@const selected = providerSearch.kind === kind}
                    <button
                        type="button"
                        role="tab"
                        aria-selected={selected}
                        class="rounded-md px-3 py-1.5 text-xs font-medium capitalize transition-colors
                               {selected
                            ? 'bg-background text-foreground shadow-sm'
                            : 'text-muted-foreground hover:text-foreground'}"
                        onclick={() => providerSearch.setKind(kind)}
                    >
                        {kind === "track" ? "Songs" : `${kind}s`}
                    </button>
                {/each}
            </div>
        {/if}
        {/if}

        {#if providerSearch.error}
            <p
                class="border-destructive/50 bg-destructive/5 text-destructive selectable rounded-md border px-3 py-2 text-sm"
                role="alert"
            >
                {providerSearch.error}
            </p>
        {/if}
    {/snippet}

    {#if opened && opened.collection.kind === "artist"}
        {#if providerSearch.expanding && opened.tracks.length === 0}
            <p class="text-muted-foreground px-2 py-6 text-sm">
                Opening…
            </p>
        {:else}
            <!--
              A section of its own, with a heading and a rule. On a playlist
              the tracks *are* the page, so they need no introduction; on an
              artist page they are one thing among several the page could
              eventually show, and saying so is what stops this reading as a
              playlist with a round picture.
            -->
            <section class="flex flex-col gap-2">
                <div class="flex items-baseline justify-between border-b px-2 pb-2">
                    <h3 class="text-sm font-semibold">Songs</h3>
                    {#if truncated}
                        <span class="text-muted-foreground text-xs">
                            first {opened.tracks.length}
                        </span>
                    {/if}
                </div>

                <VirtualList
                    rows={opened.tracks}
                    estimateSize={SEARCH_ROW_HEIGHT}
                    resetKey={opened.collection.url}
                >
                    {#snippet row(result)}
                        {@render resultRow(result)}
                    {/snippet}
                </VirtualList>
            </section>
        {/if}
    {:else if opened}
        <!--
          The header is the whole reason a collection is worth opening rather
          than being queued straight from its row: the three things you might
          want to do with fifty tracks are here, above the fifty tracks.
        -->
        <div class="mb-4 flex items-start gap-4 px-2">
            <CoverArt
                seed={`${opened.collection.uploader ?? ""}::${opened.collection.title}`}
                src={opened.collection.thumbnailUrl}
                class="aspect-video w-32 shrink-0"
                glyph={false}
            />

            <div class="flex min-w-0 flex-1 flex-col gap-2">
                <div class="flex flex-col gap-0.5">
                    <span class="text-muted-foreground text-xs">
                        {opened.collection.uploader ??
                            (opened.collection.kind === "artist"
                                ? "Artist"
                                : "Playlist")}
                        {#if opened.tracks.length > 0}
                            · {opened.tracks.length}
                            {opened.tracks.length === 1 ? "track" : "tracks"}
                        {/if}
                    </span>
                    {#if truncated}
                        <!-- Said plainly. A list silently cut at 200 looks
                             like a provider that lost the rest. -->
                        <span class="text-muted-foreground text-xs">
                            Showing the first {opened.tracks.length} — long
                            playlists are capped so opening one stays quick.
                        </span>
                    {/if}
                </div>

                <div class="flex flex-wrap items-center gap-2">
                    <Button
                        size="sm"
                        disabled={busyWithCollection}
                        onclick={() => providerSearch.playAll()}
                    >
                        <PlayIcon data-icon="inline-start" />
                        Play all
                    </Button>
                    <Button
                        variant="outline"
                        size="sm"
                        disabled={busyWithCollection}
                        onclick={() => providerSearch.shuffleAll()}
                    >
                        <ShuffleIcon data-icon="inline-start" />
                        Shuffle
                    </Button>
                    <!--
                      Saving keeps the *list*. The tracks inside stay out of
                      the library exactly as an audition does — importing an
                      album is not a decision to file fifty songs.
                    -->
                    <Button
                        variant="outline"
                        size="sm"
                        disabled={busyWithCollection}
                        onclick={() => providerSearch.importCollection()}
                    >
                        {#if providerSearch.importing}
                            <LoaderIcon
                                data-icon="inline-start"
                                class="animate-spin"
                            />
                        {:else}
                            <BookmarkIcon data-icon="inline-start" />
                        {/if}
                        Save as playlist
                    </Button>
                </div>
            </div>
        </div>

        {#if providerSearch.expanding && opened.tracks.length === 0}
            <p class="text-muted-foreground px-2 py-6 text-sm">
                Opening… this takes a few seconds for a long playlist.
            </p>
        {:else}
            <VirtualList
                    rows={opened.tracks}
                    estimateSize={SEARCH_ROW_HEIGHT}
                    resetKey={opened.collection.url}
                >
                {#snippet row(result)}
                    {@render resultRow(result)}
                {/snippet}
            </VirtualList>
        {/if}
    {:else if providerSearch.kind !== "track"}
        {#if providerSearch.searching && providerSearch.collections.length === 0}
            <p class="text-muted-foreground px-2 py-6 text-sm">Searching…</p>
        {:else if providerSearch.collections.length === 0}
            <EmptyState
                icon={providerSearch.kind === "artist"
                    ? UserIcon
                    : ListMusicIcon}
                title={providerSearch.searched
                    ? "No results"
                    : `Search ${providerSearch.providerName} ${providerSearch.kind}s`}
                hint={providerSearch.searched
                    ? "Try fewer words — a playlist's name rarely matches a song's."
                    : "Open one to see what is inside, then play it or keep it as a playlist of your own."}
            />
        {:else}
            <ul
                class="flex flex-col gap-1"
                class:opacity-60={providerSearch.searching}
            >
                {#each providerSearch.collections as collection (collection.url)}
                    <CollectionRow
                        {collection}
                        onopen={() =>
                            providerSearch.openCollection(collection)}
                    />
                {/each}
            </ul>
        {/if}
    {:else if providerSearch.searching && providerSearch.results.length === 0}
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
        <div class:opacity-60={providerSearch.searching}>
            <!--
              A new search, or the same words sent to another service, is a
              new list — and results here are ordered by relevance, so being
              left at the bottom of the previous one hides exactly the rows
              worth seeing.
            -->
            <VirtualList
                rows={providerSearch.results}
                estimateSize={SEARCH_ROW_HEIGHT}
                resetKey="{providerSearch.source} {providerSearch.kind} {providerSearch.query}"
            >
                {#snippet row(result)}
                    {@render resultRow(result)}
                {/snippet}
            </VirtualList>
        </div>
    {/if}
</PageShell>

<!--
  One row, rendered from two places: a search result and a track inside an
  opened collection are the same thing and must offer the same actions. A
  second copy would drift the moment one of them gained a button.
-->
{#snippet resultRow(result: SearchResult)}
    {@const duration = formatDuration(result.durationSecs)}
    {@const count = formatCount(result.viewCount, countNoun)}
    {@const busy = providerSearch.saving === result.remoteId}
    {@const preview = looksLikePreview(result)}
    {@const inLibrary = providerSearch.isFiled(result)}
                <div
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
                            onclick={() => providerSearch.playFromHere(result)}
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
                            {#if result.channelUrl}
                                <!--
                                  The uploader's name is the way to their other
                                  uploads, and the only one that works on both
                                  services: SoundCloud cannot be searched for
                                  artists at all, but every result it returns
                                  still says whose it is.
                                -->
                                <button
                                    type="button"
                                    class="hover:text-foreground underline-offset-2 transition-colors hover:underline"
                                    title="See everything by {uploaderLabel(
                                        result,
                                    )}"
                                    onclick={() =>
                                        providerSearch.openArtistOf(result)}
                                >
                                    {uploaderLabel(result)}
                                </button>
                            {:else}
                                {uploaderLabel(result)}
                            {/if}
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
                </div>
{/snippet}
