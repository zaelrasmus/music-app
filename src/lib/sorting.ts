/**
 * How the library list is ordered.
 *
 * The values match `Sort` and `Direction` in `search.rs`; the backend rejects
 * anything else, so a stale persisted value degrades to the default rather
 * than reaching the SQL.
 */
export type Sort =
  | "auto"
  | "title"
  | "artist"
  | "dateAdded"
  | "dateUploaded"
  | "duration"
  | "lastPlayed"
  | "playCount"
  | "custom";

export type Direction = "asc" | "desc";

export type SortOption = {
  id: Sort;
  label: string;
  /** Direction labels, because "A–Z" and "Newest first" are not interchangeable. */
  asc: string;
  desc: string;
  hint?: string;
};

export const SORT_OPTIONS: SortOption[] = [
  {
    id: "auto",
    // Labelled by what it actually does, which depends on whether you are
    // searching. The frontend swaps the word; the backend has one value.
    label: "Default",
    asc: "Default",
    desc: "Default",
  },
  { id: "title", label: "Title", asc: "A – Z", desc: "Z – A" },
  { id: "artist", label: "Artist", asc: "A – Z", desc: "Z – A" },
  {
    id: "dateAdded",
    label: "Date added",
    asc: "Oldest first",
    desc: "Newest first",
  },
  {
    id: "dateUploaded",
    label: "Date uploaded",
    asc: "Oldest first",
    desc: "Newest first",
    // Said once, here, rather than discovered as "why is this order random".
    hint: "SoundCloud reports this when you save a track. YouTube only reveals it when a track is first played, so tracks you have never played sort last.",
  },
  {
    id: "duration",
    label: "Duration",
    asc: "Shortest first",
    desc: "Longest first",
  },
  {
    id: "lastPlayed",
    label: "Last played",
    asc: "Longest ago first",
    desc: "Most recent first",
    // Said once, because the alternative is wondering why a track skipped
    // past yesterday is not at the top.
    hint: "Counted only after thirty seconds, so this is what you listened to rather than what you skipped past. Tracks never played sort last.",
  },
  {
    id: "playCount",
    label: "Most played",
    asc: "Least first",
    desc: "Most first",
  },
];

/**
 * The playlist view offers one more: the order the user put them in.
 *
 * First, and the default, because it is the only mode in which dragging a row
 * means anything -- every other option is a view over the same list, and the
 * panel turns reordering off while one is active.
 */
export const PLAYLIST_SORT_OPTIONS: SortOption[] = [
  {
    id: "custom",
    label: "Custom order",
    asc: "As arranged",
    desc: "As arranged",
    hint: "The order you put them in. Drag to rearrange — which only works here, because under any other sort a row's position on screen is not its position in the playlist.",
  },
  ...SORT_OPTIONS.filter((option) => option.id !== "auto"),
];

export function sortOption(id: Sort): SortOption {
  return (
    PLAYLIST_SORT_OPTIONS.find((o) => o.id === id) ??
    SORT_OPTIONS.find((o) => o.id === id) ??
    SORT_OPTIONS[0]
  );
}

/**
 * What the control should say.
 *
 * "Default" means relevance while searching and artist otherwise — two
 * different orders behind one value, so the label has to tell you which you
 * are looking at.
 */
export function sortLabel(id: Sort, searching: boolean): string {
  if (id !== "auto") return sortOption(id).label;
  return searching ? "Best match" : "Artist";
}
