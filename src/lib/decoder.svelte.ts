import { invoke } from "@tauri-apps/api/core";

export type DecoderStatus = {
  present: boolean;
  path: string | null;
  message: string | null;
};

/**
 * Whether the app can play anything at all.
 *
 * ffmpeg decodes every track — local files included — so its absence is not a
 * degraded mode, it is a silent app. The library still lists, playlists still
 * open, settings still work, and then every single track fails. Asking once at
 * launch is what lets the app say so while the listener is still looking at a
 * screen that appears to be working.
 *
 * Deliberately optimistic until told otherwise: a warning that flashes up for
 * one frame on every launch, before the answer arrives, would be worse than no
 * warning at all.
 */
class DecoderStore {
  present = $state(true);
  path = $state<string | null>(null);
  message = $state<string | null>(null);
  /** Whether the question has been answered yet. */
  checked = $state(false);

  async refresh() {
    try {
      const status = await invoke<DecoderStatus>("decoder_status");
      this.present = status.present;
      this.path = status.path;
      this.message = status.message;
    } catch {
      // The command itself failing says nothing about ffmpeg, so this must not
      // claim the decoder is missing — that would put a scary, wrong banner in
      // front of someone whose audio is fine.
    } finally {
      this.checked = true;
    }
  }
}

export const decoder = new DecoderStore();
