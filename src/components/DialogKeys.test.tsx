/**
 * DialogKeys tests.
 *
 * The contract that matters: a dialog owns Escape/Enter only while it is
 * actually on screen. Anything that claims a key while rendering nothing
 * takes that key away from the terminal application-wide — Escape is a real
 * keystroke there (vim's mode switch, `less`, readline vi-mode), and it has
 * no visible symptom in the UI, so these tests guard the invariant directly.
 */
import { describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen } from "@testing-library/react";
import ReactDOM from "react-dom";
import { DialogKeys, dialogOpen } from "./DialogKeys";
import { UpdateDialog } from "./UpdateDialog";

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));
vi.mock("../i18n", () => ({
  default: { t: (key: string) => key, changeLanguage: vi.fn() },
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(() => Promise.resolve(vi.fn())),
}));
// No update available: UpdateDialog settles in the "idle" phase and renders
// null, which is the state that used to swallow Escape for the whole app.
vi.mock("@tauri-apps/plugin-updater", () => ({ check: vi.fn(() => Promise.resolve(null)) }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: vi.fn(() => Promise.resolve()) }));

/**
 * Stands in for the terminal: a focusable element below the document-level
 * capture listener. If a claim swallows the key, this never sees it.
 */
function TerminalStub({ onKey }: { onKey: (e: KeyboardEvent) => void }): React.JSX.Element {
  return (
    <textarea
      data-testid="terminal"
      ref={(el) => {
        el?.addEventListener("keydown", onKey);
      }}
    />
  );
}

function pressEscape(): void {
  fireEvent.keyDown(screen.getByTestId("terminal"), { key: "Escape" });
}

