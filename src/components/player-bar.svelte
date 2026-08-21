<script lang="ts">
    import ScrubBar from "$components/scrub-bar.svelte";
    import CoverArt from "$components/cover-art.svelte";
    import SourceBadge from "$components/source-badge.svelte";
    import { coverSeed } from "$lib/cover";
    import { player } from "$lib/player.svelte";
    import { queueStore } from "$lib/queue.svelte";
    import { trackStore } from "$lib/tracks.svelte";
    import { cacheStore } from "$lib/cache.svelte";
    import ListMusicIcon from "@lucide/svelte/icons/list-music";
    import PlayIcon from "@lucide/svelte/icons/play";
    import PauseIcon from "@lucide/svelte/icons/pause";
    import SkipBackIcon from "@lucide/svelte/icons/skip-back";
    import SkipForwardIcon from "@lucide/svelte/icons/skip-forward";
    import ShuffleIcon from "@lucide/svelte/icons/shuffle";
    import RepeatIcon from "@lucide/svelte/icons/repeat";
    import Repeat1Icon from "@lucide/svelte/icons/repeat-1";
    import Volume2Icon from "@lucide/svelte/icons/volume-2";
    import Volume1Icon from "@lucide/svelte/icons/volume-1";
    import VolumeXIcon from "@lucide/svelte/icons/volume-x";
    import LoaderIcon from "@lucide/svelte/icons/loader-circle";
    import WifiOffIcon from "@lucide/svelte/icons/wifi-off";

    /**
     * What is playing, described by whichever list can describe it.
     *
     * `player.trackId` is the identity and nothing else gets a vote. The two
     * sources arrive as separate events — `player-state` carries the id,
     * `player-queue` carries the details — so between them the queue still
     * describes the *previous* track. Reading it unconditionally is what put
     * the last song's title and artwork over the new song's audio.
     *
     * The queue payload is preferred where it agrees, because the backend
     * hydrates it directly and so knows about a YouTube result saved seconds
     * ago that the library list has not reloaded yet.
     */
    const nowPlaying = $derived.by(() => {
        const id = player.trackId;
        if (id === null) return null;

        const queued = queueStore.current;
        if (queued?.trackId === id) return queued;

        return trackStore.tracks.find((t) => t.id === id) ?? null;
    });

    // Total comes from the scanned tag, not from rodio, whose total_duration
    // is None for several formats.
    /**
     * The playing track's artwork.
     *
     * `nowPlaying` is a queue entry or a library row depending on which
     * arrived first, and only the queue entry carries a cover key -- so this
     * reads it defensively rather than assuming a shape.
     */
    const coverKey = $derived(
        (nowPlaying as { coverKey?: string | null } | null)?.coverKey ?? null,
    );

    /** Same shape question as the cover key: both list types carry it. */
    const remoteThumbnail = $derived(
        (nowPlaying as { remoteThumbnailUrl?: string | null } | null)
            ?.remoteThumbnailUrl ?? null,
    );

    const totalSecs = $derived(nowPlaying?.durationSecs ?? 0);
    const loading = $derived(player.state === "loading");
    /**
     * Nothing is arriving. Deliberately not shown as an error: the buffer
     * absorbs short drops, so by the time this appears it is worth saying, and
     * it usually recovers on its own.
     */
    const stalled = $derived(player.stalled);
    // Nothing to seek within until audio is actually flowing.
    const seekable = $derived(
        totalSecs > 0 && player.state !== "stopped" && !loading,
    );

    // Kept within [0, max] so the bar never has to snap a value back at us.
    // On VBR files the real position can drift past the tag-reported duration.
    const sliderMax = $derived(Math.max(totalSecs, 1));
    const sliderValue = $derived(
        Math.min(Math.max(player.displaySecs, 0), sliderMax),
    );

    const repeatLabel = $derived(
        player.repeat === "one"
            ? "Repeat one"
            : player.repeat === "all"
              ? "Repeat all"
              : "Repeat off",
    );

    function formatTime(secs: number) {
        if (!Number.isFinite(secs) || secs < 0) return "0:00";
        const total = Math.floor(secs);
        const m = Math.floor(total / 60);
        const s = total % 60;
        return `${m}:${String(s).padStart(2, "0")}`;
    }

    /** Ghost buttons in the transport row; the play button is separate. */
    const ghost =
        "grid size-8 shrink-0 place-items-center rounded-md transition-colors hover:bg-accent focus-visible:outline-none focus-visible:bg-accent disabled:opacity-40 disabled:hover:bg-transparent";
    const on = "text-foreground";
    const off = "text-muted-foreground hover:text-foreground";
</script>

<!--
  The player bar sits on the background, not on a raised card. The demo draws
  it as part of the window rather than as a panel stuck to the bottom, and a
  single hairline is enough to separate it from the list above.
-->
<footer
    class="border-border/60 relative flex h-[78px] shrink-0 items-center gap-4 border-t px-4"
