<script lang="ts">
    import { onMount } from "svelte";
    import { Button } from "$components/ui/button";
    import { library } from "$lib/library.svelte";
    import { trackStore } from "$lib/tracks.svelte";
    import { player } from "$lib/player.svelte";
    import YoutubeSearch from "$components/youtube-search.svelte";
    import FolderPlusIcon from "@lucide/svelte/icons/folder-plus";
    import RefreshCwIcon from "@lucide/svelte/icons/refresh-cw";
    import Trash2Icon from "@lucide/svelte/icons/trash-2";
    import PlayIcon from "@lucide/svelte/icons/play";
    import PauseIcon from "@lucide/svelte/icons/pause";
    import PencilIcon from "@lucide/svelte/icons/pencil";
    import CheckIcon from "@lucide/svelte/icons/check";
    import XIcon from "@lucide/svelte/icons/x";
    import DownloadIcon from "@lucide/svelte/icons/download";
    import LoaderIcon from "@lucide/svelte/icons/loader-circle";
    import { Input } from "$components/ui/input";
    import type { Track } from "$lib/tracks.svelte";

    onMount(() => {
        library.load();
        trackStore.load();
        player.restorePreferences();

        const scans = trackStore.listenForScans();
        const playback = player.listenForPlayer();

        return () => {
            scans.then((off) => off());
            playback.then((off) => off());
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

    /** The visible list is the queue, snapshotted at click time. */
    const queueIds = $derived(trackStore.tracks.map((t) => t.id));

    // Inline metadata editing. Only one row is editable at a time.
    let editingId = $state<number | null>(null);
    let editTitle = $state("");
    let editArtist = $state("");

    function startEdit(track: Track) {
        editingId = track.id;
        editTitle = track.title;
        editArtist = track.artist ?? "";
    }

    function cancelEdit() {
        editingId = null;
    }

    async function saveEdit(trackId: number) {
        // An empty artist means "unknown", which the backend stores as NULL.
        const artist = editArtist.trim() === "" ? null : editArtist;
        const saved = await trackStore.updateMetadata(trackId, editTitle, artist);
        if (saved) editingId = null;
    }
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

    <YoutubeSearch />

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
                {#each trackStore.tracks as track, index (track.id)}
                    {@const isCurrent = player.isCurrent(track.id)}
                    {@const isPlaying = isCurrent && player.state === "playing"}
                    <li
                        class="flex items-center gap-2 border-b py-1 text-sm last:border-b-0"
                        class:opacity-50={track.state === "missing"}
                    >
                        <Button
                            variant="ghost"
                            size="icon"
                            aria-label={isPlaying
                                ? `Pause ${track.title}`
                                : `Play ${track.title}`}
                            onclick={() =>
                                isCurrent
                                    ? player.togglePlayPause()
                                    : player.playQueue(queueIds, index)}
                        >
                            {#if isPlaying}
                                <PauseIcon />
                            {:else}
                                <PlayIcon />
                            {/if}
                        </Button>

                        {#if editingId === track.id}
                            <!-- Inline rather than a dialog: two fields do not
                                 justify a modal. -->
                            <div class="flex min-w-0 flex-1 items-center gap-2">
                                <Input
                                    bind:value={editTitle}
                                    placeholder="Title"
                                    class="h-8"
                                    onkeydown={(e) => {
                                        if (e.key === "Enter") saveEdit(track.id);
                                        if (e.key === "Escape") cancelEdit();
                                    }}
                                />
                                <Input
                                    bind:value={editArtist}
                                    placeholder="Artist"
                                    class="h-8"
                                    onkeydown={(e) => {
                                        if (e.key === "Enter") saveEdit(track.id);
                                        if (e.key === "Escape") cancelEdit();
                                    }}
                                />
                                <Button
                                    variant="ghost"
                                    size="icon"
                                    aria-label="Save"
                                    onclick={() => saveEdit(track.id)}
                                >
                                    <CheckIcon />
                                </Button>
                                <Button
                                    variant="ghost"
                                    size="icon"
                                    aria-label="Cancel"
                                    onclick={cancelEdit}
                                >
                                    <XIcon />
                                </Button>
                            </div>
                        {:else}
                            <span class="min-w-0 flex-1 truncate">
                                <span class:font-medium={isCurrent}>
                                    {track.title}
                                </span>
                                <span class="text-muted-foreground">
                                    — {track.artist ?? "Unknown artist"}
                                </span>
                                {#if track.state === "missing"}
                                    <span class="text-muted-foreground text-xs">
                                        (missing)
                                    </span>
                                {:else if track.state === "saved"}
                                    <span
                                        class="text-muted-foreground border-muted-foreground/40 ml-1 rounded border px-1 text-[10px]"
                                        title="Streams from YouTube; needs internet"
                                    >
                                        streaming
                                    </span>
                                {:else if track.state === "downloaded"}
                                    <span
                                        class="text-primary border-primary/40 ml-1 rounded border px-1 text-[10px]"
                                        title="Saved to disk; plays offline"
                                    >
                                        offline
                                    </span>
                                {/if}
                            </span>

                            <span class="text-muted-foreground shrink-0 text-xs">
                                {formatDuration(track.durationSecs)}
                            </span>

                            <Button
                                variant="ghost"
                                size="icon"
                                aria-label="Edit {track.title}"
                                onclick={() => startEdit(track)}
                            >
                                <PencilIcon />
                            </Button>

                            {#if track.source === "youtube"}
                                {#if track.state === "saved"}
                                    <Button
                                        variant="ghost"
                                        size="icon"
                                        aria-label="Download {track.title}"
                                        disabled={trackStore.isDownloading(track.id)}
                                        onclick={() => trackStore.download(track.id)}
                                    >
                                        {#if trackStore.isDownloading(track.id)}
                                            <LoaderIcon class="animate-spin" />
                                        {:else}
                                            <DownloadIcon />
                                        {/if}
                                    </Button>
                                {:else}
                                    <Button
                                        variant="ghost"
                                        size="icon"
                                        aria-label="Delete download of {track.title}"
                                        onclick={() =>
                                            trackStore.deleteDownload(track.id)}
                                    >
                                        <Trash2Icon />
                                    </Button>
                                {/if}
                            {/if}
                        {/if}
                    </li>
                {/each}
            </ul>
        {/if}
    </section>
</main>
