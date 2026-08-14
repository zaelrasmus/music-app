import { invoke } from "@tauri-apps/api/core";
import { toast } from "svelte-sonner";
import { SvelteMap } from "svelte/reactivity";

export type Tag = {
  id: number;
  name: string;
  trackCount: number;
};

export type TrackTag = {
  trackId: number;
  tagId: number;
  name: string;
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
  byTrack = new SvelteMap<number, Tag[]>();

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
        list.push({ id: pair.tagId, name: pair.name, trackCount: 0 });
        this.byTrack.set(pair.trackId, list);
      }
    } catch (e) {
      toast.error(String(e));
    }
  }

  forTrack(trackId: number): Tag[] {
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
