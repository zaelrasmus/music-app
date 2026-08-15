/**
 * Cover art for tracks that have none.
 *
 * Local files here carry no embedded artwork and streamed tracks keep no
 * thumbnail, so every tile would otherwise be the same grey square with the
 * same note icon -- which makes a list of forty tracks harder to scan, not
 * easier. A gradient derived from the track's own text gives each one a stable,
 * distinct silhouette: the same song is always the same colours, so the eye
 * learns them, and nothing has to be fetched or stored to get that.
 *
 * Not a claim about the music. It is a placeholder that happens to be
 * recognisable, which is the most a placeholder can honestly be.
 */

/**
 * FNV-1a. Chosen for being short and well distributed over small strings --
 * neighbouring titles like "Track 01" and "Track 02" land far apart, which a
 * naive character sum would not manage.
 */
function hash(text: string): number {
  let h = 0x811c9dc5;
  for (let i = 0; i < text.length; i++) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return h >>> 0;
}

/**
 * A CSS gradient for one track.
 *
 * Lightness and chroma are fixed so no tile can come out muddy or blinding,
 * and both hues are mid-range, which keeps the white overlay icon readable on
 * every one of them -- in either theme, since the tile brings its own ground.
 */
export function coverGradient(seed: string): string {
  const h = hash(seed || "untitled");

  const hue = h % 360;
  // A second hue close enough to look like one object lit from an angle,
  // rather than two unrelated colours meeting in the middle.
  const shift = 24 + ((h >>> 9) % 44);
  const angle = 110 + ((h >>> 17) % 60);

  const from = `oklch(0.66 0.16 ${hue})`;
  const to = `oklch(0.48 0.19 ${(hue + shift) % 360})`;

  return `linear-gradient(${angle}deg, ${from}, ${to})`;
}

/** The text a track's art is derived from. */
export function coverSeed(track: { title: string; artist?: string | null }): string {
  return `${track.artist ?? ""}::${track.title}`;
}