describe("DialogKeys escape ownership", () => {
  it("lets Escape through to the terminal when nothing claims it", () => {
    const onKey = vi.fn();
    render(<TerminalStub onKey={onKey} />);

    pressEscape();

    expect(onKey).toHaveBeenCalledTimes(1);
  });

  it("takes Escape while mounted and calls the handler", () => {
    const onKey = vi.fn();
    const onEscape = vi.fn();
    render(
      <>
        <DialogKeys onEscape={onEscape} />
        <TerminalStub onKey={onKey} />
      </>,
    );

    pressEscape();

    expect(onEscape).toHaveBeenCalledTimes(1);
    expect(onKey).not.toHaveBeenCalled();
  });

  it("releases Escape back to the terminal once unmounted", () => {
    const onKey = vi.fn();
    const onEscape = vi.fn();
    const { rerender } = render(
      <>
        <DialogKeys onEscape={onEscape} />
        <TerminalStub onKey={onKey} />
      </>,
    );

    rerender(<TerminalStub onKey={onKey} />);
    pressEscape();

    expect(onEscape).not.toHaveBeenCalled();
    expect(onKey).toHaveBeenCalledTimes(1);
  });

  it("gives Escape to the last dialog in document order", () => {
    const first = vi.fn();
    const second = vi.fn();
    const onKey = vi.fn();
    render(
      <>
        <DialogKeys onEscape={first} />
        <DialogKeys onEscape={second} />
        <TerminalStub onKey={onKey} />
      </>,
    );

    pressEscape();

    expect(second).toHaveBeenCalledTimes(1);
    expect(first).not.toHaveBeenCalled();
  });

  it("gives the key to the dialog painted on top, not the one mounted last", () => {
    // The real scenario: a portaled dialog (quit confirmation) is open, then
    // an asynchronous dialog — a host-key prompt — mounts inline behind it.
    // Every overlay shares z-index, so the portal is what the user sees; if
    // the later-mounted inline dialog took Enter/Escape, the user would be
    // answering a prompt that is not on screen.
    const portaled = vi.fn();
    const inlineBehind = vi.fn();
    function Scenario({ showInline }: { showInline: boolean }): React.JSX.Element {
      return (
        <div>
          {ReactDOM.createPortal(<DialogKeys onEscape={portaled} />, document.body)}
          {showInline && <DialogKeys onEscape={inlineBehind} />}
          <TerminalStub onKey={vi.fn()} />
        </div>
      );
    }

    const { rerender } = render(<Scenario showInline={false} />);
    rerender(<Scenario showInline={true} />);
    pressEscape();

    expect(portaled).toHaveBeenCalledTimes(1);
    expect(inlineBehind).not.toHaveBeenCalled();
  });

  it("gives the key to the painted-on-top dialog in the other mount order too", () => {
    // Mirror of the case above: the inline dialog is already open when the
    // portaled one appears over it. Arbitration is time-invariant, so this
    // is a trip-wire against a future refactor reintroducing mount order.
    const portaled = vi.fn();
    const inlineBehind = vi.fn();
    function Scenario({ showPortal }: { showPortal: boolean }): React.JSX.Element {
      return (
        <div>
          {showPortal && ReactDOM.createPortal(<DialogKeys onEscape={portaled} />, document.body)}
          <DialogKeys onEscape={inlineBehind} />
          <TerminalStub onKey={vi.fn()} />
        </div>
      );
    }

    const { rerender } = render(<Scenario showPortal={false} />);
    rerender(<Scenario showPortal={true} />);
    pressEscape();

    expect(portaled).toHaveBeenCalledTimes(1);
    expect(inlineBehind).not.toHaveBeenCalled();
  });

  it("falls back to the dialog below when the topmost one closes", () => {
    const below = vi.fn();
    const above = vi.fn();
    const onKey = vi.fn();
    function Scenario({ showAbove }: { showAbove: boolean }): React.JSX.Element {
      return (
        <>
          <DialogKeys onEscape={below} />
          {showAbove && <DialogKeys onEscape={above} />}
          <TerminalStub onKey={onKey} />
        </>
      );
    }

    const { rerender } = render(<Scenario showAbove={true} />);
    rerender(<Scenario showAbove={false} />);
    pressEscape();

    expect(below).toHaveBeenCalledTimes(1);
    expect(above).not.toHaveBeenCalled();
    expect(onKey).not.toHaveBeenCalled();
  });

  it("does not claim a key it was given no handler for", () => {
    const onKey = vi.fn();
    render(
      <>
        <DialogKeys onEnter={vi.fn()} />
        <TerminalStub onKey={onKey} />
      </>,
    );

    pressEscape();

    expect(onKey).toHaveBeenCalledTimes(1);
  });
});

