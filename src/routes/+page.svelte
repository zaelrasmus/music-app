<script lang="ts">
    import { onMount } from "svelte";
    import { nav } from "$lib/nav.svelte";
    import { library } from "$lib/library.svelte";
    import { trackStore } from "$lib/tracks.svelte";
    import { player } from "$lib/player.svelte";
    import { providerSearch } from "$lib/provider-search.svelte";
    import { cacheStore } from "$lib/cache.svelte";
    import { loudnessStore } from "$lib/loudness.svelte";
    import { decoder } from "$lib/decoder.svelte";
    import { equalizer } from "$lib/equalizer.svelte";
    import { historyStore } from "$lib/history.svelte";
    import { playlistStore } from "$lib/playlists.svelte";
    import { tagStore } from "$lib/tags.svelte";
    import { libraryView } from "$lib/library-view.svelte";
    import { queueStore } from "$lib/queue.svelte";
    import { covers } from "$lib/covers.svelte";
    import { ytDlp } from "$lib/ytdlp.svelte";
    import { downloads } from "$lib/downloads.svelte";
    import { extras } from "$lib/extras.svelte";
    import { waveform } from "$lib/waveform.svelte";

    import LibraryView from "$components/library-view.svelte";
    import ProviderSearch from "$components/provider-search.svelte";
    import PlaylistsPanel from "$components/playlists-panel.svelte";
    import HistoryPanel from "$components/history-panel.svelte";
    import SettingsView from "$components/settings-view.svelte";
    import DetailsView from "$components/details-view.svelte";

    onMount(() => {
        library.load();
        trackStore.load();
        playlistStore.restoreSorts().then(() => playlistStore.load());
        // The artists a rule can name, and the pictures an artist playlist
        // borrows. Loaded once here rather than per panel, because both the
        // grid and the picker read it.
        playlistStore.loadArtists();
        tagStore.load();
        tagStore.loadPalette();
        // `restore` refreshes once the saved sort is known, so the list is not
        // fetched twice and does not visibly reorder itself on launch.
        libraryView.restore();
        providerSearch.loadProviders();
        cacheStore.restore();
        loudnessStore.refresh();
        decoder.refresh();
        historyStore.load();
        covers.load();
        ytDlp.refresh();
        downloads.refresh();
        extras.restore().then(() => waveform.reload(player.trackId));
        player.restorePreferences().then(() => player.restorePlayback());
        equalizer.restore();

        const scans = trackStore.listenForScans();
        const playback = player.listenForPlayer();
        const queue = queueStore.listenForQueue();
        const extractor = ytDlp.listenForUpdates();
        const activity = downloads.listenForActivity();

        // Artwork for a just-saved track arrives after the save returned, so
        // the lists holding that row have to be told. Reloading them is a few
        // local queries and far less code than patching one row in each of
        // four arrays — and it cannot drift out of step with the database.
        const artwork = covers.listenForCovers(() => {
            void libraryView.refresh();
            void historyStore.load();
            void queueStore.refresh();
        });

        return () => {
            scans.then((off) => off());
            playback.then((off) => off());
            queue.then((off) => off());
            extractor.then((off) => off());
            activity.then((off) => off());
            artwork.then((off) => off());
        };
    });
</script>

<!--
  One view at a time.

  Each is destroyed when it is not showing, which is deliberate: the state
  worth keeping (search results, the open playlist, filters) lives in the
  stores, not in the components, so a view can be thrown away and rebuilt
  without losing anything or re-fetching on every visit.
-->
{#if nav.view === "library"}
    <LibraryView />
{:else if nav.view === "search"}
    <ProviderSearch />
{:else if nav.view === "playlists"}
    <PlaylistsPanel />
{:else if nav.view === "history"}
    <HistoryPanel />
{:else if nav.view === "details"}
    <DetailsView />
{:else}
    <SettingsView />
{/if}
