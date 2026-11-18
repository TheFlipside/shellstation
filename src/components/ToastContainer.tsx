import React, { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useToastStore, type Toast } from "../stores/toastStore";
import { useSettingsStore } from "../stores/settingsStore";
import { useDialogOver } from "./DialogKeys";

/**
 * A toast. Its time does not run while it `waits`: under a dialog, where it
 * may be hidden, and in a dialog's own strip, which would move the dialog's
 * content when a toast went by itself. It starts, in full, once the toast
 * waits no longer.
 */
function ToastItem({ toast, waits }: { toast: Toast; waits: boolean }): React.JSX.Element {
  const removeToast = useToastStore((s) => s.removeToast);
  const autoDismiss = useSettingsStore((s) => s.toastAutoDismiss);
  const dismissSeconds = useSettingsStore((s) => s.toastDismissSeconds);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (!toast.sticky && !waits && autoDismiss && dismissSeconds > 0) {
      timerRef.current = setTimeout(() => {
        removeToast(toast.id);
      }, dismissSeconds * 1000);
    }
    return () => {
      if (timerRef.current !== null) {
        clearTimeout(timerRef.current);
      }
    };
  }, [autoDismiss, dismissSeconds, removeToast, toast.id, toast.sticky, waits]);

  return (
    <div className={`toast toast-${toast.level}`}>
      <span className="toast-message">{toast.message}</span>
      <button
        type="button"
        className="toast-close"
        onClick={() => {
          removeToast(toast.id);
        }}
      >
        &times;
      </button>
    </div>
  );
}

/**
 * The sticky toasts, or the others, oldest first, in a list that scrolls
 * when it is too tall. With more than one toast in it that may be closed
 * with others, a button above the list, which does not scroll with it,
 * closes those: toasts of this group in this stack and no others, and none
 * that is closed alone.
 */
function ToastGroup({
  toasts,
  sticky,
  layer,
  waits,
}: {
  toasts: Toast[];
  sticky: boolean;
  layer: string | null;
  waits: boolean;
}): React.JSX.Element | null {
  const { t } = useTranslation();
  const clearToasts = useToastStore((s) => s.clearToasts);
  const listRef = useRef<HTMLDivElement>(null);
  const newestShown = useRef(0);

  const [scrolls, setScrolls] = useState(false);

  // The stack lets the pointer through to what is under it, all but its
  // toasts and buttons, since it lies over the tab bar and a terminal's
  // scroll bar. A list too tall for its room is the exception: its scroll
  // bar could not be used otherwise. The room depends on the window and on
  // the other group, which renders when this one does.
  useLayoutEffect(() => {
    const measure = (): void => {
      const list = listRef.current;
      setScrolls(list !== null && list.scrollHeight > list.clientHeight);
    };
    measure();
    window.addEventListener("resize", measure);
    return () => {
      window.removeEventListener("resize", measure);
    };
  }, [toasts]);

  // A new toast goes to the end of its list, which is out of sight once the
  // list is taller than its room. The list of toasts that are not sticky
  // scrolls to a new one, before it is painted. Only a new toast scrolls it:
  // one that goes leaves the list where the user has it. Ids count up, so
  // the last toast is new when its id is greater than any seen before.
  //
  // The sticky toasts are never scrolled for the user. Each says something
  // that must not be missed, and scrolling to a new one would take an older
  // one out of sight.
  const newest = toasts.length > 0 ? Number(toasts[toasts.length - 1].id) : 0;
  useLayoutEffect(() => {
    if (sticky || newest <= newestShown.current) return;
    newestShown.current = newest;
    const list = listRef.current;
    if (list) list.scrollTop = list.scrollHeight;
  }, [newest, sticky]);

  if (toasts.length === 0) return null;

  const closedTogether = toasts.filter((toast) => !toast.closeAlone).length;

  return (
    <div className={`toast-group ${sticky ? "toast-group-sticky" : "toast-group-passing"}`}>
      {closedTogether > 1 && (
        <button
          type="button"
          className="toast-close-all"
          onClick={() => {
            clearToasts(sticky, layer);
          }}
        >
          {t("common.closeAllNotifications", { count: String(closedTogether) })}
        </button>
      )}
      <div ref={listRef} className={scrolls ? "toast-list toast-list-scrolls" : "toast-list"}>
        {toasts.map((toast) => (
          <ToastItem key={toast.id} toast={toast} waits={waits} />
        ))}
      </div>
    </div>
  );
}

