<script lang="ts">
    import { Button } from "$components/ui/button";
    import { Input } from "$components/ui/input";
    import AddToPlaylist from "$components/add-to-playlist.svelte";
    import TrackActions from "$components/track-actions.svelte";
    import SourceBadge from "$components/source-badge.svelte";
    import { cacheStore } from "$lib/cache.svelte";
    import { player } from "$lib/player.svelte";
    import { trackStore, type Track } from "$lib/tracks.svelte";
    import { tagStore } from "$lib/tags.svelte";
    import PlayIcon from "@lucide/svelte/icons/play";
    import PauseIcon from "@lucide/svelte/icons/pause";
    import PencilIcon from "@lucide/svelte/icons/pencil";
    import CheckIcon from "@lucide/svelte/icons/check";
    import XIcon from "@lucide/svelte/icons/x";
    import DownloadIcon from "@lucide/svelte/icons/download";
    import Trash2Icon from "@lucide/svelte/icons/trash-2";
    import LoaderIcon from "@lucide/svelte/icons/loader-circle";
    import TagIcon from "@lucide/svelte/icons/tag";

    interface Props {
        track: Track;
        /** Ids forming the context when this row is played, in display order. */
        queueIds: number[];
        index: number;
        /** Shown as "Next from …" in the queue panel. */
        contextName?: string;
    }

    let {
        track,
        queueIds,
        index,
        contextName = "your library",
    }: Props = $props();

    const isCurrent = $derived(player.isCurrent(track.id));
    const isPlaying = $derived(isCurrent && player.state === "playing");
    const tags = $derived(tagStore.forTrack(track.id));

    let editing = $state(false);
    let editTitle = $state("");
    let editArtist = $state("");

    let tagging = $state(false);
    let newTag = $state("");

    function startEdit() {
        editTitle = track.title;
        editArtist = track.artist ?? "";
        editing = true;
    }

    async function saveEdit() {
        // An empty artist means "unknown", stored as NULL.
        const artist = editArtist.trim() === "" ? null : editArtist;
        if (await trackStore.updateMetadata(track.id, editTitle, artist)) {
            editing = false;
        }
    }

    async function commitTag() {
        if (newTag.trim() === "") {
            tagging = false;
            return;
        }
        await tagStore.assign(track.id, newTag);
        newTag = "";
        tagging = false;
    }

    function formatDuration(secs: number | null) {
        if (secs === null) return "";
        const m = Math.floor(secs / 60);
        const s = secs % 60;
        return `${m}:${String(s).padStart(2, "0")}`;
    }
</script>

<li
    class="flex flex-col gap-1 border-b py-1 text-sm last:border-b-0"
    class:opacity-50={track.state === "missing"}
>
    <div class="flex items-center gap-2">
        <Button
            variant="ghost"
            size="icon"
            aria-label={isPlaying ? `Pause ${track.title}` : `Play ${track.title}`}
            onclick={() =>
                isCurrent
                    ? player.togglePlayPause()
                    : player.playQueue(queueIds, index, contextName)}
        >
            {#if isPlaying}
                <PauseIcon />
            {:else}
                <PlayIcon />
            {/if}
        </Button>

        {#if editing}
            <div class="flex min-w-0 flex-1 items-center gap-2">
                <Input
                    bind:value={editTitle}
                    placeholder="Title"
                    class="h-8"
                    onkeydown={(e) => {
                        if (e.key === "Enter") saveEdit();
                        if (e.key === "Escape") editing = false;
                    }}
                />
                <Input
                    bind:value={editArtist}
                    placeholder="Artist"
                    class="h-8"
                    onkeydown={(e) => {
                        if (e.key === "Enter") saveEdit();
                        if (e.key === "Escape") editing = false;
                    }}
                />
                <Button variant="ghost" size="icon" aria-label="Save" onclick={saveEdit}>
                    <CheckIcon />
                </Button>
                <Button
                    variant="ghost"
                    size="icon"
                    aria-label="Cancel"
                    onclick={() => (editing = false)}
                >
                    <XIcon />
                </Button>
            </div>
        {:else}
            <span class="min-w-0 flex-1 truncate">
                <span class:font-medium={isCurrent}>{track.title}</span>
                <span class="text-muted-foreground">
                    — {track.artist ?? "Unknown artist"}
                </span>
                <SourceBadge
                    source={track.source}
                    state={track.state}
                    durationSecs={track.durationSecs}
                    cached={cacheStore.isCached(track.id)}
                />
            </span>

            <span class="text-muted-foreground shrink-0 text-xs">
                {formatDuration(track.durationSecs)}
            </span>

            <Button
                variant="ghost"
                size="icon"
                aria-label="Tag {track.title}"
                onclick={() => (tagging = !tagging)}
            >
                <TagIcon />
            </Button>

            <TrackActions
                resolveTrackId={async () => track.id}
                label="Queue {track.title}"
            />

            <AddToPlaylist
                resolveTrackId={async () => track.id}
                label="Add {track.title} to a playlist"
            />

            <Button
                variant="ghost"
                size="icon"
                aria-label="Edit {track.title}"
                onclick={startEdit}
            >
                <PencilIcon />
            </Button>

            <!-- Any provider track can be downloaded for offline playback;
                 only local files have nothing to fetch. -->
            {#if track.source !== "local"}
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
                        onclick={() => trackStore.deleteDownload(track.id)}
                    >
                        <Trash2Icon />
                    </Button>
                {/if}
            {/if}
        {/if}
    </div>

    {#if tags.length > 0 || tagging}
        <div class="flex flex-wrap items-center gap-1 pl-10">
            {#each tags as tag (tag.id)}
                <span
                    class="bg-muted text-muted-foreground flex items-center gap-1 rounded-full px-2 py-0.5 text-[11px]"
                >
                    {tag.name}
                    <button
                        type="button"
                        class="hover:text-foreground"
                        aria-label="Remove tag {tag.name}"
                        onclick={() => tagStore.remove(track.id, tag.id)}
                    >
                        <XIcon class="size-3" />
                    </button>
                </span>
            {/each}

            {#if tagging}
                <Input
                    bind:value={newTag}
                    placeholder="Tag name"
                    class="h-6 w-32 text-xs"
                    onkeydown={(e) => {
                        if (e.key === "Enter") commitTag();
                        if (e.key === "Escape") {
                            tagging = false;
                            newTag = "";
                        }
                    }}
                />
            {/if}
        </div>
    {/if}
</li>
