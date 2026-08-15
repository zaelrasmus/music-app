<script lang="ts">
    import { Button } from "$components/ui/button";
    import { Slider } from "$components/ui/slider";
    import { player } from "$lib/player.svelte";
    import { queueStore } from "$lib/queue.svelte";
    import { trackStore } from "$lib/tracks.svelte";
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

    /**
     * The backend hydrates the queue payload itself, so it knows about a
     * YouTube result saved seconds ago that the library list has not reloaded
     * yet. `trackStore` is the fallback for the moment before the first queue
     * event arrives.
     */
    const nowPlaying = $derived(
        queueStore.current ??
            trackStore.tracks.find((t) => t.id === player.trackId) ??
            null,
    );

    // Total comes from the scanned tag, not from rodio, whose total_duration
    // is None for several formats.
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

    // Kept within [0, max] so the slider never has to snap a value back at us.
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
</script>

<div class="bg-card fixed inset-x-0 bottom-0 border-t">
    <div class="mx-auto flex max-w-4xl flex-col gap-1 px-4 py-2">
        <!-- Progress -->
        <div class="flex items-center gap-3">
            <span
                class="text-muted-foreground w-10 shrink-0 text-right text-xs tabular-nums"
            >
                {formatTime(player.displaySecs)}
            </span>

            <Slider
                type="single"
                value={sliderValue}
                min={0}
                max={sliderMax}
                step={1}
                disabled={!seekable}
                aria-label="Seek"
                onValueChange={(v) => player.scrubTo(v)}
                onValueCommit={(v) => player.commitScrub(v)}
            />

            <span
                class="text-muted-foreground w-10 shrink-0 text-xs tabular-nums"
            >
                {formatTime(totalSecs)}
            </span>
        </div>

        <!-- Transport -->
        <div class="flex items-center gap-2">
            <div class="flex min-w-0 flex-1 items-baseline gap-2 text-sm">
                {#if nowPlaying}
                    <span class="truncate font-medium">{nowPlaying.title}</span>
                    <span class="text-muted-foreground truncate text-xs">
                        {#if stalled}
                            Reconnecting…
                        {:else if loading}
                            Loading…
                        {:else}
                            {nowPlaying.artist ?? "Unknown artist"}
                        {/if}
                    </span>
                {:else}
                    <span class="text-muted-foreground">Nothing playing</span>
                {/if}
            </div>

            <div class="flex shrink-0 items-center gap-1">
                <Button
                    variant="ghost"
                    size="icon"
                    aria-label="Shuffle"
                    aria-pressed={player.shuffle}
                    class={player.shuffle ? "text-primary" : ""}
                    onclick={() => player.toggleShuffle()}
                >
                    <ShuffleIcon />
                </Button>
                <Button
                    variant="ghost"
                    size="icon"
                    aria-label="Previous track"
                    onclick={() => player.previous()}
                >
                    <SkipBackIcon />
                </Button>
                <Button
                    variant="ghost"
                    size="icon"
                    aria-label={loading ? "Loading" : player.state === "playing" ? "Pause" : "Play"}
                    disabled={loading}
                    onclick={() => player.togglePlayPause()}
                >
                    {#if loading}
                        <LoaderIcon class="animate-spin" />
                    {:else if player.state === "playing"}
                        <PauseIcon />
                    {:else}
                        <PlayIcon />
                    {/if}
                </Button>
                <Button
                    variant="ghost"
                    size="icon"
                    aria-label="Next track"
                    onclick={() => player.next()}
                >
                    <SkipForwardIcon />
                </Button>
                <Button
                    variant="ghost"
                    size="icon"
                    aria-label={repeatLabel}
                    title={repeatLabel}
                    aria-pressed={player.repeat !== "off"}
                    class={player.repeat !== "off" ? "text-primary" : ""}
                    onclick={() => player.cycleRepeat()}
                >
                    {#if player.repeat === "one"}
                        <Repeat1Icon />
                    {:else}
                        <RepeatIcon />
                    {/if}
                </Button>
            </div>

            <div class="flex w-36 shrink-0 items-center gap-1">
                <Button
                    variant="ghost"
                    size="icon"
                    aria-label={player.muted ? "Unmute" : "Mute"}
                    onclick={() => player.toggleMute()}
                >
                    {#if player.muted || player.volume === 0}
                        <VolumeXIcon />
                    {:else if player.volume < 0.5}
                        <Volume1Icon />
                    {:else}
                        <Volume2Icon />
                    {/if}
                </Button>
                <Slider
                    type="single"
                    value={player.muted ? 0 : player.volume}
                    min={0}
                    max={1}
                    step={0.01}
                    aria-label="Volume"
                    onValueChange={(v) => player.setVolume(v)}
                />
            </div>

            <div class="flex shrink-0 items-center gap-1">
                <Button
                    variant="ghost"
                    size="icon"
                    aria-label="Up next"
                    title="Up next"
                    aria-pressed={queueStore.open}
                    class={queueStore.open ? "text-primary" : ""}
                    onclick={() => queueStore.toggle()}
                >
                    <ListMusicIcon />
                </Button>

                {#if player.manualLength > 0}
                    <!-- The queue is the thing worth a badge; the context
                         position is not, and would read as a total. -->
                    <span
                        class="bg-primary text-primary-foreground rounded-full px-1.5 text-[10px] tabular-nums"
                    >
                        {player.manualLength}
                    </span>
                {:else if player.contextLength > 0}
                    <span
                        class="text-muted-foreground w-12 text-right text-xs tabular-nums"
                    >
                        {player.contextPosition + 1}/{player.contextLength}
                    </span>
                {/if}
            </div>
        </div>
    </div>
</div>
