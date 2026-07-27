import React, { useEffect, useLayoutEffect, useRef } from "react";

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
 * Why a component and not a hook: these handlers are registered on `document`
 * in the capture phase and stop propagation, so whoever holds the topmost
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
 */

/** One dialog's claim on a key: the handler plus where it sits in the DOM. */
interface Claim {
  callback: { current: (() => void) | undefined };
  anchor: { current: HTMLElement | null };
}

interface KeyChannel {
  claims: Claim[];
  handler: (event: KeyboardEvent) => void;
  attached: boolean;
}

/**
 * The callback of the visually topmost claim — the one whose anchor comes
 * last in document order. Claims whose anchor is missing or detached are
 * skipped, and no claim at all means the key is left alone: passing a
 * keystroke through is always safer than swallowing it.
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
  return top?.callback.current;
}

function handleEscapeKeyDown(event: KeyboardEvent): void {
  if (event.key !== "Escape") return;
  const claim = topClaim(escapeChannel);
  if (claim === undefined) return;
  // No preventDefault: Escape has no default browser action worth blocking,
  // and intercepting it would interfere with native dialog / IME behavior.
  event.stopPropagation();
  claim();
}

function handleEnterKeyDown(event: KeyboardEvent): void {
  if (event.key !== "Enter") return;
  // Let an IME candidate selection through, and let multi-line inputs insert
  // their newline.
  if (event.isComposing) return;
  if (event.target instanceof HTMLTextAreaElement) return;
  const claim = topClaim(enterChannel);
  if (claim === undefined) return;
  event.preventDefault();
  event.stopPropagation();
  claim();
}

const escapeChannel: KeyChannel = {
  claims: [],
  handler: handleEscapeKeyDown,
  attached: false,
};

const enterChannel: KeyChannel = {
  claims: [],
  handler: handleEnterKeyDown,
  attached: false,
};

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
    if (!channel.attached) {
      document.addEventListener("keydown", channel.handler, true);
      channel.attached = true;
    }
    return () => {
      const idx = channel.claims.lastIndexOf(claim);
      if (idx !== -1) channel.claims.splice(idx, 1);
      if (channel.claims.length === 0) {
        document.removeEventListener("keydown", channel.handler, true);
        channel.attached = false;
      }
    };
  }, [channel, claimed]);
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
  return <span ref={anchor} hidden data-dialog-keys="" />;
}
