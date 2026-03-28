import { create } from "zustand";

export type ToastLevel = "error" | "warning" | "success" | "info";

export interface Toast {
  id: string;
  message: string;
  level: ToastLevel;
}

let nextId = 0;

interface ToastState {
  toasts: Toast[];
  addToast: (message: string, level?: ToastLevel) => string;
  removeToast: (id: string) => void;
}

export const useToastStore = create<ToastState>((set) => ({
  toasts: [],

  addToast: (message: string, level: ToastLevel = "error"): string => {
    const id = String(++nextId);
    set((state) => ({
      toasts: [...state.toasts, { id, message, level }],
    }));
    return id;
  },

  removeToast: (id: string) => {
    set((state) => ({
      toasts: state.toasts.filter((t) => t.id !== id),
    }));
  },
}));
