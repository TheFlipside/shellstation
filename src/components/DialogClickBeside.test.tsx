/**
 * A click beside a dialog's box closes nothing. The two managers, Quick
 * Connect and the paste confirmation used to close on one, and a click is
 * what a browser makes of a press in a field and a release beside the box,
 * as when text is selected with the pointer: the dialog closed, and what was
 * typed into it was gone.
 *
 * The overlay of a dialog has no handler for the pointer, which the last
 * tests read from the source of the components in this directory. That
 * holds a dialog that is yet to be written to the rule as far as its source
 * is written as the dialogs' is now; `overlayTags` says what is not seen.
 */
import React from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen } from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import i18n from "../i18n";
import { CredentialManager } from "./CredentialManager";
import { LoginSequenceManager } from "./LoginSequenceManager";
import { PasteConfirmDialog } from "./PasteConfirmDialog";
import { QuickConnect } from "./QuickConnect";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

beforeEach(async () => {
  await i18n.changeLanguage("en");
  // The managers list what is there when they open: nothing.
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockResolvedValue([]);
});

function overlay(): HTMLElement {
  const element = document.querySelector<HTMLElement>(".dialog-overlay");
  if (!element) throw new Error("no dialog");
  return element;
}

/** A press and a release beside the box, and the click they make. */
function clickBeside(): void {
  fireEvent.mouseDown(overlay());
  fireEvent.mouseUp(overlay());
  fireEvent.click(overlay());
}

/**
 * A press in a field and a release beside the box. The click goes to what
 * holds both, which is the overlay.
 */
function dragOutOf(field: HTMLElement): void {
  fireEvent.mouseDown(field);
  fireEvent.mouseUp(overlay());
  fireEvent.click(overlay());
}

async function renderOpen(dialog: React.JSX.Element): Promise<void> {
  render(dialog);
  // Let the list a manager asks for at its start arrive.
  await act(async () => {
    await Promise.resolve();
  });
}

function field(id: string): HTMLInputElement {
  const element = document.getElementById(id);
  if (!(element instanceof HTMLInputElement)) throw new Error(`no field ${id}`);
  return element;
}

/** The four dialogs that closed on a click beside them, and the button that closes each. */
const DIALOGS = [
  [
    "the login sequence manager",
    (close: () => void) => <LoginSequenceManager onClose={close} />,
    "settings.close",
  ],
  [
    "the credential manager",
    (close: () => void) => <CredentialManager onClose={close} />,
    "settings.close",
  ],
  [
    "Quick Connect",
    (close: () => void) => <QuickConnect onConnect={vi.fn()} onCancel={close} />,
    "dialog.cancel",
  ],
  [
    "the paste confirmation",
    (close: () => void) => (
      <PasteConfirmDialog content={"one\ntwo"} onConfirm={vi.fn()} onCancel={close} />
    ),
    "dialog.cancel",
  ],
] as const;

/**
 * The source of every component, by its path from here. Tests are left out.
 * The components are all in this directory, which has none below it.
 */
const SOURCES: Record<string, string> = Object.fromEntries(
  Object.entries(
    import.meta.glob<string>("./*.tsx", { query: "?raw", import: "default", eager: true }),
  ).filter(([path]) => !path.endsWith(".test.tsx")),
);

/**
 * What an overlay's tag must not have: a handler for a click, a press or a
 * release, as React names them, or a spread, which could bring one.
 */
const TAKES_POINTER =
  /\bon(?:Click|DoubleClick|AuxClick|MouseDown|MouseUp|PointerDown|PointerUp|TouchStart|TouchEnd)(?:Capture)?\s*=|\{\s*\.\.\./;

/** An overlay's class, written out in the tag as text or as a template. */
const OVERLAY_CLASS = /className=\s*(?:"[^"]*|'[^']*|\{\s*`[^`]*)\bdialog-overlay\b/;

/** The start of a line that is a comment, or goes on in one. */
const COMMENT_LINE = /^\s*(?:\/\/|\/?\*)/;

/**
 * How often a component's source renders `<DialogKeys>`. Every dialog has
 * one, as a child of its overlay (see `DialogKeys`), so a file has as many
 * of them as it has overlays.
 */
function dialogMarkers(source: string): number {
  return (
    source
      .split("\n")
      .filter((line) => !COMMENT_LINE.test(line))
      .join("\n")
      .split(/<DialogKeys\b/).length - 1
  );
}

