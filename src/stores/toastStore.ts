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
}

export interface ToastOptions {
  sticky?: boolean;
  closeAlone?: boolean;
}

/**
 * How many toasts that are not sticky are up at a time. One more takes the
 * place of the oldest of them: something that reports in a loop, such as an
 * import with a warning for every entry, could otherwise put up thousands.
 *
 * Sticky toasts do not count and never make room. Each says something the
 * user must not miss, and each is shown once, so there are only as many as
 * there are things to say.
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
  addToast: (message: string, level?: ToastLevel, options?: ToastOptions) => string;
  removeToast: (id: string) => void;
  /**
   * Dismiss every sticky toast, or every toast that is not sticky. Never
   * both: a pile of ordinary toasts must not take a sticky one with it. A
   * toast that is closed alone stays.
   */
  clearToasts: (sticky: boolean) => void;
}

export const useToastStore = create<ToastState>((set, get) => ({
  toasts: [],

  addToast: (message: string, level: ToastLevel = "error", options: ToastOptions = {}): string => {
    const sticky = options.sticky ?? false;
    // A sticky toast only goes when dismissed, so the same one is shown once
    // however often it is asked for, instead of piling up.
    const shown = sticky
      ? get().toasts.find((t) => t.sticky && t.message === message && t.level === level)
      : undefined;
    if (shown) return shown.id;
    const id = String(++nextId);
    set((state) => ({
      toasts: [
        ...(sticky ? state.toasts : withRoom(state.toasts)),
        { id, message, level, sticky, closeAlone: options.closeAlone ?? false },
      ],
    }));
    return id;
  },

  removeToast: (id: string) => {
    set((state) => ({
      toasts: state.toasts.filter((t) => t.id !== id),
    }));
  },

  clearToasts: (sticky: boolean) => {
    set((state) => ({
      toasts: state.toasts.filter((t) => t.sticky !== sticky || t.closeAlone),
    }));
  },
}));
