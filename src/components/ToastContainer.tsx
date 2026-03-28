import React, { useEffect, useRef } from "react";
import { useToastStore, type Toast } from "../stores/toastStore";
import { useSettingsStore } from "../stores/settingsStore";

function ToastItem({ toast }: { toast: Toast }): React.JSX.Element {
  const removeToast = useToastStore((s) => s.removeToast);
  const autoDismiss = useSettingsStore((s) => s.toastAutoDismiss);
  const dismissSeconds = useSettingsStore((s) => s.toastDismissSeconds);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (autoDismiss && dismissSeconds > 0) {
      timerRef.current = setTimeout(() => {
        removeToast(toast.id);
      }, dismissSeconds * 1000);
    }
    return () => {
      if (timerRef.current !== null) {
        clearTimeout(timerRef.current);
      }
    };
  }, [autoDismiss, dismissSeconds, removeToast, toast.id]);

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

export function ToastContainer(): React.JSX.Element | null {
  const toasts = useToastStore((s) => s.toasts);

  if (toasts.length === 0) return null;

  return (
    <div className="toast-container">
      {toasts.map((toast) => (
        <ToastItem key={toast.id} toast={toast} />
      ))}
    </div>
  );
}
