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
import { DialogKeys } from "./DialogKeys";
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
