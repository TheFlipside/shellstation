import { create } from "zustand";

export type ToastLevel = "error" | "warning" | "success" | "info";

export interface Toast {
  /** A number that counts up: read as a number, a later toast's is greater. */
  id: string;
  message: string;
  level: ToastLevel;
  /** Stays until dismissed, whatever the auto-dismiss setting says. */
  sticky: boolean;
  /**
   * Closed by its own button only, never together with other toasts: for a
   * notice that must not go with a pile of others that the user clears.
   */
  closeAlone: boolean;
  /**
   * The open dialog that raised it and shows it with itself, or null for the
   * window's own stack, which lies under every dialog.
   */
  layer: string | null;
}

export interface ToastOptions {
  sticky?: boolean;
  closeAlone?: boolean;
  /**
   * The dialog that raises it, by the name it opened a layer under (see
   * `useToastLayer`). Without one, or once that dialog has closed, the toast
   * goes to the window's own stack.
   */
  layer?: string;
}

/**
 * How many toasts that are not sticky are up at a time. One more takes the
 * place of the oldest of them: something that reports in a loop, such as an
 * import with a warning for every entry, could otherwise put up thousands.
 *
 * Sticky toasts do not count and never make room. Each says something the
 * user must not miss, and each is shown once, so there are only as many as
 * there are things to say.
 *
 * The limit is for the window's stack and the dialogs' layers together. One
 * for each would not hold: a dialog's toasts go to the window's stack when it
 * closes, which would then be over its limit, or drop its oldest there and
 * then.
 */
export const MAX_TOASTS = 100;

let nextId = 0;

/** `toasts` with room for one more toast that is not sticky. */
function withRoom(toasts: Toast[]): Toast[] {
  let excess = toasts.filter((t) => !t.sticky).length - (MAX_TOASTS - 1);
  if (excess <= 0) return toasts;
  return toasts.filter((t) => {
    if (t.sticky || excess <= 0) return true;
    excess -= 1;
    return false;
  });
}

interface ToastState {
  toasts: Toast[];
  /**
   * The dialogs that show their own toasts and are open.
   *
   * A dialog lies above the window's stack of toasts, so that no toast can
   * cover a dialog or take a click meant for it: the host key prompt is a
   * dialog, and a toast can hold text from a file or a host. A toast that a
   * dialog raises itself, as Settings does for an error, would be hidden
   * under it. It is shown in a strip on top of that dialog's box, which
   * covers nothing and which any dialog on top of that one covers in turn.
   */
  layers: string[];
  addToast: (message: string, level?: ToastLevel, options?: ToastOptions) => string;
  removeToast: (id: string) => void;
  /**
   * Dismiss every sticky toast, or every toast that is not sticky, of the
   * window's stack or of one dialog's layer. Never sticky and not sticky
   * both: a pile of ordinary toasts must not take a sticky one with it. Nor
   * the toasts of another stack, which the button's count does not hold. A
   * toast that is closed alone stays.
   */
  clearToasts: (sticky: boolean, layer?: string | null) => void;
  /** A dialog opens that shows its own toasts. */
  openLayer: (layer: string) => void;
  /**
   * That dialog closes. The toasts it still shows go to the window's stack,
   * so that closing a dialog loses none of what it had to say.
   */
  closeLayer: (layer: string) => void;
}

export const useToastStore = create<ToastState>((set, get) => ({
  toasts: [],
  layers: [],

  addToast: (message: string, level: ToastLevel = "error", options: ToastOptions = {}): string => {
    const sticky = options.sticky ?? false;
    // A sticky toast only goes when dismissed, so the same one is shown once
    // however often it is asked for, instead of piling up.
    const shown = sticky
      ? get().toasts.find((t) => t.sticky && t.message === message && t.level === level)
      : undefined;
    // A dialog's work can end after the dialog has closed: what it reports
    // then has no layer left to show in.
    const layer =
      options.layer !== undefined && get().layers.includes(options.layer) ? options.layer : null;
    if (shown) {
      // Shown once, but where the dialog that asks now can be seen to say
      // it: under that dialog it would be dimmed, and perhaps hidden.
      if (layer !== null && shown.layer !== layer) {
        set((state) => ({
          toasts: state.toasts.map((t) => (t.id === shown.id ? { ...t, layer } : t)),
        }));
      }
      return shown.id;
    }
    const id = String(++nextId);
    set((state) => ({
      toasts: [
        ...(sticky ? state.toasts : withRoom(state.toasts)),
        { id, message, level, sticky, closeAlone: options.closeAlone ?? false, layer },
      ],
    }));
    return id;
  },

  removeToast: (id: string) => {
    set((state) => ({
      toasts: state.toasts.filter((t) => t.id !== id),
    }));
  },

  clearToasts: (sticky: boolean, layer: string | null = null) => {
    set((state) => ({
      toasts: state.toasts.filter((t) => t.layer !== layer || t.sticky !== sticky || t.closeAlone),
    }));
  },

  openLayer: (layer: string) => {
    set((state) => (state.layers.includes(layer) ? state : { layers: [...state.layers, layer] }));
  },

  closeLayer: (layer: string) => {
    set((state) => ({
      layers: state.layers.filter((l) => l !== layer),
      toasts: state.toasts.some((t) => t.layer === layer)
        ? state.toasts.map((t) => (t.layer === layer ? { ...t, layer: null } : t))
        : state.toasts,
    }));
  },
}));
