/**
 * What an element's box is on the screen, where it is scaled with CSS `zoom`.
 *
 * Engines disagree on what `getBoundingClientRect` returns under `zoom`: the
 * box as it is on the screen (Chromium), which is what the pointer's
 * coordinates are in, or the box as laid out, before the zoom (WebKitGTK).
 * Which of the two it is shows in the rectangle's height against the laid-out
 * one, and a laid-out rectangle is scaled to the screen before the pointer is
 * held against it.
 */

/**
 * The factor that turns `rect`, the bounding box of `element`, into screen
 * coordinates: 1 where the engine reported it as on the screen, `zoom` where
 * it reported it as laid out. An element whose laid-out height is not known
 * (none in jsdom) is taken as reported on the screen.
 */
export function boxScale(element: HTMLElement, rect: DOMRect, zoom: number): number {
  const measured = element.offsetHeight > 0 ? rect.height / element.offsetHeight : zoom;
  return Math.abs(measured - zoom) <= Math.abs(measured - 1) ? 1 : zoom;
}
