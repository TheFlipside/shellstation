/**
 * Where a dragged button of the command bar goes: which button the pointer
 * is nearest, and what a drop there does to the order.
 *
 * jsdom lays nothing out, so a test says what is under the pointer and where
 * each button's box is.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { buttonDropAt, buttonPointAt, sameButtonDrop, type ButtonDrop } from "./buttonDrop";

describe("buttonDropAt", () => {
  const ids = ["a", "b", "c", "d"];

  /** The order after the drop, as the store's reorder makes it. */
  function after(drop: ButtonDrop | null): string[] | null {
    if (drop === null) return null;
    const next = [...ids];
    const [moved] = next.splice(drop.from, 1);
    next.splice(drop.to, 0, moved);
    return next;
  }

  it("puts a button before or after another, by the half it is dropped on", () => {
    expect(after(buttonDropAt(ids, "a", { id: "c", side: "before" }))).toEqual([
      "b",
      "a",
      "c",
      "d",
    ]);
    expect(after(buttonDropAt(ids, "a", { id: "c", side: "after" }))).toEqual(["b", "c", "a", "d"]);
    expect(after(buttonDropAt(ids, "a", { id: "d", side: "after" }))).toEqual(["b", "c", "d", "a"]);
    expect(after(buttonDropAt(ids, "d", { id: "a", side: "before" }))).toEqual([
      "d",
      "a",
      "b",
      "c",
    ]);
    expect(after(buttonDropAt(ids, "d", { id: "b", side: "after" }))).toEqual(["a", "b", "d", "c"]);
  });

  it("says where the line is", () => {
    expect(buttonDropAt(ids, "a", { id: "c", side: "after" })).toEqual({
      mark: { id: "c", side: "after" },
      from: 0,
      to: 2,
    });
  });

  it("does nothing where the button stays where it is", () => {
    // Beside itself.
    expect(buttonDropAt(ids, "b", { id: "b", side: "before" })).toBeNull();
    expect(buttonDropAt(ids, "b", { id: "b", side: "after" })).toBeNull();
    // At the near edge of the button before it, and of the one after it.
    expect(buttonDropAt(ids, "b", { id: "a", side: "after" })).toBeNull();
    expect(buttonDropAt(ids, "b", { id: "c", side: "before" })).toBeNull();
    // The far edges are another place.
    expect(buttonDropAt(ids, "b", { id: "a", side: "before" })).not.toBeNull();
    expect(buttonDropAt(ids, "b", { id: "c", side: "after" })).not.toBeNull();
  });

  it("does nothing for a button that is not there", () => {
    expect(buttonDropAt(ids, "x", { id: "a", side: "before" })).toBeNull();
    expect(buttonDropAt(ids, "a", { id: "x", side: "before" })).toBeNull();
  });
});

describe("sameButtonDrop", () => {
  it("compares the place and the line", () => {
    const drop = buttonDropAt(["a", "b", "c"], "a", { id: "c", side: "after" });
    expect(sameButtonDrop(drop, { mark: { id: "c", side: "after" }, from: 0, to: 2 })).toBe(true);
    // The same place, marked at another button.
    expect(
      sameButtonDrop(
        buttonDropAt(["a", "b", "c", "d"], "a", { id: "c", side: "after" }),
        buttonDropAt(["a", "b", "c", "d"], "a", { id: "d", side: "before" }),
      ),
    ).toBe(false);
    expect(sameButtonDrop(null, null)).toBe(true);
    expect(sameButtonDrop(drop, null)).toBe(false);
  });
});

