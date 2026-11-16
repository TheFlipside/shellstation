import { useCallback, useId, useLayoutEffect } from "react";
import { useToastStore, type ToastLevel, type ToastOptions } from "../stores/toastStore";

export interface ToastLayerHandle {
  /** What `<ToastLayer>` in the dialog's overlay is given. */
  layer: string;
  /** Raise a toast as this dialog's own. */
  addToast: (message: string, level?: ToastLevel, options?: Omit<ToastOptions, "layer">) => string;
}

/**
 * For a dialog that raises toasts while it stays open: Settings and the two
 * managers. A dialog lies above the window's toasts, so one it raised would
 * be under it, dimmed and partly hidden behind it. A toast raised through
 * this hook is shown with the dialog instead, in a strip on top of its box,
 * which the dialog renders with `<ToastLayer layer={layer} />` in its
 * overlay, right after its `<DialogKeys>`. The overlay gets the class
 * `dialog-overlay-messages`. No toast floats over a dialog: the strip is
 * part of the overlay's layout and covers nothing.
 *
 * Only what the dialog itself reports goes through here. A toast from
 * anywhere else, such as a tab's failed connect while the dialog is open, is
 * not the dialog's and stays under it.
 *
 * When the dialog closes, the toasts it still shows go to the window's stack.
 */
export function useToastLayer(): ToastLayerHandle {
  const layer = useId();

  // A layout effect, so the layer is open before an effect of the dialog
  // can report something. A layout effect of something the dialog renders
  // runs before this one: what it reported would go to the window's stack.
  useLayoutEffect(() => {
    useToastStore.getState().openLayer(layer);
    return () => {
      useToastStore.getState().closeLayer(layer);
    };
  }, [layer]);

  const addToast = useCallback<ToastLayerHandle["addToast"]>(
    (message, level, options) =>
      useToastStore.getState().addToast(message, level, { ...options, layer }),
    [layer],
  );

  return { layer, addToast };
}
