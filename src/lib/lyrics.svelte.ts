import { invoke } from "@tauri-apps/api/core";
import { player } from "$lib/player.svelte";

export type LyricLine = {
  /** Milliseconds into the recording. `null` on unsynced lyrics, and there is
   *  no way to invent it — which is the point. */
  atMs: number | null;
  text: string;
};

export type TrackLyrics = {
  kind: "synced" | "plain" | "instrumental";
  lines: LyricLine[];
  /** `sidecar`, `embedded`, or a provider name. */
  origin: string;
  offsetMs: number;
  /**
   * The same lines romanised, when the provider had them.
   *
   * A parallel track sharing one set of timestamps rather than different
   * lyrics, so switching to it never moves the highlight.
   */
  romaji: LyricLine[] | null;
};

/**
 * What the panel is doing, which is not the same as what it found.
 *
 * `searching` exists so a spinner appears only while a request is genuinely in
 * flight. The local read that precedes it returns in milliseconds, so showing
 * one for that would be a flicker on every track change.
 */
export type Status =
  | "idle"
  | "reading"
  | "searching"
  | "ready"
  | "empty"
  /** Several rows fit and nothing separated them, so it is your call. */
  | "choosing";

/** One row from a provider, described well enough to choose between. */
export type Candidate = {
  id: number;
  /** Which provider issued the id, since ids mean nothing across them. */
  provider: string;
  title: string;
  artist: string;
  durationSecs: number | null;
  /** Seconds this is longer (+) or shorter (−) than the track. */
  deltaSecs: number | null;
  synced: boolean;
  /** Whether a timed romanisation comes with it. */
  romaji: boolean;
  instrumental: boolean;
};

type Lookup = {
  lyrics: TrackLyrics | null;
  candidates: Candidate[];
};

/** How far one press of the offset control moves the lyrics, in milliseconds. */
const NUDGE_MS = 100;

/**
 * How long a manual scroll holds the view still.
 *
 * Someone who scrolled ahead to read the next verse is mid-thought, and having
 * the view yanked back a second later is the single most irritating thing a
 * lyrics panel can do.
 */
const MANUAL_SCROLL_HOLD_MS = 4_000;

/**
 * The line between event jitter and a real seek, in milliseconds.
 *
 * Comfortably larger than the 200 ms gap between position events, so ordinary
 * delivery jitter is well under it, and far smaller than any seek a person
 * makes on purpose.
 */
const SEEK_MS = 400;

/**
 * Lyrics for whatever is playing, and the clock that highlights them.
 *
 * # Why there is a clock here at all
 *
 * The backend reports position every 200 ms (`POLL_INTERVAL` × `PROGRESS_EVERY`
 * in `engine.rs`). Highlighting straight off those events means a line change
 * lands wherever the next event happens to fall — up to 200 ms late, every
 * time, and visibly behind the singing.
 *
 * So the events are treated as *anchors* rather than as the clock. Each one
 * records `(position, performance.now())`, and between them the position is
 * carried forward by wall time. Re-anchoring on every event means the error
 * never accumulates: this is a smoothing layer over an authoritative signal,
 * not a second source of truth.
 *
 * # Why that is cheap
 *
 * The frame loop computes a position and then writes nothing unless the
 * *active line* changed. Sixty times a second it does some arithmetic and one
 * comparison; the DOM hears about it a few times a minute. Moving between
 * lines is left to a CSS transition, which is what keeps the scroll off the
 * main thread entirely.
 *
 * Veluna renders every line from a React state update on every progress event
 * instead, and papers over the gaps with `transition: width 0.5s` on the
 * progress bar. That trick works for a bar. It does not work for text.
 */
class LyricsStore {
  /** Whether the view is showing. */
  open = $state(false);

  lyrics = $state<TrackLyrics | null>(null);
  status = $state<Status>("idle");

