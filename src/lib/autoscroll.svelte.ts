/**
 * Scrolls a list while something is being dragged near its edge.
 *
 * Without this, a drag can only move a track as far as the screen: to send one
 * from position 40 to position 1 you would have to drop it, scroll, pick it up
 * again, and repeat. The list is virtualised, so "just make the list shorter"
 * is not available either.
 *
 * Driven by `requestAnimationFrame` rather than by the drag events themselves.
 * `dragover` fires only while the pointer *moves*, so a cursor held still at
 * the top edge — which is exactly what someone waiting for a list to scroll
 * does — would stop producing events and the scrolling would stall.
 */

/** How close to an edge the pointer must be, in pixels. */
const EDGE = 64;

/** Fastest scroll, in pixels per frame, reached at the very edge. */
const MAX_SPEED = 18;

let frame: number | null = null;
let scroller: HTMLElement | null = null;
let pointerY = 0;

function step() {
  frame = null;
  if (!scroller) return;

  const box = scroller.getBoundingClientRect();
  const fromTop = pointerY - box.top;
  const fromBottom = box.bottom - pointerY;

  // Speed ramps with proximity, so easing towards the edge nudges and sitting
  // on it moves properly. A single fixed speed is either too slow to be worth
  // waiting for or too fast to stop where you meant to.
  let delta = 0;
  if (fromTop < EDGE) delta = -MAX_SPEED * (1 - Math.max(fromTop, 0) / EDGE);
  else if (fromBottom < EDGE) delta = MAX_SPEED * (1 - Math.max(fromBottom, 0) / EDGE);

  if (delta !== 0) scroller.scrollTop += delta;

  frame = requestAnimationFrame(step);
}

/** Called from `dragover`, which is the only reliable source of a pointer position during a drag. */
export function autoScrollTowards(element: HTMLElement | null, clientY: number) {
  pointerY = clientY;

  if (!element) return;
  if (scroller !== element) scroller = element;
  if (frame === null) frame = requestAnimationFrame(step);
}

/** Called on `dragend` and on drop. Safe to call when nothing is running. */
export function stopAutoScroll() {
  if (frame !== null) cancelAnimationFrame(frame);
  frame = null;
  scroller = null;
}