/**
 * Where the string or the comment that begins at `at` ends, or `at` where
 * none begins.
 */
function pastText(source: string, at: number): number {
  const char = source[at];
  if (char === '"' || char === "'" || char === "`") {
    let end = at + 1;
    while (end < source.length && source[end] !== char) end += source[end] === "\\" ? 2 : 1;
    return end + 1;
  }
  if (source.startsWith("//", at)) {
    const end = source.indexOf("\n", at);
    return end === -1 ? source.length : end;
  }
  if (source.startsWith("/*", at)) {
    const end = source.indexOf("*/", at + 2);
    return end === -1 ? source.length : end + 2;
  }
  return at;
}

/**
 * The opening tags of the overlays in a component's source, each from its
 * `<` to the `>` that ends it. A `>` or a brace ends and opens nothing
 * inside braces, a string or a comment, and a tag that a comment shows as
 * an example is none.
 *
 * It goes by the text. An overlay whose class is not written in its tag as
 * a string or as a template is not seen: one named by a string in braces, a
 * constant, a condition or a call, or made by a component of its own. The
 * test below holds the overlays that are seen against the dialogs' markers
 * (see `dialogMarkers`), file by file, so that an overlay written another
 * way is one too few there, if it has its marker as every dialog does. What
 * it cannot tell is a handler that is added to the element by a ref or a
 * listener, or one on an element around the overlay or filling it. And a
 * tag is not read as code: a regular expression or a template inside a
 * template in it can end it early. No overlay has any of these.
 */
function overlayTags(source: string): string[] {
  const tags: string[] = [];
  for (const start of source.matchAll(/<[A-Za-z][\w.]*(?=\s)/g)) {
    const line = source.slice(source.lastIndexOf("\n", start.index) + 1, start.index);
    if (COMMENT_LINE.test(line)) continue;
    let depth = 0;
    let end = start.index;
    while (end < source.length) {
      const past = pastText(source, end);
      if (past > end) {
        end = past;
        continue;
      }
      const char = source[end];
      if (char === "{") depth++;
      else if (char === "}") depth--;
      else if (char === ">" && depth === 0) break;
      end++;
    }
    const tag = source.slice(start.index, end + 1);
    if (OVERLAY_CLASS.test(tag)) tags.push(tag);
  }
  return tags;
}