  /**
   * What to offer when we could not decide.
   *
   * Arrives with the lookup rather than being asked for separately, so the
   * panel can put the question straight in front of you instead of saying
   * "nothing found" and hiding the results it already had.
   */
  candidates = $state<Candidate[]>([]);

  /** True while the picker is open over lyrics that are already showing. */
  browsing = $state(false);

  /**
   * Show the romanised track instead of the original.
   *
   * Per session rather than per track: someone who cannot read Japanese
   * cannot read the next Japanese song either, so making them ask again on
   * every track would be a worse default than remembering.
   */
  romaji = $state(false);

  /** Which line is being sung. `-1` before the first one. */
  activeIndex = $state(-1);

  /** Set while a manual scroll is holding the view still. */
  following = $state(true);

  /** The track `lyrics` describes, so a late reply for a skipped track is
   *  discarded rather than shown over the next song. */
  #loadedFor: number | null = null;

  /** Guards against two loads racing for the same track. */
  #inFlight: number | null = null;

  #frame: number | null = null;
  #clock: ClockState = freshClock();
  #manualScrollAt = 0;

  get offsetMs() {
    return this.lyrics?.offsetMs ?? 0;
  }

  get synced() {
    return this.lyrics?.kind === "synced";
  }

  /** Whether a romanised track exists to switch to. */
  get hasRomaji() {
    return (this.lyrics?.romaji?.length ?? 0) > 0;
  }

  /**
   * The lines to draw.
   *
   * Both tracks share one set of timestamps, so `activeIndex` means the same
   * thing in either and switching never moves the highlight.
   */
  get visibleLines(): LyricLine[] {
    const lyrics = this.lyrics;
    if (!lyrics) return [];
    if (this.romaji && lyrics.romaji?.length) return lyrics.romaji;
    return lyrics.lines;
  }

  toggle() {
    this.open = !this.open;
    if (this.open) {
      void this.load(player.trackId);
      this.startClock();
    } else {
      this.stopClock();
    }
  }

  close() {
    if (!this.open) return;
    this.open = false;
    this.stopClock();
  }

