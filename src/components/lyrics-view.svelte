<script lang="ts">
    import { lyricsStore } from "$lib/lyrics.svelte";
    import { player } from "$lib/player.svelte";
    import { queueStore } from "$lib/queue.svelte";
    import { covers } from "$lib/covers.svelte";
    import { coverGradient, coverSeed } from "$lib/cover";
    import LoaderIcon from "@lucide/svelte/icons/loader-circle";
    import Mic2Icon from "@lucide/svelte/icons/mic-2";
    import AudioLinesIcon from "@lucide/svelte/icons/audio-lines";
    import ArrowDownIcon from "@lucide/svelte/icons/arrow-down";
    import MinusIcon from "@lucide/svelte/icons/minus";
    import PlusIcon from "@lucide/svelte/icons/plus";
    import XIcon from "@lucide/svelte/icons/x";
    import TimerOffIcon from "@lucide/svelte/icons/timer-off";

    /**
     * The backdrop's source, and the only thing this view reads about the
     * track.
     *
     * Deliberately not the player bar's `nowPlaying`, which is sixty lines of
     * careful fallback built to survive two events arriving out of order. That
     * logic exists because getting it wrong puts the *wrong song's title* over
     * the audio, and it has been fixed three times. Here the stake is a
     * background colour, so borrowing the risk would be a bad trade — and the
     * title is already on screen in the bar below, which is why this view does
     * not repeat it.
     */
    const track = $derived(
        queueStore.current?.trackId === player.trackId
            ? queueStore.current
            : null,
    );

    const artwork = $derived(
        covers.url(track?.coverKey) ?? track?.remoteThumbnailUrl ?? null,
    );
    const gradient = $derived(track ? coverGradient(coverSeed(track)) : null);

    const lines = $derived(lyricsStore.visibleLines);
    const kind = $derived(lyricsStore.lyrics?.kind ?? null);

    let viewport = $state<HTMLDivElement | null>(null);
    let viewportHeight = $state(0);
    let nodes = $state<(HTMLElement | null)[]>([]);

    /**
     * Where the sung line sits, as a fraction of the viewport.
     *
     * Above centre, because reading runs downward: the lines that matter next
     * want the larger share of the space.
     */
    const FOCUS = 0.38;

    /**
     * Room at each end, so the first and last lines can reach the focus point.
     *
     * Without it a song opens with its first line pinned to the top edge and
     * ends with the scroll drifting away from the last one. Measured rather
     * than a `vh` unit: this panel is not the window's height.
     *
     * Zero for unsynced lyrics, which do not move: half a screen of empty
     * space above a static block of text is just a page that starts oddly.
     */
    const pad = $derived(
        lyricsStore.synced ? Math.max(0, viewportHeight * FOCUS) : 0,
    );

    const reduceMotion =
        typeof window !== "undefined" &&
        window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;

    /**
     * Follows the active line.
     *
     * Runs on a line change rather than on a clock: the store publishes
     * `activeIndex` only when it actually moves, so this fires a few times a
     * minute and a smooth scroll never has to interrupt itself.
     */
    $effect(() => {
        const index = lyricsStore.activeIndex;
        const following = lyricsStore.following;
        if (!following || !viewport) return;

        const node = index >= 0 ? nodes[index] : nodes[0];
        if (!node) return;

        viewport.scrollTo({
            top: node.offsetTop - viewport.clientHeight * FOCUS + node.offsetHeight / 2,
            behavior: reduceMotion ? "auto" : "smooth",
        });
    });

    /**
     * A wheel or a drag means the user is reading ahead, and the view holds
     * still for a few seconds.
     *
     * Listening for those rather than for `scroll`, which our own `scrollTo`
     * also fires — that would make the view stop following itself.
     */
    function manual() {
        if (lyricsStore.synced) lyricsStore.scrolledManually();
    }

    function seekTo(index: number) {
        const at = lyricsStore.audioTimeOf(lines[index]);
        if (at === null) return;
        lyricsStore.resumeFollowing();
        void player.commitScrub(at);
    }

    function formatOffset(ms: number) {
        const sign = ms > 0 ? "+" : ms < 0 ? "−" : "";
        return `${sign}${(Math.abs(ms) / 1000).toFixed(1)}s`;
    }

    const originLabel = $derived.by(() => {
        const origin = lyricsStore.lyrics?.origin;
        if (!origin) return null;
        if (origin === "sidecar") return "from a .lrc file";
        if (origin === "embedded") return "from this file's tags";
        return `from ${origin}`;
    });

    const ghost =
        "grid size-8 shrink-0 place-items-center rounded-md text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:bg-accent";

    /** What the user typed into the picker's search box. */
    let query = $state("");

    /**
     * Seeded from the track so the field opens with the question already
     * asked, rather than empty and waiting.
     */
    function openPicker() {
        query = track ? `${track.artist ?? ""} ${track.title}`.trim() : "";
        void lyricsStore.search(query || undefined);
    }

    function formatDuration(secs: number | null) {
        if (secs === null) return "—";
        const total = Math.round(secs);
        return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
    }

    /**
     * How far a candidate is from this track, in seconds.
     *
     * The single most useful column, and the one a person can act on without
     * knowing anything about the ranking: a song of the right length is
     * probably the right song, and one that is a minute out is not.
     */
    function formatDelta(delta: number | null) {
        if (delta === null) return null;
        const rounded = Math.round(delta);
        if (rounded === 0) return "exact";
        return `${rounded > 0 ? "+" : "−"}${Math.abs(rounded)}s`;
    }

    function deltaTone(delta: number | null) {
        if (delta === null) return "text-muted-foreground/50";
        const off = Math.abs(delta);
        if (off <= 2) return "text-foreground";
        if (off <= 10) return "text-muted-foreground";
        return "text-muted-foreground/50";
    }
