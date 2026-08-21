<script lang="ts">
    import CoverArt from "$components/cover-art.svelte";
    import PlayIcon from "@lucide/svelte/icons/play";
    import ShuffleIcon from "@lucide/svelte/icons/shuffle";
    import type { Snippet } from "svelte";

    /**
     * The head of a track list.
     *
     * Lifted from the demo, which puts `Play / Shuffle / 56 songs` in a single
     * row above the list. It is the best thing in that design: the two actions
     * anyone actually wants are one click away without touching a row, and the
     * count sits with them instead of being tucked into a heading.
     *
     * The block above it is the upgrade. The demo's list views open straight
     * onto a toolbar with no name and no artwork, so a 56-track playlist does
     * not say which playlist it is -- and a two-track one reads as broken
     * rather than as small. Giving the list an identity fixes both, and costs
     * nothing but the space it fills with something worth reading.
     */
    interface Props {
        /** "Playlist", "Recently played" — what kind of thing this is. */
        eyebrow?: string;
        title: string;
        /** Text the generated artwork derives from. Omit for no artwork. */
        cover?: string | null;
        /** A real image, when the user chose one. Falls back to generated. */
        coverKey?: string | null;
        /** A remote picture, for an artist whose avatar is not stored. */
        coverSrc?: string | null;
        /**
         * Draws the head as a person rather than as a collection.
         *
         * A round, larger picture, because that is what a circle means
         * everywhere else in this app and in every other music player: a
         * collection is a square, a person is not. A playlist that fills
         * itself from one artist *is* that artist's page, and looking like a
         * different kind of thing than the provider's page for the same person
         * is the confusion worth avoiding.
         */
        artist?: boolean;
        /** "56 songs", "3h 12m", "12 offline" — joined with separators. */
        meta?: (string | null)[];
        onPlay?: () => void;
        onShuffle?: () => void;
        /** Both actions are meaningless on an empty list. */
        empty?: boolean;
        /** Sits before the title: a back button, when a view has depth. */
        leading?: Snippet;
        /** Right-aligned controls on the action row. */
        actions?: Snippet;
        /** Search boxes and filters, below the action row. */
        toolbar?: Snippet;
    }

    let {
        eyebrow,
        title,
        cover = null,
        coverKey = null,
        coverSrc = null,
        artist = false,
        meta = [],
        onPlay,
        onShuffle,
        empty = false,
        leading,
        actions,
        toolbar,
    }: Props = $props();

    const parts = $derived(meta.filter((m): m is string => !!m));

    /** The white pill. One per screen, on the action everyone came for. */
    const solid =
        "inline-flex h-9 shrink-0 items-center gap-2 rounded-full bg-primary px-4 text-[13px] font-medium text-primary-foreground transition-opacity hover:opacity-90 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background disabled:opacity-40";
    const outline =
        "inline-flex h-9 shrink-0 items-center gap-2 rounded-full border border-border px-4 text-[13px] font-medium transition-colors hover:bg-accent focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40";
</script>

<div class="flex flex-col gap-4">
    <div class="flex items-end gap-4">
        {#if leading}
            <div class="flex shrink-0 items-center self-center">
                {@render leading()}
            </div>
        {/if}

        {#if cover !== null}
            <CoverArt
                seed={cover}
                {coverKey}
                src={coverSrc}
                class={artist
                    ? "size-28 rounded-full shadow-sm sm:size-32"
                    : "size-[104px] rounded-xl"}
                glyph={false}
            />
        {/if}

        <div class="flex min-w-0 flex-1 flex-col gap-1">
            {#if eyebrow}
                <span
                    class="text-muted-foreground text-[11px] font-semibold tracking-[0.08em] uppercase"
                >
                    {eyebrow}
                </span>
            {/if}

            <!-- Big and tight. The demo's headings are the only large type on
                 screen, which is what lets everything else stay quiet. -->
            <h1
                class="truncate text-[28px] leading-[1.15] font-bold tracking-[-0.02em]"
                {title}
            >
                {title}
            </h1>

            {#if parts.length > 0}
                <p class="text-muted-foreground truncate text-[13px]">
                    {parts.join(" · ")}
                </p>
            {/if}
        </div>
    </div>

    <div class="flex flex-wrap items-center gap-2">
        {#if onPlay}
            <button type="button" class={solid} disabled={empty} onclick={onPlay}>
                <PlayIcon class="size-4 fill-current" />
                Play
            </button>
        {/if}
        {#if onShuffle}
            <button type="button" class={outline} disabled={empty} onclick={onShuffle}>
                <ShuffleIcon class="size-4" />
                Shuffle
            </button>
        {/if}

        {#if actions}
            <div class="ml-auto flex items-center gap-2">{@render actions()}</div>
        {/if}
    </div>

    {#if toolbar}
        <div class="flex flex-col gap-2">{@render toolbar()}</div>
    {/if}
</div>