describe("buttonPointAt", () => {
  /**
   * A bar of two lines: the toggle, "+", a and b on the first, c on the
   * second; each button 40 wide and 20 high, 4 apart, and the lines 4 apart.
   */
  function page(): {
    row: HTMLElement;
    el: (id: string) => HTMLElement;
  } {
    document.body.innerHTML = `
      <div id="row">
        <button id="toggle"></button><button id="add"></button>
        <button id="a" data-button-id="a"><span id="label">a</span></button>
        <button id="b" data-button-id="b"></button>
        <button id="c" data-button-id="c"></button>
      </div>
      <div id="outside" data-button-id="z"></div>`;
    const el = (id: string): HTMLElement => {
      const found = document.getElementById(id);
      if (!found) throw new Error(`no #${id}`);
      return found;
    };
    box(el("toggle"), 0, 0, 20);
    box(el("add"), 24, 0, 20);
    box(el("a"), 48, 0, 40);
    box(el("b"), 92, 0, 40);
    box(el("c"), 0, 24, 40);
    return { row: el("row"), el };
  }

  /** What is at every point of the window. */
  function under(element: Element | null): void {
    vi.spyOn(document, "elementFromPoint").mockReturnValue(element);
  }

  /**
   * A button's box as the engine reports it, and its height as laid out
   * (that of the box unless given).
   */
  function box(
    button: HTMLElement,
    left: number,
    top: number,
    width: number,
    height = 20,
    laidOutHeight = height,
  ): void {
    vi.spyOn(button, "getBoundingClientRect").mockReturnValue({
      left,
      top,
      width,
      height,
      right: left + width,
      bottom: top + height,
      x: left,
      y: top,
      toJSON: () => ({}),
    });
    Object.defineProperty(button, "offsetHeight", { configurable: true, value: laidOutHeight });
  }

  afterEach(() => {
    vi.restoreAllMocks();
    document.body.innerHTML = "";
  });

  it("finds the button under the pointer, and the half of it", () => {
    const { row, el } = page();
    // What is hit is something inside the button.
    under(el("label"));
    expect(buttonPointAt(60, 10, row, 1)).toEqual({ id: "a", side: "before" });
    under(el("a"));
    expect(buttonPointAt(80, 10, row, 1)).toEqual({ id: "a", side: "after" });
    under(el("c"));
    expect(buttonPointAt(30, 30, row, 1)).toEqual({ id: "c", side: "after" });
  });

  it("takes the nearest button on the pointer's line where it is between buttons", () => {
    const { row, el } = page();
    under(row);
    // In the gap between a and b, nearer b.
    expect(buttonPointAt(91, 10, row, 1)).toEqual({ id: "b", side: "before" });
    // Beside the last button of the first line, and of the second.
    expect(buttonPointAt(300, 10, row, 1)).toEqual({ id: "b", side: "after" });
    expect(buttonPointAt(300, 30, row, 1)).toEqual({ id: "c", side: "after" });
    // Over "+", which is not moved: before the first button.
    under(el("add"));
    expect(buttonPointAt(30, 10, row, 1)).toEqual({ id: "a", side: "before" });
    // Between the two lines, nearer the second.
    under(row);
    expect(buttonPointAt(10, 23, row, 1)).toEqual({ id: "c", side: "before" });
  });

  it("finds the same button in a scaled bar, whichever box the engine reports", () => {
    const { row, el } = page();
    under(el("b"));
    // b laid out at 92 wide 40, in a bar scaled to 125%: on the screen it
    // starts at 115 and is 50 wide, so 135 is in its first half and 145 in
    // its second.
    box(el("a"), 60, 0, 50, 25, 20);
    box(el("b"), 115, 0, 50, 25, 20);
    box(el("c"), 0, 30, 50, 25, 20);
    expect(buttonPointAt(135, 10, row, 1.25)).toEqual({ id: "b", side: "before" });
    expect(buttonPointAt(145, 10, row, 1.25)).toEqual({ id: "b", side: "after" });
    // The boxes as laid out, before the scale.
    box(el("a"), 48, 0, 40);
    box(el("b"), 92, 0, 40);
    box(el("c"), 0, 24, 40);
    expect(buttonPointAt(135, 10, row, 1.25)).toEqual({ id: "b", side: "before" });
    expect(buttonPointAt(145, 10, row, 1.25)).toEqual({ id: "b", side: "after" });
  });

  it("finds nothing where the pointer is not over the bar", () => {
    const { row, el } = page();
    // A button elsewhere, as one of a dialog over the bar would be.
    under(el("outside"));
    expect(buttonPointAt(60, 10, row, 1)).toBeNull();
    under(document.body);
    expect(buttonPointAt(60, 10, row, 1)).toBeNull();
    under(null);
    expect(buttonPointAt(60, 10, row, 1)).toBeNull();
  });

  it("passes over a button without a box, and finds nothing in a bar without buttons", () => {
    const { row, el } = page();
    under(row);
    box(el("b"), 0, 0, 0, 0);
    expect(buttonPointAt(300, 10, row, 1)).toEqual({ id: "a", side: "after" });
    document.body.innerHTML = `<div id="empty"></div>`;
    const empty = document.getElementById("empty");
    if (!empty) throw new Error("no #empty");
    under(empty);
    expect(buttonPointAt(10, 10, empty, 1)).toBeNull();
  });
});
