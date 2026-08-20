<script lang="ts">
    import CoverArt from "$components/cover-art.svelte";
    import type { Collection } from "$lib/provider-search.svelte";
    import ListMusicIcon from "@lucide/svelte/icons/list-music";
    import UserIcon from "@lucide/svelte/icons/user";
    import ChevronRightIcon from "@lucide/svelte/icons/chevron-right";

    interface Props {
        collection: Collection;
        onopen: () => void;
    }

    let { collection, onopen }: Props = $props();

    /**
     * An artist's picture is a face or a logo and reads as round; a playlist's
     * is cover art and reads as a card. Using one shape for both would leave
     * the two lists looking identical while meaning different things.
     */
    const artistLike = $derived(collection.kind === "artist");

    const subtitle = $derived.by(() => {
        const parts: string[] = [];
        if (collection.uploader) parts.push(collection.uploader);
        if (collection.itemCount !== null) {
            parts.push(
                `${collection.itemCount} ${collection.itemCount === 1 ? "track" : "tracks"}`,
            );
        }
        // Never empty: a row with a blank second line looks broken rather than
        // sparse, and the kind is always true.
        if (parts.length === 0) parts.push(artistLike ? "Artist" : "Playlist");
        return parts.join(" · ");
    });
</script>

<li>
    <button
        type="button"
        class="hover:bg-accent/50 group/collection flex w-full items-center gap-3 rounded-lg px-2 py-2 text-left transition-colors focus-visible:outline-none"
        onclick={onopen}
    >
        <div class="relative shrink-0">
            <CoverArt
                seed={`${collection.uploader ?? ""}::${collection.title}`}
                src={collection.thumbnailUrl}
                class={artistLike
                    ? "size-12 rounded-full"
                    : "aspect-video w-[5.5rem]"}
                glyph={false}
            />

            <!-- The kind, on the art, so a mixed list is never ambiguous. -->
            <span
                class="absolute -right-1 -bottom-1 grid size-5 place-items-center rounded-full border bg-background text-muted-foreground"
                aria-hidden="true"
            >
                {#if artistLike}
                    <UserIcon class="size-3" />
                {:else}
                    <ListMusicIcon class="size-3" />
                {/if}
            </span>
        </div>

        <div class="flex min-w-0 flex-1 flex-col gap-0.5">
            <span class="truncate text-sm leading-snug">{collection.title}</span>
            <span class="text-muted-foreground truncate text-xs">
                {subtitle}
            </span>
        </div>

        <ChevronRightIcon
            class="text-muted-foreground size-4 shrink-0 opacity-0 transition-opacity group-hover/collection:opacity-100"
        />
    </button>
</li>