describe("a click beside a dialog", () => {
  it.each(DIALOGS)("does not close %s", async (_name, dialog) => {
    const close = vi.fn();
    await renderOpen(dialog(close));
    clickBeside();
    expect(close).not.toHaveBeenCalled();
  });

  it.each(DIALOGS)("still closes %s by its button and by Escape", async (_name, dialog, button) => {
    const close = vi.fn();
    await renderOpen(dialog(close));
    fireEvent.click(screen.getByRole("button", { name: i18n.t(button) }));
    expect(close).toHaveBeenCalledTimes(1);

    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(close).toHaveBeenCalledTimes(2);
  });

  it("keeps a login sequence that is being written, and its manager", async () => {
    const onClose = vi.fn();
    await renderOpen(<LoginSequenceManager onClose={onClose} />);
    fireEvent.click(screen.getByRole("button", { name: i18n.t("loginSequences.newSequence") }));
    fireEvent.change(field("lsm-name"), { target: { value: "cisco enable" } });
    fireEvent.change(field("lsm-pattern-0"), { target: { value: "Password:" } });

    dragOutOf(field("lsm-pattern-0"));
    clickBeside();

    expect(onClose).not.toHaveBeenCalled();
    expect(field("lsm-name").value).toBe("cisco enable");
    expect(field("lsm-pattern-0").value).toBe("Password:");
  });

  it("keeps a credential profile that is being written, and its manager", async () => {
    const onClose = vi.fn();
    await renderOpen(<CredentialManager onClose={onClose} />);
    fireEvent.click(screen.getByRole("button", { name: i18n.t("credentialProfiles.newProfile") }));
    fireEvent.change(field("cm-name"), { target: { value: "lab admin" } });
    fireEvent.change(field("cm-password"), { target: { value: "hunter2" } });

    dragOutOf(field("cm-password"));
    clickBeside();

    expect(onClose).not.toHaveBeenCalled();
    expect(field("cm-name").value).toBe("lab admin");
    expect(field("cm-password").value).toBe("hunter2");
  });

  it("keeps what was typed into Quick Connect", async () => {
    const onCancel = vi.fn();
    await renderOpen(<QuickConnect onConnect={vi.fn()} onCancel={onCancel} />);
    fireEvent.change(field("qc-host"), { target: { value: "switch.example" } });

    dragOutOf(field("qc-host"));

    expect(onCancel).not.toHaveBeenCalled();
    expect(field("qc-host").value).toBe("switch.example");
  });

  it("leaves the form of a manager on Escape, and the manager on the next", async () => {
    const onClose = vi.fn();
    await renderOpen(<CredentialManager onClose={onClose} />);
    fireEvent.click(screen.getByRole("button", { name: i18n.t("credentialProfiles.newProfile") }));

    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(document.getElementById("cm-name")).toBeNull();
    expect(onClose).not.toHaveBeenCalled();

    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

describe("the overlay of a dialog", () => {
  /** A dialog with this as its overlay's tag, and a box that takes clicks. */
  const dialog = (tag: string): string =>
    `<>\n${tag}\n<div className="dialog" onClick={stay} />\n</div>\n</>`;

  it("is told from the source, whatever stands in its tag", () => {
    // What the test below goes by, tried on what it must find.
    const taking = [
      '<div className="dialog-overlay" onClick={onClose} role="presentation">',
      // The handler before the class, with a `>` and braces of its own.
      '<div onMouseDown={(e) => { if (e.target === e.currentTarget) onClose(); }} className="dialog-overlay">',
      "<div className={`dialog-overlay ${extra}`} style={{ zIndex: 1 }} onPointerDown={close}>",
      // A `>` and a brace in a string end and open nothing.
      '<div className="dialog-overlay" title="a > b" data-x={"}"} onTouchEnd={close}>',
      // Nor do they in a comment, and an apostrophe there begins no string.
      "<div\n  className={`\n    dialog-overlay\n  `}\n  // The box's > and {\n  onClickCapture={close}\n>",
      "<div className='dialog-overlay' /* > */ {...rest}>",
    ];
    for (const tag of taking) {
      expect(overlayTags(dialog(tag))).toEqual([tag]);
      expect(tag).toMatch(TAKES_POINTER);
    }

    // The box's own handlers are not the overlay's.
    const plain = '<div className="dialog-overlay dialog-overlay-messages" role="presentation">';
    expect(overlayTags(dialog(plain))).toEqual([plain]);
    expect(plain).not.toMatch(TAKES_POINTER);

    // A tag that a comment shows is no overlay.
    const shown = '<div className="dialog-overlay" onClick={onClose}>';
    expect(overlayTags(`/**\n * As in:\n *   ${shown}\n */\n// ${shown}\n`)).toEqual([]);
    // Nor is an element that only speaks of the class.
    expect(
      overlayTags('<button className="dialog-btn" onClick={() => find(".dialog-overlay")}>'),
    ).toEqual([]);
  });

  it("counts the dialogs of a source by their markers, not those a comment shows", () => {
    expect(dialogMarkers(dialog('<div className="dialog-overlay">\n<DialogKeys />'))).toBe(1);
    expect(
      dialogMarkers(
        " *   <DialogKeys onEscape={onCancel} />\n// <DialogKeys />\n<DialogKeysTest />",
      ),
    ).toBe(0);
    // An overlay the scan does not see is a marker without one.
    const unseen = dialog("<div className={OVERLAY} onClick={onClose}>\n<DialogKeys />");
    expect(overlayTags(unseen)).toEqual([]);
    expect(dialogMarkers(unseen)).toBe(1);
  });

  it("has no handler for a click, a press or a release, in any component", () => {
    const files = Object.entries(SOURCES).map(([path, source]) => ({
      path,
      tags: overlayTags(source),
      markers: dialogMarkers(source),
    }));
    // A file with a dialog whose overlay was not seen, or with an overlay
    // that is no dialog's.
    expect(
      files.filter((file) => file.tags.length !== file.markers).map((file) => file.path),
    ).toEqual([]);

    // Every dialog has one, far more than the four above: 23 in 22 files
    // when this was written. With none, nothing would have been read.
    const overlays = files.flatMap((file) => file.tags);
    expect(overlays.length).toBeGreaterThanOrEqual(20);
    expect(overlays.filter((tag) => TAKES_POINTER.test(tag))).toEqual([]);
  });
});