/**
 * A stack with toasts in it. It is never taller than the window: its lists
 * scroll instead, so that every toast can be read and closed.
 *
 * The sticky toasts come first, in a group of their own, whatever came when.
 * However many other toasts arrive, such as an import's warnings, they then
 * neither push a sticky toast out of sight nor are closed together with one.
 *
 * While a dialog lies over the stack, it is inert: the pointer does not
 * reach it through the dialog anyway, and the keyboard must not either. A
 * button under a dialog would otherwise still take the focus, and with it a
 * key meant for the dialog.
 *
 * The window's stack floats at the window's top right, under every dialog.
 * A dialog's own is a strip on top of the dialog's box, in the overlay's
 * layout (App.css): it covers nothing of the dialog, and the box makes room
 * for it. Its toasts stay until they are closed or the dialog is.
 */
function ToastStack({
  toasts,
  layer,
}: {
  toasts: Toast[];
  layer: string | null;
}): React.JSX.Element {
  const stackRef = useRef<HTMLDivElement>(null);
  const covered = useDialogOver(stackRef);

  return (
    <div
      ref={stackRef}
      className={layer === null ? "toast-stack toast-container" : "toast-stack toast-strip"}
      inert={covered}
    >
      <ToastGroup
        toasts={toasts.filter((toast) => toast.sticky)}
        sticky={true}
        layer={layer}
        waits={covered || layer !== null}
      />
      <ToastGroup
        toasts={toasts.filter((toast) => !toast.sticky)}
        sticky={false}
        layer={layer}
        waits={covered || layer !== null}
      />
    </div>
  );
}

/**
 * The window's stack of toasts. Every dialog lies above it: its toasts show
 * dimmed beside a dialog, take no input and wait there, and no toast covers
 * a dialog or takes a click meant for it. It comes before every dialog in the
 * document (see `useDialogOver`), so the app renders it first.
 */
export function ToastContainer(): React.JSX.Element | null {
  const toasts = useToastStore((s) => s.toasts);
  const shown = toasts.filter((toast) => toast.layer === null);

  // The stack is there only with toasts in it, and asks what lies over it
  // when it appears.
  if (shown.length === 0) return null;
  return <ToastStack toasts={shown} layer={null} />;
}

/**
 * How long a dialog's box takes no click after its strip has changed, in
 * milliseconds: longer than the second click of a double click takes.
 */
export const SETTLE_MS = 500;

/** What the pointer does to what it is over. */
const POINTER_EVENTS = [
  "pointerdown",
  "mousedown",
  "pointerup",
  "mouseup",
  "click",
  "dblclick",
  "auxclick",
  "contextmenu",
] as const;

function refuse(event: Event): void {
  event.preventDefault();
  event.stopPropagation();
}

/**
 * The toasts a dialog raised itself, shown with that dialog: in its overlay,
 * right after its `<DialogKeys>` and before its box, as a strip on top of the
 * box. The overlay has the class `dialog-overlay-messages`, which lays the
 * two out one above the other. Every dialog opened from it or lying on top
 * of it covers the strip as it covers the box. See `useToastLayer`.
 *
 * The strip is as tall as what it holds, so the box under it moves when a
 * toast comes or goes, and a control of the dialog comes to lie where the
 * pointer is: under the button that closed the last toast, for the second
 * click of a double click, or under a click that was on its way when a toast
 * came. For a moment after the strip has changed, the box therefore takes no
 * click. It is refused at the box, and is not passed on.
 */
export function ToastLayer({ layer }: { layer: string }): React.JSX.Element {
  const toasts = useToastStore((s) => s.toasts);
  const shown = toasts.filter((toast) => toast.layer === layer);
  const anchor = useRef<HTMLSpanElement>(null);

  // The toasts the strip holds decide how tall it is.
  const held = shown.map((toast) => toast.id).join(" ");
  const heldBefore = useRef(held);
  useLayoutEffect(() => {
    if (held === heldBefore.current) return undefined;
    heldBefore.current = held;
    const overlay = anchor.current?.parentElement;
    if (!overlay) return undefined;
    const boxes = Array.from(overlay.children).filter((child) =>
      child.classList.contains("dialog"),
    );
    for (const box of boxes) {
      for (const type of POINTER_EVENTS) box.addEventListener(type, refuse, true);
    }
    const release = (): void => {
      for (const box of boxes) {
        for (const type of POINTER_EVENTS) box.removeEventListener(type, refuse, true);
      }
    };
    const timer = window.setTimeout(release, SETTLE_MS);
    return () => {
      window.clearTimeout(timer);
      release();
    };
  }, [held]);

  return (
    <>
      {/* Says where the dialog's box is, also while there is no strip. */}
      <span ref={anchor} hidden />
      {shown.length > 0 && <ToastStack toasts={shown} layer={layer} />}
    </>
  );
}