>
    <!--
      Left: what is playing.
      Wider than the demo's, which truncated the title while leaving a large
      empty gap before the transport. The centre column is fixed, so this can
      take whatever the window has spare.
    -->
    <div class="flex min-w-0 flex-1 items-center gap-3">
        {#if nowPlaying}
            <CoverArt
                seed={coverSeed(nowPlaying)}
                coverKey={coverKey}
                src={remoteThumbnail}
                class="size-[52px] rounded-lg"
            />
            <div class="flex min-w-0 flex-col gap-0.5">
                <span class="selectable truncate text-[13px] leading-tight font-medium">
                    {nowPlaying.title}
                </span>
                <span class="text-muted-foreground flex items-center gap-1.5 text-xs leading-tight">
                    {#if stalled}
                        <span class="text-foreground/80 flex items-center gap-1">
                            <WifiOffIcon class="size-3" />
                            Reconnecting…
                        </span>
                    {:else if loading}
                        <span>Loading…</span>
                    {:else}
                        <span class="selectable truncate">
                            {nowPlaying.artist ?? "Unknown artist"}
                        </span>
                        <SourceBadge
                            source={nowPlaying.source}
                            state={nowPlaying.state}
                            durationSecs={nowPlaying.durationSecs}
                            cached={player.trackId !== null &&
                                cacheStore.isCached(player.trackId)}
                            compact
                        />
                    {/if}
                </span>
            </div>
        {:else}
            <div class="bg-muted grid size-[52px] shrink-0 place-items-center rounded-lg">
                <ListMusicIcon class="text-muted-foreground size-5" />
            </div>
            <span class="text-muted-foreground text-[13px]">Nothing playing</span>
        {/if}
    </div>

    <!-- Centre: transport over the seek bar. Fixed width, so the controls stay
         put as the title beside them changes length. -->
    <div class="flex w-[34rem] max-w-[45%] shrink-0 flex-col items-center gap-1">
        <div class="flex items-center gap-1">
            <button
                type="button"
                class="{ghost} {player.shuffle ? on : off}"
                aria-label="Shuffle"
                title={player.shuffle ? "Shuffle on" : "Shuffle off"}
                aria-pressed={player.shuffle}
                onclick={() => player.toggleShuffle()}
            >
                <ShuffleIcon class="size-4" />
            </button>

            <button
                type="button"
                class="{ghost} {off}"
                aria-label="Previous track"
                title="Previous"
                onclick={() => player.previous()}
            >
                <SkipBackIcon class="size-[18px] fill-current" />
            </button>

            <!-- The one solid control. Everything else on this bar is a
                 modifier; this is the verb. -->
            <button
                type="button"
                class="bg-foreground text-background grid size-9 shrink-0 place-items-center rounded-full transition-transform hover:scale-105 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background active:scale-95 disabled:opacity-50 disabled:hover:scale-100"
                aria-label={loading
                    ? "Loading"
                    : player.state === "playing"
                      ? "Pause"
                      : "Play"}
                disabled={loading || !nowPlaying}
                onclick={() => player.togglePlayPause()}
            >
                {#if loading}
                    <LoaderIcon class="size-4 animate-spin" />
                {:else if player.state === "playing"}
                    <PauseIcon class="size-4 fill-current" />
                {:else}
                    <PlayIcon class="size-4 translate-x-[1px] fill-current" />
                {/if}
            </button>

            <button
                type="button"
                class="{ghost} {off}"
                aria-label="Next track"
                title="Next"
                onclick={() => player.next()}
            >
                <SkipForwardIcon class="size-[18px] fill-current" />
            </button>

            <button
                type="button"
                class="{ghost} {player.repeat !== 'off' ? on : off}"
                aria-label={repeatLabel}
                title={repeatLabel}
                aria-pressed={player.repeat !== "off"}
                onclick={() => player.cycleRepeat()}
            >
                {#if player.repeat === "one"}
                    <Repeat1Icon class="size-4" />
                {:else}
                    <RepeatIcon class="size-4" />
                {/if}
            </button>
        </div>

        <div class="flex w-full items-center gap-2.5">
            <span
                class="text-muted-foreground w-9 shrink-0 text-right text-[11px] tabular-nums"
            >
                {formatTime(player.displaySecs)}
            </span>

            <ScrubBar
                value={sliderValue}
                max={sliderMax}
                step={5}
                disabled={!seekable}
                label="Seek"
                valueText="{formatTime(player.displaySecs)} of {formatTime(totalSecs)}"
                onScrub={(v) => player.scrubTo(v)}
                onCommit={(v) => player.commitScrub(v)}
            />

            <span class="text-muted-foreground w-9 shrink-0 text-[11px] tabular-nums">
                {formatTime(totalSecs)}
            </span>
        </div>
    </div>

    <!-- Right: volume and the queue. -->
    <div class="flex min-w-0 flex-1 items-center justify-end gap-1">
        <button
            type="button"
            class="{ghost} {off}"
            aria-label={player.muted ? "Unmute" : "Mute"}
            title={player.muted ? "Unmute" : "Mute"}
            onclick={() => player.toggleMute()}
        >
            {#if player.muted || player.volume === 0}
                <VolumeXIcon class="size-4" />
            {:else if player.volume < 0.5}
                <Volume1Icon class="size-4" />
            {:else}
                <Volume2Icon class="size-4" />
            {/if}
        </button>

        <ScrubBar
            class="w-24 shrink-0"
            value={player.muted ? 0 : player.volume}
            max={1}
            step={0.05}
            label="Volume"
            valueText="{Math.round((player.muted ? 0 : player.volume) * 100)}%"
            onScrub={(v) => player.setVolume(v)}
        />

        <div class="ml-2 flex shrink-0 items-center gap-1">
            <button
                type="button"
                class="{ghost} {queueStore.open ? on : off}"
                aria-label="Up next"
                title="Up next"
                aria-pressed={queueStore.open}
                onclick={() => queueStore.toggle()}
            >
                <ListMusicIcon class="size-4" />
            </button>

            {#if player.manualLength > 0}
                <!-- The queue is the thing worth a badge; the context position
                     is not, and would read as a total. -->
                <span
                    class="bg-primary text-primary-foreground min-w-4 rounded-full px-1 text-center text-[10px] leading-4 font-medium tabular-nums"
                >
                    {player.manualLength}
                </span>
            {:else if player.contextLength > 0}
                <span
                    class="text-muted-foreground w-12 text-right text-[11px] tabular-nums"
                >
                    {player.contextPosition + 1}/{player.contextLength}
                </span>
            {/if}
        </div>
    </div>
</footer>
