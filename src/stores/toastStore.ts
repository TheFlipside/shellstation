import { create } from "zustand";

export type ToastLevel = "error" | "warning" | "success" | "info";

export interface Toast {
  id: string;
  message: string;
  level: ToastLevel;
  /** Stays until dismissed, whatever the auto-dismiss setting says. */
  sticky: boolean;
}

export interface ToastOptions {
  sticky?: boolean;
}

let nextId = 0;

interface ToastState {
  toasts: Toast[];
  addToast: (message: string, level?: ToastLevel, options?: ToastOptions) => string;
  removeToast: (id: string) => void;
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
      toasts: [...state.toasts, { id, message, level, sticky }],
    }));
    return id;
  },

  removeToast: (id: string) => {
    set((state) => ({
      toasts: state.toasts.filter((t) => t.id !== id),
    }));
  },
}));
