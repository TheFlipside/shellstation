/**
 * Where a button of the command bar goes when it is dragged.
 *
 * The bar's buttons are one row that wraps onto further lines. A dragged
 * button goes before or after the button the pointer is nearest, by the half
 * of it the pointer is on: the one under the pointer, or, where the pointer is
 * between buttons or beside the last of a line, the nearest one on the
 * pointer's line. `buttonPointAt` finds that button from where the pointer
 * is, and `buttonDropAt` says what a drop there does, or `null` where it
 * leaves the order as it is. The rules are here and not in the bar so that
 * they can be tested without a layout.
 */
import { boxScale } from "./zoomBox";

export type ButtonSide = "before" | "after";

/** The button the pointer is nearest, and on which half of it. */
export interface ButtonPoint {
  id: string;
  side: ButtonSide;
}

export interface ButtonDrop {
  /** Where the bar shows the line: at this side of this button. */
  mark: ButtonPoint;
  /** Where the dragged button is now. */
  from: number;
  /**
   * Where it is afterwards, among the buttons without it, as
   * `reorderCommandButtons` takes the two.
   */
  to: number;
}

/**
 * What a drop of `dragged` at `point` does to the order `ids`, or `null`
 * where it does nothing: beside the button itself, or at the near edge of the
 * button before or after it. A button that is not in `ids`, as one deleted
 * while the other was held, goes nowhere.
 */
export function buttonDropAt(
  ids: readonly string[],
  dragged: string,
  point: ButtonPoint,
): ButtonDrop | null {
  const from = ids.indexOf(dragged);
  if (from < 0 || point.id === dragged) return null;
  const at = ids.filter((id) => id !== dragged).indexOf(point.id);
  if (at < 0) return null;
  const to = point.side === "before" ? at : at + 1;
  return to === from ? null : { mark: point, from, to };
}

/** Whether two drops show the same and do the same. */
export function sameButtonDrop(a: ButtonDrop | null, b: ButtonDrop | null): boolean {
  if (a === null || b === null) return a === b;
  return (
    a.from === b.from && a.to === b.to && a.mark.id === b.mark.id && a.mark.side === b.mark.side
  );
}

/**
 * The button of `row` that a pointer at a point of the window is nearest, or
 * `null` where the pointer is not over the row. What is hit is asked of the
 * point (`elementFromPoint`), so nothing that lies over the bar, such as a
 * dialog, lets a drop through. The buttons are those with a
 * `data-button-id`; the nearest is the one on the pointer's line, or the
 * line closest to it, that is closest across. `zoom` is the bar's.
 */
export function buttonPointAt(
  x: number,
  y: number,
  row: HTMLElement,
  zoom: number,
): ButtonPoint | null {
  const under = document.elementFromPoint(x, y);
  if (!under || !row.contains(under)) return null;
  let best: (ButtonPoint & { down: number; across: number }) | null = null;
  for (const button of row.querySelectorAll<HTMLElement>("[data-button-id]")) {
    const id = button.dataset.buttonId;
    const rect = button.getBoundingClientRect();
    if (!id || rect.width <= 0 || rect.height <= 0) continue;
    const scale = boxScale(button, rect, zoom);
    const left = rect.left * scale;
    const right = rect.right * scale;
    const top = rect.top * scale;
    const bottom = rect.bottom * scale;
    const down = Math.max(top - y, 0, y - bottom);
    const across = Math.max(left - x, 0, x - right);
    if (best === null || down < best.down || (down === best.down && across < best.across)) {
      const side: ButtonSide = x < (left + right) / 2 ? "before" : "after";
      best = { id, side, down, across };
    }
  }
  return best && { id: best.id, side: best.side };
}
