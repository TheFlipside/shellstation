/**
 * Dragging a button of the command bar to another place.
 *
 * Where a drop goes is decided in `buttonDrop.ts` and pinned in its own
 * test. The contract pinned here is what the bar makes of it: a button is
 * picked up with the pointer once it has moved, and a press that does not
 * move is a click; the place a drop would go to is marked while it is
 * dragged; a drop puts the button there, and sends nothing to a terminal;
 * the focus goes back to the terminal; Escape and a dialog that opens end
 * the drag; nothing is picked up under a dialog.
 *
 * jsdom lays nothing out, so a test says what is under the pointer and
 * where each button is.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import type { Terminal as XTerm } from "@xterm/xterm";
import { useSettingsStore } from "../stores/settingsStore";
import { useTerminalStore } from "../stores/terminalStore";
import { registerTerminal, unregisterTerminal } from "../terminalRegistry";
import { CommandBar } from "./CommandBar";
import { ConfirmDialog } from "./ConfirmDialog";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(() => Promise.resolve()) }));
vi.mock("@tauri-apps/plugin-clipboard-manager", () => ({
  readText: vi.fn(() => Promise.resolve("")),
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("../i18n", () => ({
  default: { t: (key: string) => key, changeLanguage: vi.fn() },
}));

// addCommandButton takes only v4 UUIDs.
const IDS = {
  one: "1b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
  two: "2b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
  three: "3b1deb4d-3b7d-4bad-9bdd-2b0d7b3dcb6d",
};
type Name = keyof typeof IDS;
const NAMES: Name[] = ["one", "two", "three"];

/** The buttons' names, in the store's order. */
function order(): string[] {
  return useSettingsStore.getState().commandButtons.map((b) => b.name);
}

function button(name: Name): HTMLElement {
  return screen.getByText(name, { selector: ".command-bar-btn" });
}

/** The places marked, as "before two" or "after three". */
function marks(): string[] {
  return NAMES.flatMap((name) => {
    const cls = button(name).classList;
    if (cls.contains("command-bar-btn-drop-before")) return [`before ${name}`];
    if (cls.contains("command-bar-btn-drop-after")) return [`after ${name}`];
    return [];
  });
}

function preview(): HTMLElement | null {
  return document.querySelector(".command-bar-drag-preview");
}

const TOP = 100;

/**
 * Lays the buttons out on one line, 40 wide and 4 apart from x = 100, and
 * says what is under the pointer from now on.
 */
function layOut(under: Element): void {
  NAMES.forEach((name, i) => {
    const left = 100 + i * 44;
    vi.spyOn(button(name), "getBoundingClientRect").mockReturnValue({
      left,
      top: TOP,
      width: 40,
      height: 20,
      right: left + 40,
      bottom: TOP + 20,
      x: left,
      y: TOP,
      toJSON: () => ({}),
    });
  });
  vi.spyOn(document, "elementFromPoint").mockReturnValue(under);
}

/** The pointer over a button, `ratio` of the way across it. */
function moveOver(name: Name, ratio: number): void {
  layOut(button(name));
  const left = 100 + NAMES.indexOf(name) * 44;
  fireEvent.pointerMove(document, {
    isPrimary: true,
    clientX: left + 40 * ratio,
    clientY: TOP + 10,
  });
}

/** Presses on a button and moves far enough for the drag to begin. */
function pickUp(name: Name): void {
  const left = 100 + NAMES.indexOf(name) * 44;
  fireEvent.pointerDown(button(name), {
    isPrimary: true,
    button: 0,
    clientX: left + 20,
    clientY: TOP + 10,
  });
  fireEvent.pointerMove(document, { isPrimary: true, clientX: left + 20, clientY: TOP + 30 });
}

/**
 * Lets go, and clicks where the button was pressed, as the release does in
 * a browser when it is over the same button.
 */
function release(pressed: Name): void {
  fireEvent.pointerUp(document, { isPrimary: true, clientX: 0, clientY: TOP });
  fireEvent.click(button(pressed));
}

let terminalInput: HTMLTextAreaElement;
const fakeTerm = { scrollToBottom: vi.fn() } as unknown as XTerm;

