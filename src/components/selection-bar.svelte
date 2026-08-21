<script lang="ts">
    import { selection } from "$lib/selection.svelte";
    import { playlistStore } from "$lib/playlists.svelte";
    import { trackStore, setManyInLibrary, setManyArtists } from "$lib/tracks.svelte";
    import { tagStore } from "$lib/tags.svelte";
    import { promptFor } from "$lib/prompt.svelte";
    import * as DropdownMenu from "$components/ui/dropdown-menu";
    import XIcon from "@lucide/svelte/icons/x";
    import ListPlusIcon from "@lucide/svelte/icons/list-plus";
    import LibraryBigIcon from "@lucide/svelte/icons/library-big";
    import PencilIcon from "@lucide/svelte/icons/pencil";
    import TagIcon from "@lucide/svelte/icons/tag";
    import PlusIcon from "@lucide/svelte/icons/plus";

    interface Props {
        /** The list as displayed, for "select all". */
        order: number[];
        /** Offered only where removing means something, e.g. inside a playlist. */
        onRemove?: (trackIds: number[]) => void;
        removeLabel?: string;
    }

    let { order, onRemove, removeLabel = "Remove" }: Props = $props();

    const count = $derived(selection.count);

    /** Everything the actions need, taken once so a reload cannot shift it. */
    function taken() {
        return [...selection.ids];
    }

    async function addToPlaylist(playlistId: number) {
        await playlistStore.addTracks(playlistId, taken());
        selection.clear();
    }

    async function addToNewPlaylist() {
        const name = await promptFor("New playlist", { confirmLabel: "Create" });
        if (!name) return;

        const created = await playlistStore.create(name);
        if (created) await addToPlaylist(created.id);
    }

    async function file(inLibrary: boolean) {
        await setManyInLibrary(taken(), inLibrary);
        await trackStore.load();
        selection.clear();
    }

    /**
     * The reason this whole feature exists.
     *
     * A library scanned from files with no artist tag has the artist in the
     * folder name and nowhere else. Naming a folder's worth of tracks at once
     * is the only way that gets fixed without opening a thousand dialogs.
     */
    async function setArtist() {
        const artist = await promptFor(
            `Set artist on ${count} ${count === 1 ? "track" : "tracks"}`,
            { label: "Artist", confirmLabel: "Set" },
        );
        if (artist === null) return;

        await setManyArtists(taken(), artist);
        await trackStore.load();
        selection.clear();
    }

    async function tag(name: string) {
        await tagStore.assignMany(taken(), name);
        selection.clear();
    }

    async function tagNew() {
        const name = await promptFor("New tag", { confirmLabel: "Add" });
        if (name) await tag(name);
    }
</script>

<!--
  Appears only when something is picked, and sits above the list rather than
  replacing its toolbar: what you selected stays visible behind it, which is
  what makes "12 selected" checkable rather than a claim.
-->
{#if selection.active}
    <div
        class="bg-primary/10 border-primary/25 mb-3 flex flex-wrap items-center gap-2 rounded-lg border px-3 py-2"
    >
        <span class="text-[13px] font-medium tabular-nums">
            {count} selected
        </span>

        {#if count < order.length}
            <button
                type="button"
                class="text-muted-foreground hover:text-foreground text-xs underline underline-offset-2 transition-colors"
                onclick={() => selection.selectAll(order)}
            >
                Select all {order.length}
            </button>
        {/if}

        <div class="flex-1"></div>

        <DropdownMenu.Root>
            <DropdownMenu.Trigger>
                {#snippet child({ props })}
                    <button
                        {...props}
                        type="button"
                        class="hover:bg-accent flex items-center gap-1.5 rounded-full border px-3 py-1 text-xs transition-colors"
                    >
                        <ListPlusIcon class="size-3.5" />
                        Add to playlist
                    </button>
                {/snippet}
            </DropdownMenu.Trigger>
            <DropdownMenu.Content align="end" class="w-56">
                {#each playlistStore.playlists as playlist (playlist.id)}
                    <DropdownMenu.Item onSelect={() => addToPlaylist(playlist.id)}>
                        {playlist.name}
                    </DropdownMenu.Item>
                {/each}
                {#if playlistStore.playlists.length > 0}
                    <DropdownMenu.Separator />
                {/if}
                <DropdownMenu.Item onSelect={addToNewPlaylist}>
                    <PlusIcon />
                    New playlist…
                </DropdownMenu.Item>
            </DropdownMenu.Content>
        </DropdownMenu.Root>

        <DropdownMenu.Root>
            <DropdownMenu.Trigger>
                {#snippet child({ props })}
                    <button
                        {...props}
                        type="button"
                        class="hover:bg-accent flex items-center gap-1.5 rounded-full border px-3 py-1 text-xs transition-colors"
                    >
                        <TagIcon class="size-3.5" />
                        Tag
                    </button>
                {/snippet}
            </DropdownMenu.Trigger>
            <DropdownMenu.Content align="end" class="w-56">
                {#each tagStore.tags as t (t.id)}
                    <DropdownMenu.Item onSelect={() => tag(t.name)}>
                        {t.name}
                    </DropdownMenu.Item>
                {/each}
                {#if tagStore.tags.length > 0}
                    <DropdownMenu.Separator />
                {/if}
                <DropdownMenu.Item onSelect={tagNew}>
                    <PlusIcon />
                    New tag…
                </DropdownMenu.Item>
            </DropdownMenu.Content>
        </DropdownMenu.Root>

        <button
            type="button"
            class="hover:bg-accent flex items-center gap-1.5 rounded-full border px-3 py-1 text-xs transition-colors"
            onclick={setArtist}
        >
            <PencilIcon class="size-3.5" />
            Set artist…
        </button>

        <button
            type="button"
            class="hover:bg-accent flex items-center gap-1.5 rounded-full border px-3 py-1 text-xs transition-colors"
            onclick={() => file(true)}
        >
            <LibraryBigIcon class="size-3.5" />
            Add to library
        </button>

        {#if onRemove}
            <button
                type="button"
                class="text-destructive hover:bg-destructive/10 rounded-full border px-3 py-1 text-xs transition-colors"
                onclick={() => {
                    onRemove(taken());
                    selection.clear();
                }}
            >
                {removeLabel}
            </button>
        {/if}

        <button
            type="button"
            class="text-muted-foreground hover:bg-accent hover:text-foreground grid size-7 place-items-center rounded-full transition-colors"
            aria-label="Clear selection"
            title="Clear selection (Esc)"
            onclick={() => selection.clear()}
        >
            <XIcon class="size-3.5" />
        </button>
    </div>
{/if}
