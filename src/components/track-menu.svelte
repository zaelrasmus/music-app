<script lang="ts">
    import * as DropdownMenu from "$components/ui/dropdown-menu";
    import { player } from "$lib/player.svelte";
    import { playlistStore } from "$lib/playlists.svelte";
    import { trackStore, type Track } from "$lib/tracks.svelte";
    import { promptFor } from "$lib/prompt.svelte";
    import MoreHorizontalIcon from "@lucide/svelte/icons/more-horizontal";
    import CornerUpRightIcon from "@lucide/svelte/icons/corner-up-right";
    import ListPlusIcon from "@lucide/svelte/icons/list-plus";
    import PlusIcon from "@lucide/svelte/icons/plus";
    import PencilIcon from "@lucide/svelte/icons/pencil";
    import TagIcon from "@lucide/svelte/icons/tag";
    import DownloadIcon from "@lucide/svelte/icons/download";
    import Trash2Icon from "@lucide/svelte/icons/trash-2";
    import LoaderIcon from "@lucide/svelte/icons/loader-circle";
    import type { Snippet } from "svelte";

    /**
     * Everything you can do to a track, in one menu.
     *
     * This replaces the six always-visible icon buttons each row used to carry.
     * Six buttons on forty rows is two hundred and forty targets competing for
     * attention with the track titles, and the titles are what the list is for.
     * They also forced every action to be explainable as a single glyph, which
     * is why "download" and "delete the download" looked like unrelated things.
     *
     * The two queue actions stay at the top because they are the ones used
     * mid-listen, when the menu is being opened by muscle memory.
     */
    interface Props {
        /**
         * Produces the track id to act on.
         *
         * A function rather than a value because a search result is not a track
         * yet -- it has to be saved first, and that should only happen if the
         * user actually picks something.
         */
        resolveTrackId: () => Promise<number | null>;
        label?: string;
        /** Present for saved tracks; absent for search results. */
        track?: Track | null;
        onEdit?: () => void;
        onTag?: () => void;
        /** Context-specific items, e.g. "Remove from this playlist". */
        extra?: Snippet;
        align?: "start" | "end";
        /** Rows reveal the trigger on hover; standalone callers do not. */
        trigger?: string;
    }

    let {
        resolveTrackId,
        label = "More actions",
        track = null,
        onEdit,
        onTag,
        extra,
        align = "end",
        trigger = "",
    }: Props = $props();

    let busy = $state(false);

    const downloading = $derived(
        track !== null && trackStore.isDownloading(track.id),
    );

    async function run(action: (trackId: number) => Promise<unknown>) {
        busy = true;
        try {
            const trackId = await resolveTrackId();
            if (trackId === null) return;
            await action(trackId);
        } finally {
            busy = false;
        }
    }

    async function addToNew() {
        const name = await promptFor("New playlist", {
            label: "What should it be called?",
            placeholder: "Playlist name",
            confirmLabel: "Create",
        });
        if (name === null) return;

        const playlist = await playlistStore.create(name);
        if (!playlist) return;

        await run((id) => playlistStore.addTracks(playlist.id, [id]));
    }
</script>

<DropdownMenu.Root>
    <DropdownMenu.Trigger>
        {#snippet child({ props })}
            <button
                {...props}
                type="button"
                class="text-muted-foreground hover:bg-accent hover:text-foreground data-open:bg-accent data-open:text-foreground inline-grid size-8 shrink-0 place-items-center rounded-md transition-colors disabled:opacity-50 {trigger}"
                aria-label={label}
                title={label}
                disabled={busy}
            >
                {#if busy}
                    <LoaderIcon class="size-4 animate-spin" />
                {:else}
                    <MoreHorizontalIcon class="size-4" />
                {/if}
            </button>
        {/snippet}
    </DropdownMenu.Trigger>

    <DropdownMenu.Content {align} class="w-56">
        <DropdownMenu.Item onSelect={() => run((id) => player.playNext(id))}>
            <CornerUpRightIcon />
            Play next
        </DropdownMenu.Item>
        <DropdownMenu.Item onSelect={() => run((id) => player.addToQueue(id))}>
            <ListPlusIcon />
            Add to queue
        </DropdownMenu.Item>

        <DropdownMenu.Separator />

        <DropdownMenu.Sub>
            <DropdownMenu.SubTrigger>
                <PlusIcon />
                Add to playlist
            </DropdownMenu.SubTrigger>
            <DropdownMenu.SubContent class="max-h-72 w-56 overflow-y-auto">
                {#each playlistStore.playlists as playlist (playlist.id)}
                    <DropdownMenu.Item
                        onSelect={() =>
                            run((id) => playlistStore.addTracks(playlist.id, [id]))}
                    >
                        <span class="truncate">{playlist.name}</span>
                        <span class="text-muted-foreground ml-auto text-xs tabular-nums">
                            {playlist.trackCount}
                        </span>
                    </DropdownMenu.Item>
                {/each}

                {#if playlistStore.playlists.length > 0}
                    <DropdownMenu.Separator />
                {/if}

                <DropdownMenu.Item onSelect={addToNew}>
                    <PlusIcon />
                    New playlist…
                </DropdownMenu.Item>
            </DropdownMenu.SubContent>
        </DropdownMenu.Sub>

        {#if onEdit || onTag}
            <DropdownMenu.Separator />
            {#if onTag}
                <DropdownMenu.Item onSelect={onTag}>
                    <TagIcon />
                    Add a tag
                </DropdownMenu.Item>
            {/if}
            {#if onEdit}
                <DropdownMenu.Item onSelect={onEdit}>
                    <PencilIcon />
                    Edit title and artist
                </DropdownMenu.Item>
            {/if}
        {/if}

        <!-- Any provider track can be downloaded for offline playback; only
             local files have nothing to fetch. -->
        {#if track && track.source !== "local"}
            <DropdownMenu.Separator />
            {#if track.state === "saved"}
                <DropdownMenu.Item
                    disabled={downloading}
                    onSelect={() => trackStore.download(track.id)}
                >
                    {#if downloading}
                        <LoaderIcon class="animate-spin" />
                        Downloading…
                    {:else}
                        <DownloadIcon />
                        Download to keep
                    {/if}
                </DropdownMenu.Item>
            {:else}
                <DropdownMenu.Item
                    onSelect={() => trackStore.deleteDownload(track.id)}
                >
                    <Trash2Icon />
                    Delete the download
                </DropdownMenu.Item>
            {/if}
        {/if}

        {#if extra}
            <DropdownMenu.Separator />
            {@render extra()}
        {/if}
    </DropdownMenu.Content>
</DropdownMenu.Root>