describe("DialogKeys enter ownership", () => {
  it("takes Enter while mounted", () => {
    const onEnter = vi.fn();
    render(
      <>
        <DialogKeys onEnter={onEnter} />
        <input data-testid="field" />
      </>,
    );

    fireEvent.keyDown(screen.getByTestId("field"), { key: "Enter" });

    expect(onEnter).toHaveBeenCalledTimes(1);
  });

  it("leaves Enter alone inside a textarea, where it inserts a newline", () => {
    const onEnter = vi.fn();
    const onKey = vi.fn();
    render(
      <>
        <DialogKeys onEnter={onEnter} />
        <TerminalStub onKey={onKey} />
      </>,
    );

    fireEvent.keyDown(screen.getByTestId("terminal"), { key: "Enter" });

    expect(onEnter).not.toHaveBeenCalled();
    expect(onKey).toHaveBeenCalledTimes(1);
  });

  it("gives a dialog no Enter that a dialog without a claim on Enter lies over", () => {
    // A host-key prompt, which accepts on Enter, under a dialog with a form
    // of its own, as the keyboard-interactive prompt is: the Enter typed
    // into that form is not the hidden prompt's.
    const accept = vi.fn();
    const onKey = vi.fn();
    render(
      <>
        <DialogKeys onEscape={vi.fn()} onEnter={accept} />
        <div>
          <DialogKeys onEscape={vi.fn()} />
          <input
            data-testid="field"
            ref={(el) => {
              el?.addEventListener("keydown", onKey);
            }}
          />
        </div>
      </>,
    );

    fireEvent.keyDown(screen.getByTestId("field"), { key: "Enter" });

    expect(accept).not.toHaveBeenCalled();
    // The key is left alone, for the form to take.
    expect(onKey).toHaveBeenCalledTimes(1);
    expect((onKey.mock.calls[0][0] as KeyboardEvent).defaultPrevented).toBe(false);
  });

  it("gives a dialog no key that a dialog claiming neither key lies over", () => {
    const below = vi.fn();
    const onKey = vi.fn();
    function Scenario({ covered }: { covered: boolean }): React.JSX.Element {
      return (
        <>
          <DialogKeys onEscape={below} onEnter={below} />
          {covered && ReactDOM.createPortal(<DialogKeys />, document.body)}
          <TerminalStub onKey={onKey} />
          <input data-testid="field" />
        </>
      );
    }

    const { rerender } = render(<Scenario covered={true} />);
    pressEscape();
    fireEvent.keyDown(screen.getByTestId("field"), { key: "Enter" });
    expect(below).not.toHaveBeenCalled();
    expect(onKey).toHaveBeenCalledTimes(1);

    // With the dialog on top gone, the keys are the lower one's again.
    rerender(<Scenario covered={false} />);
    pressEscape();
    fireEvent.keyDown(screen.getByTestId("field"), { key: "Enter" });
    expect(below).toHaveBeenCalledTimes(2);
    expect(onKey).toHaveBeenCalledTimes(1);
  });

  it("takes a held Enter for no second Enter", () => {
    // Enter confirms a dialog, which closes, and the key is still down: its
    // repeats must not accept what the dialog under it asks.
    const accept = vi.fn();
    const onKey = vi.fn();
    render(
      <>
        <DialogKeys onEnter={accept} />
        <input
          data-testid="field"
          ref={(el) => {
            el?.addEventListener("keydown", onKey);
          }}
        />
      </>,
    );

    fireEvent.keyDown(screen.getByTestId("field"), { key: "Enter", repeat: true });
    expect(accept).not.toHaveBeenCalled();
    // Nor does it reach a focused button, which it would press.
    expect(onKey).not.toHaveBeenCalled();

    fireEvent.keyDown(screen.getByTestId("field"), { key: "Enter" });
    expect(accept).toHaveBeenCalledTimes(1);
  });

  it("makes the box of a dialog inert while another dialog lies over it", () => {
    function Dialog({
      name,
      children,
    }: {
      name: string;
      children?: React.ReactNode;
    }): React.JSX.Element {
      return (
        <div className="dialog-overlay">
          <DialogKeys onEscape={vi.fn()} />
          <div className="dialog" data-testid={name}>
            <button type="button">Accept</button>
          </div>
          {children}
        </div>
      );
    }
    function Scenario({ top, nested }: { top: boolean; nested: boolean }): React.JSX.Element {
      return (
        <>
          <Dialog name="host-key" />
          <Dialog name="settings">{nested && <Dialog name="nested" />}</Dialog>
          {top && ReactDOM.createPortal(<Dialog name="update" />, document.body)}
        </>
      );
    }

    const { rerender } = render(<Scenario top={false} nested={false} />);
    // The later of two is on top.
    expect(screen.getByTestId("host-key")).toHaveAttribute("inert");
    expect(screen.getByTestId("settings")).not.toHaveAttribute("inert");

    // A dialog opened inside an overlay lies over that dialog's box alone.
    rerender(<Scenario top={false} nested={true} />);
    expect(screen.getByTestId("settings")).toHaveAttribute("inert");
    expect(screen.getByTestId("nested")).not.toHaveAttribute("inert");

    rerender(<Scenario top={true} nested={true} />);
    expect(screen.getByTestId("nested")).toHaveAttribute("inert");
    expect(screen.getByTestId("update")).not.toHaveAttribute("inert");

    rerender(<Scenario top={false} nested={false} />);
    expect(screen.getByTestId("host-key")).toHaveAttribute("inert");
    expect(screen.getByTestId("settings")).not.toHaveAttribute("inert");
  });

  it("leaves Enter alone while an IME is composing", () => {
    const onEnter = vi.fn();
    render(
      <>
        <DialogKeys onEnter={onEnter} />
        <input data-testid="field" />
      </>,
    );

    fireEvent.keyDown(screen.getByTestId("field"), { key: "Enter", isComposing: true });

    expect(onEnter).not.toHaveBeenCalled();
  });
});

