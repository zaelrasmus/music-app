<script lang="ts">
    import WifiOffIcon from "@lucide/svelte/icons/wifi-off";
    import CloudIcon from "@lucide/svelte/icons/cloud";
    import HardDriveDownloadIcon from "@lucide/svelte/icons/hard-drive-download";

    /**
     * Where a track came from and whether it needs the network.
     *
     * One component rather than three copies: the library, playlists and the
     * queue all showed this, and the markup had already drifted apart before a
     * second provider made it ambiguous as well.
     *
     * Reduced to an icon plus a word. The old version spelled out
     * "YouTube · offline" on every row, which at forty rows is forty repetitions
     * of a fact the user learns once. The provider is the constant; whether it
     * plays offline is the part that actually changes between rows, so that is
     * what keeps the colour.
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
        /**
         * A cached copy exists, so this plays without a connection -- but only
         * until the cache needs the room back.
         */
        cached?: boolean;
        /** Smaller type, for the queue panel's tighter rows. */
        compact?: boolean;
    }

    let {
        source,
        state,
        durationSecs = null,
        cached = false,
        compact = false,
    }: Props = $props();

    const PROVIDER_NAMES: Record<string, string> = {
        youtube: "YouTube",
        soundcloud: "SoundCloud",
    };

    const providerName = $derived(PROVIDER_NAMES[source] ?? null);

    const box = $derived(
        `inline-flex shrink-0 items-center gap-1 rounded-full border px-1.5 ${
            compact ? "py-0 text-[10px]" : "py-[1px] text-[10px]"
        }`,
    );

    /**
     * Same inference as the search results, applied to tracks already saved.
     *
     * Answers "why does this song stop at 30 seconds" wherever the track shows
     * up, not just in the search list where it was chosen.
     */
    const preview = $derived(source === "soundcloud" && durationSecs === 30);
</script>

{#if state === "missing"}
    <span
        class="{box} border-destructive/40 text-destructive"
        title="The file is not where the library expects it. Rescan after moving it back."
    >
        missing
    </span>
{:else if providerName}
    {#if state === "downloaded"}
        <!-- Kept by the user, on purpose. The strongest promise available, so
             it gets the strongest colour. -->
        <span
            class="{box} border-signal/40 text-signal"
            title="Saved to disk from {providerName}. Yours to keep — plays offline until you delete it."
        >
            <HardDriveDownloadIcon class="size-3" />
            saved
        </span>
    {:else if cached}
        <!-- The same promise as a download -- it plays offline -- but
             deliberately not the same weight, because this copy is ours to
             reclaim rather than the listener's to keep. -->
        <span
            class="{box} border-signal/25 text-signal/80"
            title="Kept from an earlier play on {providerName}, so it works offline. May be removed to free space."
        >
            <WifiOffIcon class="size-3" />
            offline
        </span>
    {:else}
        <span
            class="{box} border-border text-muted-foreground"
            title="Streams from {providerName}; needs internet"
        >
            <CloudIcon class="size-3" />
            {providerName}
        </span>
    {/if}

    {#if preview}
        <span
            class="{box} border-border text-muted-foreground"
            title="SoundCloud only serves a 30-second snippet for this upload (Go+ gated). Another upload of the same song may be full length."
        >
            30s
        </span>
    {/if}
{/if}
