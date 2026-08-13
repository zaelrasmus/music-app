<script lang="ts">
    import { onMount } from "svelte";
    import { Button } from "$components/ui/button";
    import { library } from "$lib/library.svelte";
    import { trackStore } from "$lib/tracks.svelte";
    import FolderPlusIcon from "@lucide/svelte/icons/folder-plus";
    import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
    import Trash2Icon from "@lucide/svelte/icons/trash-2";

    onMount(() => {
        library.load();
        trackStore.load();

        const unlisten = trackStore.listenForScans();
        return () => {
            unlisten.then((off) => off());
        };
    });

    function formatDate(unixSeconds: number) {
        return new Date(unixSeconds * 1000).toLocaleDateString();
    }

    function formatDuration(secs: number | null) {
        if (secs === null) return "";
        const m = Math.floor(secs / 60);
        const s = secs % 60;
        return `${m}:${String(s).padStart(2, "0")}`;
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

    <!-- Deliberately bare: the polished track view is the next task. -->
    <section class="flex flex-col gap-2">
        <h2 class="text-lg font-semibold">
            Tracks
            <span class="text-muted-foreground text-sm font-normal">
                ({trackStore.tracks.length})
            </span>
        </h2>

        {#if trackStore.tracks.length === 0}
            <p class="text-muted-foreground text-sm">
                No tracks yet. Add a folder or press Rescan.
            </p>
        {:else}
            <ul class="flex flex-col">
                {#each trackStore.tracks as track (track.id)}
                    <li
                        class="flex items-baseline justify-between gap-3 border-b py-1.5 text-sm last:border-b-0"
                        class:opacity-50={track.state === "missing"}
                    >
                        <span class="truncate">
                            {track.title}
                            <span class="text-muted-foreground">
                                — {track.artist ?? "Unknown artist"}
                            </span>
                            {#if track.state === "missing"}
                                <span class="text-muted-foreground text-xs">
                                    (missing)
                                </span>
                            {/if}
                        </span>
                        <span class="text-muted-foreground shrink-0 text-xs">
                            {formatDuration(track.durationSecs)}
                        </span>
                    </li>
                {/each}
            </ul>
        {/if}
    </section>
</main>