describe("UpdateDialog key ownership", () => {
  it("does not hold Escape while idle at the app root", async () => {
    const onKey = vi.fn();
    render(
      <>
        <UpdateDialog />
        <TerminalStub onKey={onKey} />
      </>,
    );
    // Let the mount-time update check settle into the idle phase.
    await vi.waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });

    pressEscape();

    expect(onKey).toHaveBeenCalledTimes(1);
  });
});

const watched = new WeakSet<HTMLElement>();

/**
 * A window with a terminal, a field and a tab in it, and a dialog over it
 * that did not take the focus from any of them.
 */
function Window({
  dialog,
  seen,
  onEscape,
  onEnter,
  onLastKey,
}: {
  dialog: boolean;
  /** Called with the type of every event that reaches something in the window. */
  seen: (type: string, where: string) => void;
  onEscape?: () => void;
  onEnter?: () => void;
  /** What the dialog's last stop does on a key itself, as a select does on Tab. */
  onLastKey?: () => void;
}): React.JSX.Element {
  const watch =
    (where: string) =>
    (el: HTMLElement | null): void => {
      // Once for each element, however often the window renders.
      if (el === null || watched.has(el)) return;
      watched.add(el);
      for (const type of ["keydown", "keyup", "input", "paste", "compositionend", "contextmenu"]) {
        el.addEventListener(type, () => {
          seen(type, where);
        });
      }
    };
  return (
    <>
      <textarea data-testid="terminal" ref={watch("terminal")} />
      <input data-testid="search" ref={watch("search")} />
      <button type="button" data-testid="tab" ref={watch("tab")} />
      {dialog && (
        <div className="dialog-overlay">
          <DialogKeys onEscape={onEscape} onEnter={onEnter} />
          <div className="dialog">
            <p data-testid="text" tabIndex={-1} />
            <input type="hidden" />
            <input data-testid="first" ref={watch("first")} />
            <textarea data-testid="note" ref={watch("note")} />
            <button type="button" data-testid="middle" />
            <button type="button" disabled />
            <button type="button" data-testid="last" onKeyDown={onLastKey} />
            <button type="button" data-not-shown="" />
            <span data-testid="list" tabIndex={-1} />
          </div>
        </div>
      )}
    </>
  );
}

