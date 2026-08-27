<script lang="ts">
    import ScrubBar from "$components/scrub-bar.svelte";
    import CoverArt from "$components/cover-art.svelte";
    import SourceBadge from "$components/source-badge.svelte";
    import { coverSeed } from "$lib/cover";
    import { player } from "$lib/player.svelte";
    import { queueStore } from "$lib/queue.svelte";
    import { trackStore, type Track } from "$lib/tracks.svelte";
    import { invoke } from "@tauri-apps/api/core";
    import { cacheStore } from "$lib/cache.svelte";
    import { lyricsStore } from "$lib/lyrics.svelte";
    import ListMusicIcon from "@lucide/svelte/icons/list-music";
    import Mic2Icon from "@lucide/svelte/icons/mic-2";
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
     * ago that the library list has not reloaded yet. The library list is
     * second, and a direct fetch by id is the floor under both — so this
     * returning `null` now means the track is genuinely not in the database,
     * not merely that an event has yet to arrive.
     */
    const nowPlaying = $derived.by(() => {
        const id = player.trackId;
        if (id === null) return null;

        const queued = queueStore.current;
        if (queued?.trackId === id) return queued;

        const known = trackStore.tracks.find((t) => t.id === id);
        if (known) return known;

        // Asked for by id when neither list could answer. See `fetched`.
        return fetched?.id === id ? fetched.track : null;
    });

    /**
     * Something is playing that the bar cannot name.
     *
     * Now a genuinely transient state: it lasts as long as the fetch below,
     * rather than as long as whatever event went missing. It is still worth a
     * branch of its own, because the alternatives are a flat lie ("Nothing
     * playing", with audio coming out of the speakers) or the previous track's
     * title sitting over the new one's music.
     */
    const undescribed = $derived(player.trackId !== null && nowPlaying === null);

    /**
     * Ask for the track by id, rather than waiting to be told about it.
     *
     * Both earlier attempts at this bug patched the *push*: make the queue
     * payload arrive, then make the bar re-ask for it. Neither removed the
     * dependency, so the bar could still be left holding an id it had no way
     * to describe, and for a streamed audition — `in_library = 0`, so no
     * library row to fall back on — that reads as "Loading track details…"
     * over audible music.
     *
     * A fetch by id cannot come back describing somebody else, which is what
     * makes one attempt per track correct here. The re-ask it replaces was
     * bounded on the *identity* of the last payload seen, and every payload is
     * freshly deserialised — so a bar that stayed undescribed re-armed on each
     * reply and spun a request round trip per event, forever.
     */
    // Deliberately not `$state`: the effect reads it as a latch, and making it
    // reactive would have the effect depend on its own write.
    let asked: number | null = null;
    let fetched = $state<{ id: number; track: Track } | null>(null);

    $effect(() => {
        const id = player.trackId;
        if (id === null || !undescribed || asked === id) return;

        asked = id;
        void invoke<Track | null>("track_details", { trackId: id })
            .then((track) => {
                if (track) fetched = { id, track };
            })
            // Re-arm, so a transient failure is not permanent. The latch is
            // what keeps this from becoming a retry loop: it only clears for
            // the track that actually failed.
            .catch(() => {
                if (asked === id) asked = null;
            });
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

    /**
     * Named by what it acts on, not just by its mode.
     *
     * "Repeat all" and the queue panel's loop button were both a circular arrow
     * saying "again", and the difference between them — playlist versus the
     * handful of tracks queued by hand — was invisible until you had been
     * caught by it. Saying which list each one means is the cheapest fix.
     */
    const repeatLabel = $derived(
        player.repeat === "one"
            ? "Repeat this track"
            : player.repeat === "all"
              ? "Repeat the playlist"
              : "Repeat off — the playlist stops at the end",
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
        {:else if undescribed}
            <!--
              Playing, but not yet describable. "Nothing playing" would be a
              flat lie with audio coming out of the speakers, and showing the
              last song's title instead is the lie this branch exists to
              replace. So: the shape of a track, and an honest label.
            -->
            <div class="bg-muted grid size-[52px] shrink-0 place-items-center rounded-lg">
                <ListMusicIcon class="text-muted-foreground size-5" />
            </div>
            <div class="flex min-w-0 flex-col gap-0.5">
                <span class="text-muted-foreground truncate text-[13px] leading-tight">
                    Loading track details…
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
                 modifier; this is the verb.

                 Disabled on there being no track, not on being unable to name
                 one: a bar that cannot show the title must still pause. -->
            <button
                type="button"
                class="bg-foreground text-background grid size-9 shrink-0 place-items-center rounded-full transition-transform hover:scale-105 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background active:scale-95 disabled:opacity-50 disabled:hover:scale-100"
                aria-label={loading
                    ? "Loading"
                    : player.state === "playing"
                      ? "Pause"
                      : "Play"}
                disabled={loading || player.trackId === null}
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

        <!--
          102px: ten percent wider than Spotify's, which is 93px.

          That figure is measured rather than eyeballed. Spotify gives the whole
          volume control `flex: 0 1 125px` and puts a `.control-button` inside
          it at a fixed 32px, leaving 93px of slider. Our mute button is 32px
          too, so the two sit in directly comparable space.

          Still lighter than the seek bar — see `compact`. Width is what makes
          it a volume control; weight is what keeps it from reading as a second
          progress bar.
        -->
        <ScrubBar
            class="w-[102px] shrink-0"
            compact
            value={player.muted ? 0 : player.volume}
            max={1}
            step={0.05}
            label="Volume"
            valueText="{Math.round((player.muted ? 0 : player.volume) * 100)}%"
            onScrub={(v) => player.previewVolume(v)}
            onCommit={(v) => player.setVolume(v)}
        />

        <button
            type="button"
            class="{ghost} {lyricsStore.open ? on : off}"
            aria-label="Lyrics"
            title="Lyrics  (Ctrl+L)"
            aria-pressed={lyricsStore.open}
            disabled={player.trackId === null}
            onclick={() => lyricsStore.toggle()}
        >
            <Mic2Icon class="size-4" />
        </button>

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
