import React, { useEffect, useLayoutEffect, useRef, useState } from "react";

/**
 * Escape / Enter handling for modal surfaces.
 *
 * Render this *inside* a dialog's markup rather than calling a hook at the top
 * of the component:
 *
 *     return (
 *       <div className="dialog-overlay">
 *         <DialogKeys onEscape={onCancel} onEnter={onSubmit} />
 *         ...
 *
 * Why a component and not a hook: the keys are taken at `document` in the
 * capture phase, and propagation is stopped, so whoever holds the topmost
 * claim takes the key away from everything below — including the terminal,
 * where Escape is a meaningful keystroke (vim's mode switch, among others).
 * A hook called at the top of a component keeps that claim for as long as the
 * *component* is mounted, even across an early `return null`, which silently
 * breaks Escape application-wide. Claims made in JSX cannot outlive what the
 * component actually renders, so the failure mode is structurally impossible.
 * A dialog that forgets to render this simply does not dismiss on Escape —
 * visible immediately, and local to that dialog.
 *
 * When several dialogs are open, the key goes to the one the user can
 * actually see on top, and only to that one — standard modal behavior. To
 * make Escape do something else while a dialog is open, handle it inside that
 * dialog.
 *
 * "On top" is resolved by document order, not by mount order. Every
 * `.dialog-overlay` shares `z-index: 100` (App.css), so among overlays the
 * one appearing last in the document paints above the rest. Mount order is
 * not a usable proxy: dialogs portaled to `document.body` always paint above
 * dialogs rendered inline in the app tree, whenever they mounted. A dialog
 * that appears on its own schedule — `HostVerifyDialog` when a host key needs
 * verification, `KbdInteractiveDialog` on an auth prompt — can therefore
 * mount behind an already-open modal, and under mount-order arbitration would
 * take Enter from the dialog the user is actually looking at. For the host-key
 * prompt that means silently accepting an unseen key (TOFU) on a keystroke
 * meant for something else, so ownership follows what is painted on top.
 *
 * The dialog on top need not claim the key. `KbdInteractiveDialog` takes no
 * Enter of its own, which its form is for, and `UpdateDialog` takes none
 * either: a host-key prompt under one of them must not get the Enter typed
 * into the dialog the user sees. So a claim counts only while no dialog lies
 * over it, whatever that dialog claims, and the key is then left alone.
 *
 * For that the marker also says that a dialog is open at all, and where, so
 * every dialog renders this, also one that takes neither key, as a direct
 * child of its `.dialog-overlay`, beside the `.dialog` box. That box is made
 * inert while another dialog lies over it (see `DialogKeys`). The toasts go
 * by the markers as well (`useDialogOver`): a stack of them that a dialog
 * lies over takes no input and lets no toast's time run.
 *
 * A dialog is modal for the keyboard as it is for the pointer. It covers the
 * window, so no click gets past it, but it does not take the focus: what had
 * it when the dialog appeared still has it, a terminal for one. While a
 * dialog is open, what is typed at something outside the dialog on top
 * therefore reaches nothing: not the terminal and its remote host, not a
 * shortcut of the window behind. Escape and Enter typed there still go to
 * the dialog's claims, as before, so that a prompt can be answered without a
 * click into it, except an Enter typed into a terminal, which is no answer
 * to a prompt that appeared meanwhile. Tab goes into the dialog and stays in
 * it. The focus itself is left where it is: a prompt that took it from a
 * terminal would get the Enter that ends the line being typed there.
 */

/** One dialog's claim on a key: the handler plus where it sits in the DOM. */
interface Claim {
  callback: { current: (() => void) | undefined };
  anchor: { current: HTMLElement | null };
}

interface KeyChannel {
  claims: Claim[];
}

/** The markers of the dialogs that are open. */
const markers = new Set<HTMLElement>();
/** Called when a dialog opens or closes. */
const markerListeners = new Set<() => void>();

function markersChanged(): void {
  for (const listener of [...markerListeners]) listener();
}

/** Called when a dialog opens, and not when one closes. */
const openListeners = new Set<() => void>();
/** How many dialogs have opened so far. */
let opened = 0;

/**
 * Call `callback` whenever a dialog opens while the caller is mounted. A menu
 * and the list of a select close themselves by this: they are painted above
 * the dialogs, to lie over the dialog they were opened in, and would lie
 * over one that appears while they are open as well, as a host-key prompt
 * can at any time.
 */