describe("the window behind a dialog", () => {
  it("gets no key that is typed at it, and every key without a dialog", () => {
    const seen = vi.fn();
    const { rerender } = render(<Window dialog={true} seen={seen} onEscape={vi.fn()} />);
    const terminal = screen.getByTestId("terminal");

    // fireEvent says false where the key was refused.
    expect(fireEvent.keyDown(terminal, { key: "a" })).toBe(false);
    expect(fireEvent.keyDown(terminal, { key: "c", ctrlKey: true })).toBe(false);
    expect(fireEvent.keyDown(screen.getByTestId("search"), { key: "x" })).toBe(false);
    fireEvent.keyUp(terminal, { key: "a" });
    expect(seen).not.toHaveBeenCalled();

    rerender(<Window dialog={false} seen={seen} />);
    expect(fireEvent.keyDown(terminal, { key: "a" })).toBe(true);
    fireEvent.keyUp(terminal, { key: "a" });
    expect(seen.mock.calls).toEqual([
      ["keydown", "terminal"],
      ["keyup", "terminal"],
    ]);
  });

  it("gets no text either: none pasted, put in or composed", () => {
    const seen = vi.fn();
    render(<Window dialog={true} seen={seen} onEscape={vi.fn()} />);
    const terminal = screen.getByTestId("terminal");

    fireEvent.paste(terminal);
    fireEvent.input(terminal, { target: { value: "rm -rf" } });
    fireEvent.compositionEnd(terminal, { data: "かな" });
    expect(seen).not.toHaveBeenCalled();

    // What is in the dialog takes all of it as before.
    const first = screen.getByTestId("first");
    expect(fireEvent.keyDown(first, { key: "a" })).toBe(true);
    fireEvent.paste(first);
    fireEvent.input(first, { target: { value: "name" } });
    expect(seen.mock.calls).toEqual([
      ["keydown", "first"],
      ["paste", "first"],
      ["input", "first"],
    ]);
  });

  it("still answers the dialog with Escape and Enter, without a click into it", () => {
    const seen = vi.fn();
    const onEscape = vi.fn();
    const onEnter = vi.fn();
    render(<Window dialog={true} seen={seen} onEscape={onEscape} onEnter={onEnter} />);

    // The focus is where a double click on a session left it.
    fireEvent.keyDown(screen.getByTestId("tab"), { key: "Enter" });
    fireEvent.keyDown(screen.getByTestId("tab"), { key: "Escape" });
    expect(onEnter).toHaveBeenCalledTimes(1);
    expect(onEscape).toHaveBeenCalledTimes(1);
    expect(seen).not.toHaveBeenCalled();
  });

  it("does not take the Enter of a line typed into a terminal for an answer", () => {
    const seen = vi.fn();
    const onEnter = vi.fn();
    render(<Window dialog={true} seen={seen} onEscape={vi.fn()} onEnter={onEnter} />);

    expect(fireEvent.keyDown(screen.getByTestId("terminal"), { key: "Enter" })).toBe(false);
    expect(onEnter).not.toHaveBeenCalled();
    // Nor does the terminal get it, and with it the remote host.
    expect(seen).not.toHaveBeenCalled();
  });

  it("gets no Escape that the dialog on top does not claim", () => {
    const seen = vi.fn();
    render(<Window dialog={true} seen={seen} />);

    expect(fireEvent.keyDown(screen.getByTestId("terminal"), { key: "Escape" })).toBe(false);
    expect(fireEvent.keyDown(screen.getByTestId("tab"), { key: "Enter" })).toBe(false);
    expect(seen).not.toHaveBeenCalled();
  });

  it("is not gone back to with Tab, which stays in the dialog", () => {
    render(<Window dialog={true} seen={vi.fn()} onEscape={vi.fn()} />);
    const first = screen.getByTestId("first");
    const middle = screen.getByTestId("middle");
    const last = screen.getByTestId("last");

    // From behind the dialog into it, at either end.
    screen.getByTestId("terminal").focus();
    expect(fireEvent.keyDown(screen.getByTestId("terminal"), { key: "Tab" })).toBe(false);
    expect(first).toHaveFocus();
    screen.getByTestId("tab").focus();
    fireEvent.keyDown(screen.getByTestId("tab"), { key: "Tab", shiftKey: true });
    expect(last).toHaveFocus();

    // Around at the ends: the stop that takes no input is no end.
    expect(fireEvent.keyDown(last, { key: "Tab" })).toBe(false);
    expect(first).toHaveFocus();
    expect(fireEvent.keyDown(first, { key: "Tab", shiftKey: true })).toBe(false);
    expect(last).toHaveFocus();

    // Between two stops of the dialog the key is left alone.
    middle.focus();
    expect(fireEvent.keyDown(middle, { key: "Tab" })).toBe(true);
    expect(fireEvent.keyDown(middle, { key: "Tab", shiftKey: true })).toBe(true);
    expect(fireEvent.keyDown(first, { key: "Tab" })).toBe(true);
    expect(middle).toHaveFocus();
  });

  it("is not what a key is for while nothing has the focus", () => {
    // A click on text in the dialog, to select a fingerprint, leaves the
    // focus on nothing: the keys that copy it or scroll the dialog go on.
    const onEscape = vi.fn();
    const onEnter = vi.fn();
    render(<Window dialog={true} seen={vi.fn()} onEscape={onEscape} onEnter={onEnter} />);

    expect(fireEvent.keyDown(document.body, { key: "c", ctrlKey: true })).toBe(true);
    expect(fireEvent.keyDown(document.body, { key: "PageDown" })).toBe(true);
    // Escape and Enter answer the dialog from there as from anywhere.
    fireEvent.keyDown(document.body, { key: "Escape" });
    fireEvent.keyDown(document.body, { key: "Enter" });
    expect(onEscape).toHaveBeenCalledTimes(1);
    expect(onEnter).toHaveBeenCalledTimes(1);
    // Tab does not start at the top of the page, which is behind the dialog.
    expect(fireEvent.keyDown(document.body, { key: "Tab" })).toBe(false);
    expect(screen.getByTestId("first")).toHaveFocus();
  });

  it("leaves a line's Enter to a field of the dialog that takes lines", () => {
    const seen = vi.fn();
    const onEnter = vi.fn();
    render(<Window dialog={true} seen={seen} onEscape={vi.fn()} onEnter={onEnter} />);

    expect(fireEvent.keyDown(screen.getByTestId("note"), { key: "Enter" })).toBe(true);
    expect(onEnter).not.toHaveBeenCalled();
    expect(seen.mock.calls).toEqual([["keydown", "note"]]);
  });

  it("keeps Tab in the dialog from something in it that is no stop itself", () => {
    render(<Window dialog={true} seen={vi.fn()} onEscape={vi.fn()} />);
    const text = screen.getByTestId("text");
    const list = screen.getByTestId("list");

    // After the last stop: the webview would go on behind the dialog.
    list.focus();
    expect(fireEvent.keyDown(list, { key: "Tab" })).toBe(false);
    expect(screen.getByTestId("first")).toHaveFocus();
    // The button that is not shown is no stop to go back to.
    list.focus();
    fireEvent.keyDown(list, { key: "Tab", shiftKey: true });
    expect(screen.getByTestId("last")).toHaveFocus();

    // Before the first stop, the other way round.
    text.focus();
    fireEvent.keyDown(text, { key: "Tab", shiftKey: true });
    expect(screen.getByTestId("last")).toHaveFocus();
    text.focus();
    fireEvent.keyDown(text, { key: "Tab" });
    expect(screen.getByTestId("first")).toHaveFocus();
  });

  it("lets a stop of the dialog see the Tab that takes the focus around", () => {
    // A select closes its open list on Tab.
    const onLastKey = vi.fn();
    render(<Window dialog={true} seen={vi.fn()} onEscape={vi.fn()} onLastKey={onLastKey} />);

    fireEvent.keyDown(screen.getByTestId("last"), { key: "Tab" });
    expect(screen.getByTestId("first")).toHaveFocus();
    expect(onLastKey).toHaveBeenCalledTimes(1);
  });

  it("includes a dialog that another dialog lies over", () => {
    const seen = vi.fn();
    render(
      <>
        <Window dialog={true} seen={seen} onEscape={vi.fn()} />
        <div className="dialog-overlay">
          <DialogKeys />
          <div className="dialog">
            <button type="button" data-testid="top" />
          </div>
        </div>
      </>,
    );

    // The lower dialog's field is behind the one on top.
    expect(fireEvent.keyDown(screen.getByTestId("first"), { key: "a" })).toBe(false);
    expect(seen).not.toHaveBeenCalled();
    fireEvent.keyDown(screen.getByTestId("first"), { key: "Tab" });
    expect(screen.getByTestId("top")).toHaveFocus();
  });

  it("says whether a dialog is open", () => {
    const { rerender } = render(<Window dialog={false} seen={vi.fn()} />);
    expect(dialogOpen()).toBe(false);
    rerender(<Window dialog={true} seen={vi.fn()} />);
    expect(dialogOpen()).toBe(true);
    rerender(<Window dialog={false} seen={vi.fn()} />);
    expect(dialogOpen()).toBe(false);
  });
});
