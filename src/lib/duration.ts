/**
 * A total running time, in the shape a header wants.
 *
 * `3h 12m` rather than `3:12:44`: at this scale the seconds are noise, and a
 * colon-separated total is easily misread as a position within a track. Rows
 * keep `m:ss`, where the exact figure is the point.
 */
export function formatTotal(secs: number): string | null {
  if (!Number.isFinite(secs) || secs <= 0) return null;

  const total = Math.round(secs);
  const hours = Math.floor(total / 3600);
  const minutes = Math.round((total % 3600) / 60);

  if (hours === 0) return `${minutes} min`;
  // A trailing "0m" reads as a rounding artefact rather than as exactly n hours.
  return minutes === 0 ? `${hours} hr` : `${hours} hr ${minutes} min`;
}
