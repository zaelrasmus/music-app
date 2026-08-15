import { invoke } from "@tauri-apps/api/core";
import { toast } from "svelte-sonner";
import { SvelteMap } from "svelte/reactivity";

export type Tag = {
  id: number;
  name: string;
  trackCount: number;
  /** A palette name, or null for "colour this one automatically". */
  color: string | null;
};

/** A tag as it appears on a track: enough to draw a chip, and no more. */
export type TagRef = {
  id: number;
  name: string;
  color: string | null;
};

export type TrackTag = TagRef & {
  trackId: number;
  tagId: number;
};

/**
 * Tags, plus every (track, tag) pair in the library.
 *
 * The pairs are fetched wholesale rather than per row: chips are wanted on
 * every track, and one request for all of them beats one request per track.
 */
class TagStore {
  tags = $state<Tag[]>([]);
  /** trackId -> its tags, rebuilt whenever the pairs change. */
  byTrack = new SvelteMap<number, TagRef[]>();
  /** The colours the backend will accept, so the picker cannot offer others. */
  palette = $state<string[]>([]);

  async load() {
    try {
      const [tags, pairs] = await Promise.all([
        invoke<Tag[]>("list_tags"),
        invoke<TrackTag[]>("list_track_tags"),
      ]);

      this.tags = tags;

      this.byTrack.clear();
      for (const pair of pairs) {
        const list = this.byTrack.get(pair.trackId) ?? [];
        list.push({ id: pair.tagId, name: pair.name, color: pair.color });
        this.byTrack.set(pair.trackId, list);
      }
    } catch (e) {
      toast.error(String(e));
    }
  }

  /** Fixed for the life of the process, so it is fetched once. */
  async loadPalette() {
    if (this.palette.length > 0) return;
    try {
      this.palette = await invoke<string[]>("list_tag_colors");
    } catch (e) {
      console.debug("could not load the tag palette", e);
    }
  }

  forTrack(trackId: number): TagRef[] {
    return this.byTrack.get(trackId) ?? [];
  }

  /** Creates the tag if it does not exist, then attaches it. */
  async assign(trackId: number, name: string) {
    try {
      await invoke("assign_tag", { trackId, name });
      await this.load();
    } catch (e) {
      toast.error(String(e));
    }
  }

  async remove(trackId: number, tagId: number) {
    try {
      await invoke("remove_tag_from_track", { trackId, tagId });
      await this.load();
    } catch (e) {
      toast.error(String(e));
    }
  }

  async rename(tagId: number, name: string) {
    try {
      await invoke("rename_tag", { tagId, name });
      await this.load();
      return true;
    } catch (e) {
      toast.error(String(e));
      return false;
    }
  }

  /**
   * Sets a tag's colour, or clears it with `null`.
   *
   * Reloads everything afterwards rather than patching the one row: the same
   * tag is drawn on every track that carries it, and those chips live in
   * `byTrack`, not in `tags`.
   */
  async setColor(tagId: number, color: string | null) {
    try {
      await invoke("set_tag_color", { tagId, color });
      await this.load();
    } catch (e) {
      toast.error(String(e));
    }
  }

  async destroy(tagId: number) {
    try {
      await invoke("delete_tag", { tagId });
      await this.load();
    } catch (e) {
      toast.error(String(e));
    }
  }
}

export const tagStore = new TagStore();