  /**
   * Local first, then the network.
   *
   * Two commands rather than one so the panel paints what it already has
   * immediately — a sidecar, the file's own tags, a cached answer — and only
   * shows "searching" when something is actually being searched for.
   */
  async load(trackId: number | null) {
    if (trackId === null) {
      this.#loadedFor = null;
      this.lyrics = null;
      this.candidates = [];
      this.browsing = false;
      this.status = "idle";
      return;
    }
    if (this.#inFlight === trackId) return;

    this.#inFlight = trackId;
    this.#loadedFor = trackId;
    this.lyrics = null;
    this.candidates = [];
    this.browsing = false;
    this.activeIndex = -1;
    this.following = true;
    this.status = "reading";

    try {
      const local = await invoke<TrackLyrics | null>("track_lyrics", {
        trackId,
      });
      // The track moved on while we were reading. Anything we found belongs
      // to a song that is no longer playing.
      if (this.#loadedFor !== trackId) return;

      if (local) {
        this.lyrics = local;
        this.status = "ready";
        return;
      }

      this.status = "searching";
      const lookup = await invoke<Lookup>("fetch_track_lyrics", { trackId });
      if (this.#loadedFor !== trackId) return;

      this.lyrics = lookup.lyrics;
      this.candidates = lookup.candidates;
      this.status = lookup.lyrics
        ? "ready"
        : lookup.candidates.length > 0
          ? "choosing"
          : "empty";
    } catch {
      // A provider that cannot be reached is not worth a toast on every track
      // change. The panel says what it knows, which is nothing.
      if (this.#loadedFor === trackId) {
        this.lyrics = null;
        this.status = "empty";
      }
    } finally {
      if (this.#inFlight === trackId) this.#inFlight = null;
    }
  }

  /**
   * Ask again, on purpose, and show everything.
   *
   * Unranked and ungated: this is the way out of the ranking being wrong, so
   * applying the same rules to it would be circular. `deltaSecs` puts the
   * judgement in front of the person making it instead.
   */
  async search(query?: string) {
    const trackId = this.#loadedFor;
    if (trackId === null) return;

    this.browsing = true;
    const previous = this.status;
    this.status = "searching";

    try {
      const found = await invoke<Candidate[]>("search_lyrics", {
        trackId,
        query: query?.trim() || null,
      });
      if (this.#loadedFor !== trackId) return;

      this.candidates = found;
      // An empty search does not throw away lyrics that are already showing.
      this.status = found.length > 0 ? "choosing" : this.lyrics ? previous : "empty";
    } catch {
      if (this.#loadedFor === trackId) {
        this.candidates = [];
        this.status = this.lyrics ? previous : "empty";
      }
    }
  }

  /** Takes your word for which row is the right one. */
  async pick(lyricsId: number, provider: string) {
    const trackId = this.#loadedFor;
    if (trackId === null) return;

    this.status = "searching";
    try {
      const found = await invoke<TrackLyrics | null>("pick_lyrics", {
        trackId,
        lyricsId,
        provider,
      });
      if (this.#loadedFor !== trackId) return;

      this.lyrics = found;
      this.candidates = [];
      this.browsing = false;
      this.activeIndex = -1;
      this.following = true;
      this.status = found ? "ready" : "empty";
    } catch {
      if (this.#loadedFor === trackId) this.status = "choosing";
    }
  }

  /** Leaves the picker without changing anything. */
  stopBrowsing() {
    this.browsing = false;
    this.candidates = [];
    this.status = this.lyrics ? "ready" : "empty";
  }

  /** Called when the playing track changes, from the component that mounts. */
  trackChanged(trackId: number | null) {
    if (trackId === this.#loadedFor) return;
    if (!this.open) {
      // Nothing is on screen, so nothing needs fetching. Clearing keeps a
      // stale answer from flashing up when the panel is next opened.
      this.#loadedFor = null;
      this.lyrics = null;
      this.candidates = [];
      this.browsing = false;
      this.status = "idle";
      return;
    }
    void this.load(trackId);
  }

  // --- the clock ---------------------------------------------------------

  startClock() {
    if (this.#frame !== null) return;
    this.#clock = freshClock();
    const step = () => {
      this.tick(performance.now());
      this.#frame = requestAnimationFrame(step);
    };
    this.#frame = requestAnimationFrame(step);
  }

  stopClock() {
    if (this.#frame === null) return;
    cancelAnimationFrame(this.#frame);
    this.#frame = null;
  }

  /**
   * One frame: carry the position forward, and publish only a line change.
   *
   * Exported for the tests, which drive it with a fake clock rather than
   * waiting on real frames.
   */
  tick(nowMs: number) {
    const lines = this.lyrics?.lines;
    if (!lines || this.lyrics?.kind !== "synced") return;

    // While the handle is being dragged the user owns the position, and the
    // backend's ticks are being ignored anyway.
    const authoritative = player.scrubbing
      ? player.scrubSecs
      : player.positionSecs;

    // Wall time only carries the position while audio is actually moving.
    const moving = player.state === "playing" && !player.scrubbing;
    const positionMs = advance(this.#clock, authoritative, moving, nowMs);

    const next = indexAt(lines, positionMs, this.offsetMs);
    if (next !== this.activeIndex) this.activeIndex = next;

    if (!this.following && Date.now() - this.#manualScrollAt > MANUAL_SCROLL_HOLD_MS) {
      this.following = true;
    }
  }

  /** The user took the wheel. Stop moving the view out from under them. */
  scrolledManually() {
    this.#manualScrollAt = Date.now();
    this.following = false;
  }

  resumeFollowing() {
    this.#manualScrollAt = 0;
    this.following = true;
  }

  // --- offset ------------------------------------------------------------

  /**
   * Where a line's audio actually is, once this track's shift is applied.
   *
   * Positive offset means the lyrics run late against this recording and need
   * pulling earlier — the same sign convention as the LRC `[offset:]` header,
   * so a value typed here and a value baked into a file mean the same thing.
   */
  audioTimeOf(line: LyricLine): number | null {
    if (line.atMs === null) return null;
    return Math.max(0, line.atMs - this.offsetMs) / 1000;
  }

  async nudge(direction: -1 | 1) {
    await this.setOffset(this.offsetMs + direction * NUDGE_MS);
  }

  async setOffset(offsetMs: number) {
    const trackId = this.#loadedFor;
    if (trackId === null || !this.lyrics) return;

    // Optimistic, then corrected: the backend clamps, and showing the clamp
    // is more honest than silently keeping a value it refused.
    this.lyrics = { ...this.lyrics, offsetMs };
    const stored = await invoke<number>("set_lyrics_offset", {
      trackId,
      offsetMs,
    });
    if (this.#loadedFor === trackId && this.lyrics) {
      this.lyrics = { ...this.lyrics, offsetMs: stored };
    }
  }
}

/**
 * What the interpolating clock remembers between frames.
 *
 * Split out as plain data with a plain function over it so the timing rules
 * can be exercised with a fake clock, which is the only way to see a
 * jitter-induced backward step at all — it depends on when events land
 * relative to frames, and reproducing that by playing a song is not a test.
 */
export type ClockState = {
  anchorSecs: number;
  anchorWall: number;
  lastAuthoritative: number;
  reported: number;
};

export function freshClock(): ClockState {
  return {
    anchorSecs: 0,
    anchorWall: 0,
    lastAuthoritative: Number.NaN,
    reported: Number.NEGATIVE_INFINITY,
  };
}

/**
 * One frame of the interpolating clock, in milliseconds.
 *
 * Re-anchors whenever the authoritative position changes, and carries it
 * forward by wall time in between. Because every event re-anchors, error
 * never accumulates: this smooths an authoritative signal rather than
 * becoming a second source of truth.
 */
export function advance(
  clock: ClockState,
  authoritativeSecs: number,
  moving: boolean,
  nowMs: number,
): number {
  if (authoritativeSecs !== clock.lastAuthoritative) {
    clock.lastAuthoritative = authoritativeSecs;
    clock.anchorSecs = authoritativeSecs;
    clock.anchorWall = nowMs;

    // A move this large is a seek or a new track, so the monotonic clamp
    // below has to let go of wherever it had got to.
    if (Math.abs(authoritativeSecs * 1000 - clock.reported) > SEEK_MS) {
      clock.reported = Number.NEGATIVE_INFINITY;
    }
  }

  let positionMs =
    clock.anchorSecs * 1000 + (moving ? nowMs - clock.anchorWall : 0);

  // Never step backwards by a little.
  //
  // An event is emitted at one moment and arrives at another, and that delay
  // varies. So a fresh anchor can land slightly *behind* where wall time had
  // already carried us, and if a line boundary happens to sit in that gap the
  // highlight flicks back a line and then forward again. Absorbing small
  // reversals costs nothing and removes the whole class; anything larger is a
  // real seek and passes straight through.
  if (positionMs < clock.reported && clock.reported - positionMs < SEEK_MS) {
    positionMs = clock.reported;
  }
  clock.reported = positionMs;

  return positionMs;
}

/**
 * The last line whose time has passed, by binary search.
 *
 * Returns `-1` before the first line — a real state, not an error: LRC files
 * routinely start at four or five seconds, and holding the first line
 * highlighted through an intro says the wrong thing.
 */
export function indexAt(
  lines: LyricLine[],
  positionMs: number,
  offsetMs: number,
): number {
  let low = 0;
  let high = lines.length - 1;
  let found = -1;

  while (low <= high) {
    const mid = (low + high) >> 1;
    const at = lines[mid].atMs;
    if (at === null || at - offsetMs > positionMs) {
      high = mid - 1;
    } else {
      found = mid;
      low = mid + 1;
    }
  }

  return found;
}

export const lyricsStore = new LyricsStore();
