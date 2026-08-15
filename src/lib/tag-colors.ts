/**
 * Tag colours.
 *
 * A tag carries a palette *name*; this resolves it to an oklch hue. Lightness
 * and chroma are theme variables (see `.tag-chip` in layout.css), so one number
 * per colour is genuinely all that is needed, and every chip stays legible in
 * both light and dark without a second palette being maintained for each.
 *
 * The order matches `TAG_COLORS` in `tags.rs`; the picker reads its list from
 * the backend, so the two cannot silently disagree about which names exist.
 */
export const TAG_HUES: Record<string, number> = {
  rose: 12,
  orange: 55,
  amber: 85,
  lime: 130,
  emerald: 165,
  teal: 195,
  sky: 235,
  indigo: 270,
  violet: 300,
  fuchsia: 330,
};

const NAMES = Object.keys(TAG_HUES);

/**
 * The hue for a tag, whether or not anyone chose one.
 *
 * An unset colour falls back to the tag's id rather than a hash of its name:
 * ids are assigned in creation order, so the first ten tags in a library get
 * ten visibly different colours instead of whatever a hash happens to collide
 * on -- and renaming a tag does not change its colour underneath the user.
 */
export function tagHue(tagId: number, color: string | null | undefined): number {
  if (color && color in TAG_HUES) return TAG_HUES[color];
  return TAG_HUES[NAMES[Math.abs(tagId) % NAMES.length]];
}
