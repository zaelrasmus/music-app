<script lang="ts">
    import * as DropdownMenu from "$components/ui/dropdown-menu";
    import { Button } from "$components/ui/button";
    import { playlistStore } from "$lib/playlists.svelte";
    import ListPlusIcon from "@lucide/svelte/icons/list-plus";
    import PlusIcon from "@lucide/svelte/icons/plus";

    interface Props {
        /**
         * Produces the track id to add.
         *
         * A function rather than a value because a YouTube search result is
         * not a track yet — it has to be saved first, and that should only
         * happen if the user actually picks a playlist.
         */
        resolveTrackId: () => Promise<number | null>;
        label?: string;
    }

    let { resolveTrackId, label = "Add to playlist" }: Props = $props();

    let busy = $state(false);

    async function addTo(playlistId: number) {
        busy = true;
        try {
            const trackId = await resolveTrackId();
            if (trackId === null) return;
            await playlistStore.addTracks(playlistId, [trackId]);
        } finally {
            busy = false;
        }
    }

    async function addToNew() {
        busy = true;
        try {
            const name = window.prompt("New playlist name");
            if (name === null || name.trim() === "") return;

            const playlist = await playlistStore.create(name);
            if (!playlist) return;

            const trackId = await resolveTrackId();
            if (trackId === null) return;
            await playlistStore.addTracks(playlist.id, [trackId]);
        } finally {
            busy = false;
        }
    }
</script>

<DropdownMenu.Root>
    <DropdownMenu.Trigger>
        {#snippet child({ props })}
            <Button
                {...props}
                variant="ghost"
                size="icon"
                aria-label={label}
                title={label}
                disabled={busy}
            >
                <ListPlusIcon />
            </Button>
        {/snippet}
    </DropdownMenu.Trigger>

    <DropdownMenu.Content align="end" class="w-56">
        <DropdownMenu.Group>
            <DropdownMenu.GroupHeading>Add to playlist</DropdownMenu.GroupHeading>
            {#each playlistStore.playlists as playlist (playlist.id)}
                <DropdownMenu.Item onSelect={() => addTo(playlist.id)}>
                    <span class="truncate">{playlist.name}</span>
                    <span class="text-muted-foreground ml-auto text-xs">
                        {playlist.trackCount}
                    </span>
                </DropdownMenu.Item>
            {/each}

            {#if playlistStore.playlists.length === 0}
                <DropdownMenu.Item disabled>No playlists yet</DropdownMenu.Item>
            {/if}
        </DropdownMenu.Group>

        <DropdownMenu.Separator />

        <DropdownMenu.Item onSelect={addToNew}>
            <PlusIcon />
            New playlist…
        </DropdownMenu.Item>
    </DropdownMenu.Content>
</DropdownMenu.Root>
