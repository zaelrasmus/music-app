<script lang="ts">
    /**
     * Where a track came from and whether it needs the network.
     *
     * One component rather than three copies: the library, playlists and the
     * queue all showed this, and the markup had already drifted apart before a
     * second provider made it ambiguous as well.
     */
    interface Props {
        /** `tracks.source`: "local", "youtube", "soundcloud". */
        source: string;
        /** "present" | "missing" | "saved" | "downloaded". */
        state: string;
        /**
         * Used only to spot a SoundCloud Go+ snippet, which reports its length
         * as exactly 30 seconds.
         */
        durationSecs?: number | null;
        /** Smaller type, for the queue panel's tighter rows. */
        compact?: boolean;
    }

    let { source, state, durationSecs = null, compact = false }: Props = $props();

    const PROVIDER_NAMES: Record<string, string> = {
        youtube: "YouTube",
        soundcloud: "SoundCloud",
    };

    const providerName = $derived(PROVIDER_NAMES[source] ?? null);
    const size = $derived(compact ? "text-[10px]" : "text-[10px]");

    /**
     * Same inference as the search results, applied to tracks already saved.
     *
     * Answers "why does this song stop at 30 seconds" wherever the track shows
     * up, not just in the search list where it was chosen.
     */
    const preview = $derived(source === "soundcloud" && durationSecs === 30);
</script>

{#if state === "missing"}
    <span class="text-muted-foreground {size}">(missing)</span>
{:else if providerName}
    {#if state === "downloaded"}
        <span
            class="text-primary border-primary/40 rounded border px-1 {size}"
            title="Saved to disk from {providerName}; plays offline"
        >
            {providerName} · offline
        </span>
    {:else}
        <span
            class="text-muted-foreground border-muted-foreground/40 rounded border px-1 {size}"
            title="Streams from {providerName}; needs internet"
        >
            {providerName}
        </span>
    {/if}

    {#if preview}
        <span
            class="text-muted-foreground border-muted-foreground/40 rounded border px-1 {size}"
            title="SoundCloud only serves a 30-second snippet for this upload (Go+ gated). Another upload of the same song may be full length."
        >
            30s preview
        </span>
    {/if}
{/if}
