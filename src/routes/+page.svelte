<script lang="ts">
    import { onMount } from "svelte";
    import { Button } from "$components/ui/button";
    import { library } from "$lib/library.svelte";
    import { trackStore } from "$lib/tracks.svelte";
    import { player } from "$lib/player.svelte";
    import ProviderSearch from "$components/provider-search.svelte";
    import { providerSearch } from "$lib/provider-search.svelte";
    import { cacheStore } from "$lib/cache.svelte";
    import { historyStore } from "$lib/history.svelte";
    import HistoryPanel from "$components/history-panel.svelte";
    import CacheSettings from "$components/cache-settings.svelte";
    import PlaylistsPanel from "$components/playlists-panel.svelte";
    import LibraryView from "$components/library-view.svelte";
    import { playlistStore } from "$lib/playlists.svelte";
    import { tagStore } from "$lib/tags.svelte";
    import { libraryView } from "$lib/library-view.svelte";
    import { queueStore } from "$lib/queue.svelte";
    import FolderPlusIcon from "@lucide/svelte/icons/folder-plus";
    import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
    import Trash2Icon from "@lucide/svelte/icons/trash-2";

    onMount(() => {
        library.load();
        trackStore.load();
        playlistStore.load();
        tagStore.load();
        libraryView.refresh();
        providerSearch.loadProviders();
        cacheStore.restore();
        historyStore.load();
        player.restorePreferences().then(() => player.restorePlayback());

        const scans = trackStore.listenForScans();
        const playback = player.listenForPlayer();
        const queue = queueStore.listenForQueue();

        return () => {
            scans.then((off) => off());
            playback.then((off) => off());
            queue.then((off) => off());
        };
    });

    function formatDate(unixSeconds: number) {
        return new Date(unixSeconds * 1000).toLocaleDateString();
    }

    const summary = $derived(trackStore.lastSummary);
</script>

<main class="mx-auto flex max-w-2xl flex-col gap-6 p-6">
    <header class="flex items-center justify-between gap-4">
        <div class="flex flex-col gap-1">
            <h1 class="text-xl font-semibold">Library folders</h1>
            <p class="text-muted-foreground text-sm">
                Folders scanned for music.
            </p>
        </div>

        <div class="flex items-center gap-2">
            <Button
                variant="outline"
                disabled={trackStore.scanning}
                onclick={() => trackStore.rescan()}
            >
                <RefreshCwIcon data-icon="inline-start" />
                {trackStore.scanning ? "Scanning…" : "Rescan"}
            </Button>
            <Button
                disabled={trackStore.scanning}
                onclick={() => library.addFromPicker()}
            >
                <FolderPlusIcon data-icon="inline-start" />
                Add folder
            </Button>
        </div>
    </header>

    {#if library.error || trackStore.error}
        <p
            class="border-destructive/50 text-destructive rounded-md border px-3 py-2 text-sm"
            role="alert"
        >
            {library.error ?? trackStore.error}
        </p>
    {/if}

    {#if library.folders.length === 0}
        <div
            class="text-muted-foreground rounded-lg border border-dashed px-6 py-10 text-center text-sm"
        >
            No folders yet. Add one to get started.
        </div>
    {:else}
        <ul class="flex flex-col gap-2">
            {#each library.folders as folder (folder.id)}
                <li
                    class="bg-card flex items-center justify-between gap-3 rounded-lg border px-3 py-2"
                >
                    <div class="flex min-w-0 flex-col">
                        <span class="truncate text-sm">{folder.path}</span>
                        <span class="text-muted-foreground text-xs">
                            Added {formatDate(folder.addedAt)}
                        </span>
                    </div>
                    <Button
                        variant="ghost"
                        size="icon"
                        aria-label="Remove {folder.path}"
                        disabled={trackStore.scanning}
                        onclick={() => library.remove(folder.id)}
                    >
                        <Trash2Icon />
                    </Button>
                </li>
            {/each}
        </ul>
    {/if}

    {#if summary}
        <p class="text-muted-foreground text-xs">
            Scanned {summary.scanned} · added {summary.added} · updated {summary.updated}
            · unchanged {summary.unchanged} · missing {summary.markedMissing}
            {#if summary.errors > 0}· {summary.errors} unreadable{/if}
            {#if summary.skippedFolders.length > 0}
                · skipped {summary.skippedFolders.length} unreachable folder(s)
            {/if}
        </p>
    {/if}

    <HistoryPanel />

    <CacheSettings />

    <PlaylistsPanel />

    <ProviderSearch />

    <LibraryView />
</main>