export function useOnDialogOpen(callback: () => void): void {
  const callbackRef = useRef(callback);
  useLayoutEffect(() => {
    callbackRef.current = callback;
  }, [callback]);
  // What had opened when the caller was rendered. A dialog that appears in
  // the same commit as the caller has said so before the caller listens, if
  // it comes first in the tree.
  const openedBefore = useRef(opened);
  // A layout effect, as a dialog says that it is there in one.
  useLayoutEffect(() => {
    if (opened !== openedBefore.current) callbackRef.current();
    const listener = (): void => {
      callbackRef.current();
    };
    openListeners.add(listener);
    return () => {
      openListeners.delete(listener);
    };
  }, []);
}

/**
 * The overlay of the dialog on top, or null with no dialog open: the parent
 * of the marker that comes last in the document.
 */
function topDialog(): HTMLElement | null {
  let top: HTMLElement | null = null;
  for (const marker of markers) {
    if (!marker.isConnected) continue;
    if (
      top === null ||
      (top.compareDocumentPosition(marker) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0
    ) {
      top = marker;
    }
  }
  return top?.parentElement ?? null;
}

/**
 * Whether a dialog is open and the event is for something outside the one on
 * top. With the focus on nothing, a key is for the page as a whole, and that
 * is not behind the dialog: a click on text in the dialog, to select a
 * fingerprint, leaves the focus there, and the keys that copy the selection
 * or scroll the dialog must go on working. Nothing behind the dialog gets a
 * key that way, since nothing has the focus.
 */
function behindDialog(event: Event): boolean {
  const top = topDialog();
  if (top === null) return false;
  const target = event.target;
  if (!(target instanceof Element)) return false;
  if (target === document.body || target === document.documentElement) return false;
  return !top.contains(target);
}

/** Whether a dialog is open. What is behind one takes no shortcut either. */
export function dialogOpen(): boolean {
  return topDialog() !== null;
}

/**
 * The pointer does not get past a dialog, which covers the window. The
 * keyboard does: what had the focus when a dialog appeared still has it, and
 * the menu key or Shift+F10 there asks for its context menu, which would
 * open above the dialog. Taken at the document, before the app sees it.
 */
function handleContextMenu(event: Event): void {
  if (behindDialog(event)) event.stopPropagation();
}

/**
 * What else the keyboard does to what has the focus, besides a key going
 * down: the key coming up, text put in by a key, by a paste from the app's
 * menu or by an input method, and a cut. None of it is for what is behind a
 * dialog. An input method's text cannot be refused, so it is kept from
 * whoever would send it on, as a terminal does.
 */
const BEHIND_EVENTS = [
  "keypress",
  "keyup",
  "beforeinput",
  "input",
  "paste",
  "cut",
  "compositionstart",
  "compositionupdate",
  "compositionend",
] as const;

function handleBehind(event: Event): void {
  if (!behindDialog(event)) return;
  event.preventDefault();
  // Also from a listener that was put on the document after this one.
  event.stopImmediatePropagation();
}

/** Watch for what is meant for the window behind a dialog, while one is open. */
function watchBehind(): void {
  if (markers.size > 0) {
    document.addEventListener("keydown", handleKeyDown, true);
    document.addEventListener("contextmenu", handleContextMenu, true);
    for (const type of BEHIND_EVENTS) document.addEventListener(type, handleBehind, true);
  } else {
    document.removeEventListener("keydown", handleKeyDown, true);
    document.removeEventListener("contextmenu", handleContextMenu, true);
    for (const type of BEHIND_EVENTS) document.removeEventListener(type, handleBehind, true);
  }
}

/**
 * Whether a dialog lies over `element`: one whose marker comes after it in
 * the document. That is the order dialogs are painted in (see above). For a
 * dialog's own marker it says whether another dialog is on top of it. A
 * stack of toasts is painted under every dialog that follows it: the window's
 * stack comes before every dialog, and a dialog's own layer right after that
 * dialog's marker, before any dialog opened from it.
 */
function dialogOver(element: HTMLElement | null): boolean {
  if (element === null) return false;
  for (const marker of markers) {
    if (
      marker.isConnected &&
      (element.compareDocumentPosition(marker) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0
    ) {
      return true;
    }
  }
  return false;
}

/**
 * The callback of the visually topmost claim — the one whose anchor comes
 * last in document order. Claims whose anchor is missing or detached are
 * skipped, and no claim at all means the key is left alone: passing a
 * keystroke through is always safer than swallowing it. So is a topmost
 * claim that a dialog lies over, one that does not claim this key: the key
 * is that dialog's to leave alone, not the hidden one's to take.
 */
function topClaim(channel: KeyChannel): (() => void) | undefined {
  let top: Claim | undefined;
  for (const claim of channel.claims) {
    const node = claim.anchor.current;
    if (!node?.isConnected) continue;
    const topNode = top?.anchor.current ?? null;
    if (
      topNode === null ||
      (topNode.compareDocumentPosition(node) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0
    ) {
      top = claim;
    }
  }
  if (top === undefined || dialogOver(top.anchor.current)) return undefined;
  return top.callback.current;
}

const escapeChannel: KeyChannel = { claims: [] };
const enterChannel: KeyChannel = { claims: [] };

/** Give Escape to the claim on top, if there is one. Says whether it was taken. */
function takeEscape(event: KeyboardEvent): boolean {
  const claim = topClaim(escapeChannel);
  if (claim === undefined) return false;
  // No preventDefault: Escape has no default browser action worth blocking,
  // and intercepting it would interfere with native dialog / IME behavior.
  event.stopPropagation();
  claim();
  return true;
}

/** Give Enter to the claim on top, if there is one. Says whether it was taken. */
function takeEnter(event: KeyboardEvent): boolean {
  // Let an IME candidate selection through, and let multi-line inputs insert
  // their newline. In a terminal behind the dialog, Enter ends a line that
  // was being typed there, and is no answer to the dialog either.
  if (event.isComposing) return false;
  if (event.target instanceof HTMLTextAreaElement) return false;
  const claim = topClaim(enterChannel);
  if (claim === undefined) return false;
  event.preventDefault();
  event.stopPropagation();
  // A key held down is one Enter, for the dialog it was pressed in. When
  // that dialog closes on it, the repeats would go to the one under it: a
  // confirmation's Enter must not also accept a host key.
  if (event.repeat) return true;
  claim();
  return true;
}

const FOCUSABLE =
  "a[href], button, input, select, textarea, [tabindex], [contenteditable=''], [contenteditable='true']";

/** Whether the element is laid out and shown, as far as the webview can say. */
function isShown(element: HTMLElement): boolean {
  // An older webview has no checkVisibility: there, what has no box is not
  // shown, which leaves out what `visibility` hides.
  return typeof element.checkVisibility === "function"
    ? element.checkVisibility({ checkVisibilityCSS: true })
    : element.getClientRects().length > 0;
}

/** What Tab stops at in the dialog, in the order of the document. */
function tabStops(dialog: HTMLElement): HTMLElement[] {
  return Array.from(dialog.querySelectorAll<HTMLElement>(FOCUSABLE)).filter((element) => {
    if (element.tabIndex < 0 || element.matches(":disabled, input[type='hidden']")) return false;
    if (element.closest("[inert], [hidden]") !== null) return false;
    return isShown(element);
  });
}

/**
 * Keep Tab in the dialog on top: from its last stop to its first and back,
 * and from anything behind it into it. Says whether the key was taken.
 *
 * Between two stops of the dialog the key is left to the webview. From
 * something in the dialog that is no stop itself, such as its list of a
 * select or a piece of text that was clicked, the webview would go on to
 * whatever comes next in the document, the window behind the dialog if no
 * stop of the dialog does: so the next stop is found here.
 *
 * The key still reaches what it was typed at in the dialog, which may have
 * something to do on Tab, as a select has with its open list.
 */
function keepTab(event: KeyboardEvent, dialog: HTMLElement): boolean {
  if (event.ctrlKey || event.altKey || event.metaKey) return false;
  const stops = tabStops(dialog);
  const first = stops.length > 0 ? stops[0] : undefined;
  const last = stops.length > 0 ? stops[stops.length - 1] : undefined;
  const target = event.target;
  // With the focus on nothing, the webview would start at the top of the
  // page, which is behind the dialog.
  const from = target instanceof HTMLElement && dialog.contains(target) ? target : null;
  let to: HTMLElement | undefined;
  if (from === null) {
    to = event.shiftKey ? last : first;
  } else if (stops.includes(from)) {
    if (!event.shiftKey && from === last) {
      to = first;
    } else if (event.shiftKey && from === first) {
      to = last;
    } else {
      return false;
    }
  } else if (event.shiftKey) {
    const before = stops.filter(
      (stop) => (from.compareDocumentPosition(stop) & Node.DOCUMENT_POSITION_PRECEDING) !== 0,
    );
    to = before.length > 0 ? before[before.length - 1] : last;
  } else {
    to =
      stops.find(
        (stop) => (from.compareDocumentPosition(stop) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0,
      ) ?? first;
  }
  event.preventDefault();
  // What is behind the dialog gets no key; what is in it gets its own.
  if (from === null) event.stopImmediatePropagation();
  to?.focus();
  return true;
}

/**
 * Every key that goes down while a dialog is open: Escape and Enter for the
 * claims, Tab to stay in the dialog, and nothing for what is behind it.
 */
function handleKeyDown(event: KeyboardEvent): void {
  const dialog = topDialog();
  if (dialog === null) return;
  if (event.key === "Escape") {
    if (takeEscape(event)) return;
  } else if (event.key === "Enter") {
    if (takeEnter(event)) return;
  } else if (event.key === "Tab") {
    if (keepTab(event, dialog)) return;
  }
  handleBehind(event);
}

/**
 * Claim a key for as long as this element is mounted. The callback is held in
 * a ref that is refreshed on every render, so the stack only changes on mount
 * and unmount: StrictMode's double-invoke settles on exactly one claim, and an
 * inline arrow callback does not churn the stack on every render. The ref is
 * written in a layout effect rather than during render because a render can be
 * discarded before it commits, which would leave the ref pointing at an
 * abandoned closure.
 */
function useKeyClaim(
  channel: KeyChannel,
  callback: (() => void) | undefined,
  anchor: { current: HTMLElement | null },
): void {
  const callbackRef = useRef(callback);
  useLayoutEffect(() => {
    callbackRef.current = callback;
  }, [callback]);

  // One stable claim object per mounted element, so the stack only changes on
  // mount and unmount.
  const claimRef = useRef<Claim | null>(null);
  claimRef.current ??= { callback: callbackRef, anchor };

  const claimed = callback !== undefined;
  useEffect(() => {
    const claim = claimRef.current;
    if (!claimed || claim === null) return undefined;
    channel.claims.push(claim);
    return () => {
      const idx = channel.claims.lastIndexOf(claim);
      if (idx !== -1) channel.claims.splice(idx, 1);
    };
  }, [channel, claimed]);
}

/**
 * Whether a dialog lies over the element, kept up to date as dialogs open
 * and close. The element is there for as long as the caller is mounted.
 */
export function useDialogOver(element: { current: HTMLElement | null }): boolean {
  const [over, setOver] = useState(false);
  // A layout effect, as the markers are set in one: what is painted first
  // already has the answer.
  useLayoutEffect(() => {
    const check = (): void => {
      setOver(dialogOver(element.current));
    };
    check();
    markerListeners.add(check);
    return () => {
      markerListeners.delete(check);
    };
  }, [element]);
  return over;
}

interface DialogKeysProps {
  /** Called on Escape while this is the topmost dialog. */
  onEscape?: () => void;
  /** Called on Enter while this is the topmost dialog, outside textareas. */
  onEnter?: () => void;
}

/**
 * Renders a hidden marker inside the dialog; see the module comment for why
 * this is a component and not a hook. The marker is the anchor that locates
 * this dialog in the document, which is how "topmost" is decided — it carries
 * no content and is hidden from assistive technology.
 */
export function DialogKeys({ onEscape, onEnter }: DialogKeysProps): React.JSX.Element {
  const anchor = useRef<HTMLSpanElement | null>(null);
  useKeyClaim(escapeChannel, onEscape, anchor);
  useKeyClaim(enterChannel, onEnter, anchor);
  useLayoutEffect(() => {
    const marker = anchor.current;
    if (marker === null) return undefined;
    markers.add(marker);
    watchBehind();
    markersChanged();
    opened += 1;
    for (const listener of [...openListeners]) listener();
    return () => {
      markers.delete(marker);
      watchBehind();
      markersChanged();
    };
  }, []);

  // A dialog that another lies over takes no input: its box is inert. No
  // dialog keeps the focus to itself, so a button of this one can still have
  // it when another dialog appears on top, and Enter or Space typed there,
  // which no claim is asked about, would press it unseen: Accept, of a host
  // key prompt. The box is the `.dialog` beside the marker. What else the
  // overlay holds stays as it is: a dialog opened inside it, which is the
  // one on top, and the dialog's own toasts, which see to themselves.
  //
  // After every render, since the dialog may have rendered another box.
  const covered = useDialogOver(anchor);
  useLayoutEffect(() => {
    const overlay = anchor.current?.parentElement;
    if (!overlay) return;
    for (const child of overlay.children) {
      if (child.classList.contains("dialog")) child.toggleAttribute("inert", covered);
    }
  });

  return <span ref={anchor} hidden data-dialog-keys="" />;
}
