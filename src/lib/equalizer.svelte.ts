import { invoke } from "@tauri-apps/api/core";
import { readSetting, writeSetting } from "$lib/settings.svelte";

/** How far a band may be pushed, matching `equalizer.rs`. */
export const MAX_GAIN_DB = 12;

export type Preset = {
  name: string;
  /** One gain per band, low to high. */
  gains: number[];
};

/**
 * Curves worth having without building one by hand.
 *
 * Named for what they do to the music rather than for a genre where possible:
 * "Bass boost" is a promise anyone can check, where "Rock" is a guess about
 * what someone wants. The two genre names that survive are the ones people
 * actually look for in a list like this.
 *
 * Kept gentle. A preset that adds 12 dB looks impressive in a screenshot and
 * spends the whole track being undone by the limiter.
 */
export const PRESETS: Preset[] = [
  { name: "Flat", gains: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0] },
  { name: "Bass boost", gains: [6, 5, 4, 2, 0, 0, 0, 0, 0, 0] },
  { name: "Vocal", gains: [-2, -2, -1, 1, 3, 3, 2, 1, 0, -1] },
  { name: "Treble", gains: [0, 0, 0, 0, 0, 1, 2, 4, 5, 5] },
  { name: "Loudness", gains: [5, 4, 2, 0, -1, -1, 0, 2, 4, 5] },
  { name: "Electronic", gains: [4, 3, 1, 0, -1, 0, 1, 2, 3, 4] },
  { name: "Rock", gains: [3, 2, 0, -1, -1, 1, 2, 3, 3, 2] },
];

/** A short label for a centre frequency: 63, 1k, 16k. */
export function bandLabel(hz: number): string {
  if (hz >= 1000) {
    const k = hz / 1000;
    return `${Number.isInteger(k) ? k : k.toFixed(1)}k`;
  }
  return `${Math.round(hz)}`;
}

/**
 * The ten-band equaliser.
 *
 * Band centres come from the backend rather than being repeated here: they are
 * the frequencies the filters actually sit at, and a label that drifts away
 * from its filter is worse than no label.
 *
 * Every change goes straight to the audio thread's shared atomics, so dragging
 * a slider is heard immediately — there is no decode restart and nothing to
 * debounce for correctness. Only the *persisted* copy is debounced, to avoid
 * writing to disk once per pixel.
 */
class EqualizerStore {
  centres = $state<number[]>([]);
  gains = $state<number[]>(new Array(10).fill(0));
  enabled = $state(false);

  /** Which preset the current curve matches, if any. */
  readonly activePreset = $derived(
    PRESETS.find((preset) =>
      preset.gains.every((gain, i) => gain === this.gains[i]),
    )?.name ?? null,
  );

  /** True when the curve would do nothing even if switched on. */
  readonly isFlat = $derived(this.gains.every((gain) => gain === 0));

  #saveTimer: ReturnType<typeof setTimeout> | null = null;

  async restore() {
    try {
      this.centres = await invoke<number[]>("equalizer_bands");
    } catch {
      // Labels are cosmetic; a failure here must not stop the panel working.
      this.centres = [];
    }

    const saved = await readSetting<number[] | null>("equalizerGains", null);
    if (saved && saved.length === this.gains.length) {
      this.gains = saved.map((gain) =>
        Math.max(-MAX_GAIN_DB, Math.min(MAX_GAIN_DB, gain)),
      );
    }
    this.enabled = await readSetting("equalizerEnabled", false);

    // Pushed even when it is the default, so the backend and the panel cannot
    // disagree about what is in circuit.
    await this.push();
    await this.setEnabled(this.enabled);
  }

  private async push() {
    try {
      await invoke("set_equalizer_bands", { bands: this.gains });
    } catch {
      // The engine keeps whatever it had; the panel already shows the intent.
    }
  }

  /** Debounced, because the audio already changed — only the disk lags. */
  private persist() {
    if (this.#saveTimer) clearTimeout(this.#saveTimer);
    this.#saveTimer = setTimeout(() => {
      void writeSetting("equalizerGains", this.gains);
    }, 400);
  }

  async setBand(index: number, db: number) {
    const clamped = Math.max(-MAX_GAIN_DB, Math.min(MAX_GAIN_DB, db));
    // A new array rather than a mutation, so `$derived` sees the change.
    this.gains = this.gains.map((gain, i) => (i === index ? clamped : gain));
    await this.push();
    this.persist();
  }

  async applyPreset(preset: Preset) {
    this.gains = [...preset.gains];
    await this.push();
    this.persist();
  }

  async reset() {
    await this.applyPreset(PRESETS[0]);
  }

  async setEnabled(on: boolean) {
    this.enabled = on;
    try {
      await invoke("set_equalizer_enabled", { on });
    } catch {
      // Leave the toggle showing what was asked for; the next change retries.
    }
    void writeSetting("equalizerEnabled", on);
  }
}

export const equalizer = new EqualizerStore();