</script>

<!--
  Covers the view, not the window.

  Spotify's lyrics screen keeps the transport visible, and so does this: the
  player bar and the sidebar stay exactly where they were, so nothing here has
  to re-implement play, seek or volume — which is most of what a full-screen
  lyrics panel usually costs.
-->
<div class="bg-background absolute inset-0 z-20 flex flex-col overflow-hidden">
    <!-- Backdrop: the artwork, blurred past recognition, or the generated
         gradient when there is none. Inert, and behind everything. -->
    {#if artwork}
        <div
            class="pointer-events-none absolute -inset-24 bg-cover bg-center opacity-40 blur-[90px] saturate-150"
            style="background-image: url({artwork})"
        ></div>
    {:else if gradient}
        <div
            class="pointer-events-none absolute -inset-24 opacity-25 blur-[90px]"
            style="background-image: {gradient}"
        ></div>
    {/if}

    <header
        class="relative z-10 flex shrink-0 items-center gap-2 px-5 pt-4 pb-2"
    >
        <span class="text-muted-foreground flex items-center gap-2 text-xs">
            <Mic2Icon class="size-3.5" />
            Lyrics
            {#if originLabel}
                <span class="opacity-70">· {originLabel}</span>
            {/if}
        </span>

        <!--
          Says out loud whether these follow the music.

          Without it "synced" and "not synced" look identical until you notice
          nothing is moving, and then the honest conclusion available to you is
          that the feature is broken. Naming the state is cheaper than that,
          and it is the truth either way.
        -->
        {#if lyricsStore.status === "ready" && kind !== "instrumental"}
            <span
                class="rounded-full px-2 py-0.5 text-[10px] font-medium {lyricsStore.synced
                    ? 'bg-foreground/10 text-foreground'
                    : 'bg-muted text-muted-foreground'}"
            >
                {lyricsStore.synced ? "Synced" : "Not synced"}
            </span>
        {/if}

        <div class="flex-1"></div>

        <!-- Only offered where there is something to switch to. -->
        {#if lyricsStore.hasRomaji && lyricsStore.status === "ready"}
            <button
                type="button"
                class="rounded-md px-2 py-1 text-[11px] transition-colors hover:bg-accent {lyricsStore.romaji
                    ? 'text-foreground'
                    : 'text-muted-foreground hover:text-foreground'}"
                aria-pressed={lyricsStore.romaji}
                title="Show the romanised lyrics"
                onclick={() => (lyricsStore.romaji = !lyricsStore.romaji)}
            >
                Romaji
            </button>
        {/if}

        <!--
          The offset control.

          Not decoration. A YouTube upload of a song carries an intro card the
          release does not, so lyrics timed against the release run late
          against this audio — by a fixed amount, for the whole track. Nothing
          else in the panel can fix that, and without this the only available
          conclusion is that the feature is broken.
        -->
        {#if lyricsStore.synced}
            <div
                class="text-muted-foreground flex items-center gap-0.5 text-[11px]"
            >
                <button
                    type="button"
                    class={ghost}
                    aria-label="Shift lyrics earlier"
                    title="Shift lyrics earlier  ( [ )"
                    onclick={() => lyricsStore.nudge(1)}
                >
                    <MinusIcon class="size-3.5" />
                </button>
                <span class="w-11 text-center tabular-nums">
                    {formatOffset(lyricsStore.offsetMs)}
                </span>
                <button
                    type="button"
                    class={ghost}
                    aria-label="Shift lyrics later"
                    title="Shift lyrics later  ( ] )"
                    onclick={() => lyricsStore.nudge(-1)}
                >
                    <PlusIcon class="size-3.5" />
                </button>
            </div>
        {/if}

        <!--
          Always reachable, not just when the search failed.

          A confident match can still be the wrong song, and the person who
          can tell is the one listening to it.
        -->
        {#if lyricsStore.status === "ready" && !lyricsStore.browsing}
            <button
                type="button"
                class="text-muted-foreground hover:text-foreground rounded-md px-2 py-1 text-[11px] transition-colors hover:bg-accent"
                onclick={openPicker}
            >
                Wrong lyrics?
            </button>
        {/if}

        <button
            type="button"
            class={ghost}
            aria-label="Close lyrics"
            title="Close  (Esc)"
            onclick={() => lyricsStore.close()}
        >
            <XIcon class="size-4" />
        </button>
    </header>

    {#if lyricsStore.status === "searching"}
        <div
            class="text-muted-foreground relative z-10 flex flex-1 flex-col items-center justify-center gap-3"
        >
            <LoaderIcon class="size-5 animate-spin opacity-60" />
            <p class="text-[13px]">Searching lrclib…</p>
        </div>
    {:else if lyricsStore.status === "choosing"}
        <!--
          The honest answer when ranking cannot settle it.

          Several rows fit this track's length and title and nothing separates
          them — usually because the artist is unknown or is a channel name.
          Guessing here means a real chance of scrolling a different song's
          words in perfect time, which reads as the app being confidently
          wrong. Showing the list costs one click and is simply true.
        -->
        <div class="relative z-10 flex min-h-0 flex-1 flex-col gap-3 px-6 pb-4 sm:px-12">
            <div class="shrink-0">
                <p class="text-[15px] font-medium">Which one is this?</p>
                <p class="text-muted-foreground text-xs">
                    {lyricsStore.candidates.length} possible matches — nothing separated
                    them, so it is your call.
                </p>
            </div>

            <form
                class="flex shrink-0 gap-2"
                onsubmit={(event) => {
                    event.preventDefault();
                    void lyricsStore.search(query);
                }}
            >
                <input
                    bind:value={query}
                    placeholder="Search by artist and title"
                    class="border-border bg-background/60 focus-visible:ring-ring min-w-0 flex-1 rounded-md border px-3 py-1.5 text-[13px] focus-visible:ring-1 focus-visible:outline-none"
                />
                <button
                    type="submit"
                    class="bg-foreground text-background rounded-md px-3 py-1.5 text-[13px] font-medium transition-transform hover:scale-[1.03]"
                >
                    Search
                </button>
            </form>

            <div class="-mx-2 min-h-0 flex-1 overflow-y-auto px-2">
                {#each lyricsStore.candidates as candidate (candidate.id)}
                    {@const delta = formatDelta(candidate.deltaSecs)}
                    <button
                        type="button"
                        class="hover:bg-accent flex w-full items-center gap-3 rounded-md px-3 py-2 text-left transition-colors"
                        onclick={() => lyricsStore.pick(candidate.id, candidate.provider)}
                    >
                        <div class="min-w-0 flex-1">
                            <p class="truncate text-[13px] font-medium">
                                {candidate.title}
                            </p>
                            <p class="text-muted-foreground truncate text-xs">
                                {candidate.artist}
                            </p>
                        </div>

                        <!--
                          Whether it is timed is the difference between a
                          karaoke screen and a page of text, so it is weighted
                          rather than listed: a synced row is worth picking
                          over an unsynced one even when the length fits less
                          well, and the list should show that at a glance.
                        -->
                        {#if candidate.romaji}
                            <span
                                class="text-muted-foreground shrink-0 rounded-full border border-current/30 px-1.5 text-[10px]"
                                title="Comes with a romanised version"
                            >
                                romaji
                            </span>
                        {/if}

                        {#if candidate.instrumental}
                            <span class="text-muted-foreground shrink-0 text-[10px]">
                                instrumental
                            </span>
                        {:else if candidate.synced}
                            <span class="text-foreground shrink-0 text-[10px] font-medium">
                                synced
                            </span>
                        {:else}
                            <span class="text-muted-foreground/60 shrink-0 text-[10px]">
                                text only
                            </span>
                        {/if}

                        <span
                            class="w-12 shrink-0 text-right text-[11px] tabular-nums {deltaTone(
                                candidate.deltaSecs,
                            )}"
                            title="Length of this track: {formatDuration(
                                candidate.durationSecs,
                            )}"
                        >
                            {delta ?? formatDuration(candidate.durationSecs)}
                        </span>
                    </button>
                {/each}
            </div>

            {#if lyricsStore.browsing}
                <button
                    type="button"
                    class="text-muted-foreground hover:text-foreground shrink-0 self-start text-xs transition-colors"
                    onclick={() => lyricsStore.stopBrowsing()}
                >
                    Cancel
                </button>
            {/if}
        </div>
    {:else if kind === "instrumental"}
        <!--
          A positive answer, and worded like one.

          Most players fold this into "no lyrics found", which on a library of
          game soundtracks makes a working feature look broken on half of it.
          The provider said this track has no words; saying so is the whole
          value of asking.
        -->
        <div
            class="relative z-10 flex flex-1 flex-col items-center justify-center gap-3"
        >
            <AudioLinesIcon class="text-muted-foreground size-7 opacity-50" />
            <p class="text-[15px] font-medium">This track has no vocals</p>
            <p class="text-muted-foreground text-xs">
                {originLabel ?? ""} lists it as instrumental.
            </p>
        </div>
    {:else if lines.length === 0}
        <div
            class="relative z-10 flex flex-1 flex-col items-center justify-center gap-3"
        >
            <Mic2Icon class="text-muted-foreground size-7 opacity-40" />
            <p class="text-muted-foreground text-[15px]">No lyrics found</p>
            <p class="text-muted-foreground/70 max-w-xs text-center text-xs">
                Nothing matched this track closely enough to be sure it is the
                same recording.
            </p>
            <button
                type="button"
                class="border-border hover:bg-accent mt-1 rounded-md border px-3 py-1.5 text-xs transition-colors"
                onclick={openPicker}
            >
                Search for lyrics
            </button>
        </div>
    {:else}
        <!--
          No `tabindex`: every synced line below is a real button, so tabbing
          walks the lyrics and scrolls them as it goes. Making the container
          focusable as well would add a tab stop that lands nowhere useful.
        -->
        <!--
          The plain-lyrics notice.

          These lyrics exist and are almost certainly correct; what they do not
          have is timings, and nobody can tell that by looking at them. Left
          unsaid, a page of words that never moves reads as the sync being
          broken rather than absent — so it is said, along with the one thing
          that might fix it.

          Not a warning and not styled as one. Plain lyrics are a good outcome;
          the only bad outcome would be pretending they are something else.
        -->
        {#if kind === "plain"}
            <div
                class="border-border/60 bg-muted/40 relative z-10 mx-6 mb-2 flex shrink-0 flex-wrap items-center gap-x-2 gap-y-1 rounded-md border px-3 py-2 sm:mx-12"
            >
                <TimerOffIcon class="text-muted-foreground size-3.5 shrink-0" />
                <span class="text-[12px]">These lyrics have no timings.</span>
                <span class="text-muted-foreground text-[12px]">
                    They will not scroll with the music.
                </span>
                <button
                    type="button"
                    class="text-foreground ml-auto text-[12px] underline underline-offset-2 hover:no-underline"
                    onclick={openPicker}
                >
                    Look for a synced version
                </button>
            </div>
        {/if}

        <div
            class="relative z-10 min-h-0 flex-1 overflow-y-auto px-6 sm:px-12"
            bind:this={viewport}
            bind:clientHeight={viewportHeight}
            onwheel={manual}
            onpointerdown={manual}
            role="region"
            aria-label="Lyrics"
            style="scrollbar-width: none"
        >
            <div style="padding-top: {pad}px; padding-bottom: {pad}px">
                {#each lines as line, index (index)}
                    {@const active = index === lyricsStore.activeIndex}
                    {@const past = index < lyricsStore.activeIndex}
                    <!--
                      A button, not a div: clicking a line seeks to it, which
                      is the one thing synced lyrics make possible that nothing
                      else does. Unsynced lines carry no time, so they are
                      disabled — the type makes that unambiguous rather than
                      leaving it to a comment.
                    -->
                    <button
                        type="button"
                        bind:this={nodes[index]}
                        disabled={line.atMs === null}
                        onclick={() => seekTo(index)}
                        class="block w-full cursor-pointer py-2.5 text-left text-[clamp(1.1rem,2.2vw,1.6rem)] leading-snug font-semibold text-balance transition-[color,opacity,transform] duration-300 not-disabled:hover:text-foreground disabled:cursor-default
                        {active
                            ? 'text-foreground'
                            : past
                              ? 'text-muted-foreground/45'
                              : 'text-muted-foreground/70'}"
                    >
                        <!-- An LRC timestamp with no words is an instrumental
                             gap. Drawn rather than skipped, so the highlight
                             has somewhere to sit during one. -->
                        {#if line.text.trim() === ""}
                            <span
                                class="inline-flex items-center gap-1.5 align-middle"
                                aria-label="instrumental"
                            >
                                {#each [0, 1, 2] as dot (dot)}
                                    <span
                                        class="bg-current inline-block size-1.5 rounded-full transition-opacity"
                                        style="opacity: {active ? 0.8 : 0.3}"
                                    ></span>
                                {/each}
                            </span>
                        {:else}
                            {line.text}
                        {/if}
                    </button>
                {/each}
            </div>
        </div>

        {#if !lyricsStore.following}
            <div class="pointer-events-none relative z-10 flex justify-center">
                <button
                    type="button"
                    class="bg-foreground text-background pointer-events-auto absolute bottom-4 flex items-center gap-1.5 rounded-full px-3 py-1.5 text-xs font-medium shadow-lg transition-transform hover:scale-105"
                    onclick={() => lyricsStore.resumeFollowing()}
                >
                    <ArrowDownIcon class="size-3.5" />
                    Back to current line
                </button>
            </div>
        {/if}
    {/if}
</div>