beforeEach(() => {
  vi.clearAllMocks();
  useSettingsStore.setState({ commandButtons: [] });
  for (const name of NAMES) {
    useSettingsStore
      .getState()
      .addCommandButton({ id: IDS[name], name, command: `${name}\\n`, color: "#89b4fa" });
  }
  useTerminalStore.getState().addTab("tab-1", "tab-1", "ssh");
  registerTerminal("tab-1", fakeTerm);
  // The terminal's input, which the bar gives the focus back to.
  const instance = document.createElement("div");
  instance.className = "terminal-instance";
  terminalInput = document.createElement("textarea");
  terminalInput.className = "xterm-helper-textarea";
  instance.appendChild(terminalInput);
  document.body.appendChild(instance);
});

afterEach(async () => {
  vi.restoreAllMocks();
  unregisterTerminal("tab-1", fakeTerm);
  useTerminalStore.getState().removeTab("tab-1");
  useSettingsStore.setState({ commandButtons: [] });
  document.querySelector(".terminal-instance")?.remove();
  // The library stops every click on the document for 50 ms after a drag,
  // which would swallow the next test's click. Its timer was set at the
  // release, before this one, with less time, so it has run when this has.
  await new Promise((resolve) => setTimeout(resolve, 60));
});

describe("CommandBar drag", () => {
  it("puts a button where it is dropped, and sends nothing", () => {
    render(<CommandBar uiScale={100} />);
    pickUp("one");
    moveOver("three", 0.75);
    expect(marks()).toEqual(["after three"]);
    release("one");
    expect(order()).toEqual(["two", "three", "one"]);
    expect(invoke).not.toHaveBeenCalled();

    // A second drag at once, within the 50 ms in which the library stops
    // clicks after the first: its release's click is stopped by its own.
    pickUp("one");
    moveOver("two", 0.25);
    expect(marks()).toEqual(["before two"]);
    release("one");
    expect(order()).toEqual(["one", "two", "three"]);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("announces nothing to a screen reader", () => {
    render(<CommandBar uiScale={100} />);
    pickUp("one");
    moveOver("three", 0.75);
    const live = document.querySelector('[aria-live="assertive"]');
    expect(live).not.toBeNull();
    expect(live?.textContent).toBe("");
    release("one");
    expect(live?.textContent).toBe("");
  });

  it("takes a press that does not move for a click", async () => {
    render(<CommandBar uiScale={100} />);
    fireEvent.pointerDown(button("two"), {
      isPrimary: true,
      button: 0,
      clientX: 160,
      clientY: 110,
    });
    fireEvent.pointerMove(document, { isPrimary: true, clientX: 162, clientY: 112 });
    expect(preview()).toBeNull();
    release("two");
    expect(order()).toEqual(["one", "two", "three"]);
    await vi.waitFor(() => {
      expect(invoke).toHaveBeenCalledWith("ssh_write", { id: "tab-1", data: "two\n" });
    });
  });

  it("picks nothing up with another button than the primary", () => {
    render(<CommandBar uiScale={100} />);
    fireEvent.pointerDown(button("one"), {
      isPrimary: true,
      button: 2,
      clientX: 120,
      clientY: 110,
    });
    fireEvent.pointerMove(document, { isPrimary: true, clientX: 120, clientY: 140 });
    expect(preview()).toBeNull();
    moveOver("three", 0.75);
    expect(marks()).toEqual([]);
  });

  it("marks nothing, and drops nothing, where the button stays where it is", () => {
    render(<CommandBar uiScale={100} />);
    pickUp("two");
    moveOver("two", 0.25);
    expect(marks()).toEqual([]);
    moveOver("one", 0.75);
    expect(marks()).toEqual([]);
    moveOver("three", 0.25);
    expect(marks()).toEqual([]);
    release("two");
    expect(order()).toEqual(["one", "two", "three"]);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("drops nothing outside the bar", () => {
    render(<CommandBar uiScale={100} />);
    pickUp("one");
    moveOver("three", 0.75);
    expect(marks()).toEqual(["after three"]);
    layOut(document.body);
    fireEvent.pointerMove(document, { isPrimary: true, clientX: 500, clientY: 500 });
    expect(marks()).toEqual([]);
    release("one");
    expect(order()).toEqual(["one", "two", "three"]);
  });

  it("asks again at the release where the pointer is", () => {
    render(<CommandBar uiScale={100} />);
    pickUp("one");
    moveOver("three", 0.75);
    expect(marks()).toEqual(["after three"]);
    // Something comes to lie over the bar without the pointer moving.
    vi.spyOn(document, "elementFromPoint").mockReturnValue(document.body);
    release("one");
    expect(order()).toEqual(["one", "two", "three"]);
  });

  it("dims the dragged button and shows its preview, and leaves no tooltip meanwhile", () => {
    render(<CommandBar uiScale={125} />);
    expect(button("two").title).toBe("two\\n");
    pickUp("one");
    expect(button("one").classList.contains("command-bar-btn-dragged")).toBe(true);
    expect(button("two").classList.contains("command-bar-btn-dragged")).toBe(false);
    expect(button("two").title).toBe("");
    expect(preview()?.textContent).toBe("one");
    // Beside the pointer, and at the bar's scale. Until the pointer moves
    // again it is beside the press, 20 px from where the drag began.
    expect(preview()?.style.transform).toBe("translate(132px, 126px) scale(1.25)");
    fireEvent.pointerMove(document, { isPrimary: true, clientX: 300, clientY: 50 });
    expect(preview()?.style.transform).toBe("translate(312px, 66px) scale(1.25)");
    release("one");
    expect(preview()).toBeNull();
    expect(button("one").classList.contains("command-bar-btn-dragged")).toBe(false);
    expect(button("two").title).toBe("two\\n");
  });

  it("gives the focus back to the terminal", () => {
    render(<CommandBar uiScale={100} />);
    button("one").focus();
    pickUp("one");
    moveOver("three", 0.75);
    release("one");
    expect(document.activeElement).toBe(terminalInput);
  });

  it("ends the drag on Escape, and drops nothing", () => {
    render(<CommandBar uiScale={100} />);
    button("one").focus();
    pickUp("one");
    moveOver("three", 0.75);
    expect(marks()).toEqual(["after three"]);
    expect(preview()).not.toBeNull();
    fireEvent.keyDown(document, { code: "Escape", key: "Escape" });
    expect(marks()).toEqual([]);
    expect(preview()).toBeNull();
    expect(document.activeElement).toBe(terminalInput);
    release("one");
    expect(order()).toEqual(["one", "two", "three"]);
    expect(invoke).not.toHaveBeenCalled();
  });

  it("ends the drag when a dialog opens, and drops nothing under it", () => {
    render(<CommandBar uiScale={100} />);
    button("one").focus();
    pickUp("one");
    moveOver("three", 0.75);
    expect(marks()).toEqual(["after three"]);

    // A dialog appears while the pointer is held still, as a host-key
    // prompt could.
    render(<ConfirmDialog message="prompt" onConfirm={vi.fn()} onCancel={vi.fn()} />);
    expect(marks()).toEqual([]);
    expect(preview()).toBeNull();
    // The focus is not moved under a dialog, at its opening or at the
    // release.
    expect(document.activeElement).toBe(button("one"));

    release("one");
    expect(order()).toEqual(["one", "two", "three"]);
    expect(invoke).not.toHaveBeenCalled();
    expect(document.activeElement).toBe(button("one"));
  });

  it("picks nothing up under a dialog", () => {
    render(<CommandBar uiScale={100} />);
    fireEvent.pointerDown(button("one"), {
      isPrimary: true,
      button: 0,
      clientX: 120,
      clientY: 110,
    });
    // The dialog opens between the press and the first move.
    render(<ConfirmDialog message="prompt" onConfirm={vi.fn()} onCancel={vi.fn()} />);
    fireEvent.pointerMove(document, { isPrimary: true, clientX: 120, clientY: 140 });
    expect(preview()).toBeNull();
    moveOver("three", 0.75);
    expect(marks()).toEqual([]);
    fireEvent.pointerUp(document, { isPrimary: true, clientX: 0, clientY: TOP });
    expect(order()).toEqual(["one", "two", "three"]);
  });
});
