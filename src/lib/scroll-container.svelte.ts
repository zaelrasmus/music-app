import { getContext, setContext } from "svelte";

const KEY = Symbol("scroll-container");

export type ScrollContainer = {
  readonly element: HTMLElement | null;
};

/**
 * Offers this component's scrolling element to whatever renders inside it.
 *
 * `PageShell` owns the scroller for every view — that is the whole point of
 * it, one gutter and a header that never moves — but a virtualised list needs
 * a handle on that element to know what is on screen. Passing it down as a
 * prop would mean every view threading it through, and views that virtualise
 * nothing would carry it anyway.
 *
 * A getter rather than the element itself: it is bound after the context is
 * set, so anything reading it early would capture `undefined` forever.
 */
export function provideScrollContainer(
  element: () => HTMLElement | null | undefined,
) {
  setContext<ScrollContainer>(KEY, {
    get element() {
      return element() ?? null;
    },
  });
}

/** The nearest scrolling element, if something above provided one. */
export function scrollContainer(): ScrollContainer | undefined {
  return getContext<ScrollContainer | undefined>(KEY);
}
