import { LazyStore } from "@tauri-apps/plugin-store";

/**
 * Device-local UI preferences — and nothing else.
 *
 * The rule for this file: it holds settings that are meaningless on another
 * device and must never sync (volume, mute, repeat, shuffle, window state).
 * Library data lives in SQLite. Duplicating any fact across both is how you
 * end up with two sources of truth that quietly disagree.
 */
const store = new LazyStore("settings.json");

export async function readSetting<T>(key: string, fallback: T): Promise<T> {
  try {
    return (await store.get<T>(key)) ?? fallback;
  } catch (e) {
    // Preferences are best-effort: a corrupt store must not stop playback.
    console.debug(`could not read setting "${key}"`, e);
    return fallback;
  }
}

export async function writeSetting<T>(key: string, value: T): Promise<void> {
  try {
    await store.set(key, value);
    await store.save();
  } catch (e) {
    console.debug(`could not persist setting "${key}"`, e);
  }
}
