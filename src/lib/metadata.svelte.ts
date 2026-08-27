import { invoke } from "@tauri-apps/api/core";
import { trackStore, type Track } from "$lib/tracks.svelte";

/** What the file's own tags say. Null for anything with no file to read. */
export type FileTags = {
  title: string;
  artist: string | null;
  album: string | null;
};

/**
 * The metadata editor.
 *
 * A store plus one dialog mounted in the layout, the same shape as
 * `prompt.svelte.ts` — and for the same reason, which is that the alternative
 * is every list that shows a track owning a modal.
 *
 * It replaced two inputs sitting inside the row. That worked for a title and
 * an artist and stopped working at three fields plus the file's own tags: the
 * inline form took the place of the play button, so editing a track hid the
 * track you were editing.
 */
class MetadataEditor {
  /** The track being edited. `null` when the dialog is closed. */
  track = $state<Track | null>(null);

  title = $state("");
  artist = $state("");
  album = $state("");

  /**
   * What the file says, or null when there is nothing to compare against —
   * a streamed track, or a local file not read since the scanner started
   * recording tags separately.
   */
  fileTags = $state<FileTags | null>(null);

  saving = $state(false);

  get open() {
    return this.track !== null;
  }

  /**
   * Whether the boxes currently say something other than the file does.
   *
   * Drives both the "what the file says" panel and whether reverting is
   * offered at all — a row already matching its file has nothing to go back
   * to, and a button that would do nothing is worse than no button.
   *
   * Compares the boxes rather than the saved row so it answers live while
   * someone types, which is what makes the panel read as a comparison instead
   * of a status.
   */
  differsFromFile = $derived.by(() => {
    const file = this.fileTags;
    if (!file) return false;
    const same = (box: string, tag: string | null) =>
      box.trim() === (tag ?? "");
    return !(
      same(this.title, file.title) &&
      same(this.artist, file.artist) &&
      same(this.album, file.album)
    );
  });

  async edit(track: Track) {
    this.track = track;
    this.title = track.title;
    this.artist = track.artist ?? "";
    this.album = track.album ?? "";

    // Cleared first: the dialog is already on screen by the time this
    // resolves, and last track's tags flashing up under this track's name
    // would be a lie, however briefly.
    this.fileTags = null;
    const id = track.id;
    try {
      const tags = await invoke<FileTags | null>("track_file_tags", {
        trackId: id,
      });
      // Someone can open one row, close it and open another faster than this
      // returns.
      if (this.track?.id === id) this.fileTags = tags;
    } catch {
      // Not being able to say what the file holds is not worth a toast. The
      // panel simply does not appear, and editing still works.
      if (this.track?.id === id) this.fileTags = null;
    }
  }

  /**
   * Fills the boxes from the file's tags without saving.
   *
   * Deliberately not a save. Reverting is a change like any other, and seeing
   * it before committing is the difference between an undo and a surprise —
   * particularly for the rows where the file's tags are the reason someone
   * started editing.
   */
  useFileTags() {
    const file = this.fileTags;
    if (!file) return;
    this.title = file.title;
    this.artist = file.artist ?? "";
    this.album = file.album ?? "";
  }

  close() {
    this.track = null;
    this.fileTags = null;
    this.saving = false;
  }

  async save() {
    const track = this.track;
    if (!track || this.saving) return;
    if (this.title.trim() === "") return;

    this.saving = true;
    const blank = (v: string) => (v.trim() === "" ? null : v.trim());
    const ok = await trackStore.updateMetadata(
      track.id,
      this.title.trim(),
      blank(this.artist),
      blank(this.album),
    );
    this.saving = false;
    if (ok) this.close();
  }
}

export const metadataEditor = new MetadataEditor();
