/**
 * Toast store: a sticky toast only goes when dismissed, so asking for the
 * same one again must not stack up copies of it.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { useToastStore } from "./toastStore";

beforeEach(() => {
  useToastStore.setState(useToastStore.getInitialState(), true);
});

describe("addToast", () => {
  it("shows the same sticky toast once", () => {
    const { addToast } = useToastStore.getState();
    const first = addToast("check is off", "error", { sticky: true });
    expect(addToast("check is off", "error", { sticky: true })).toBe(first);
    expect(useToastStore.getState().toasts).toHaveLength(1);
  });

  it("still adds a sticky toast that differs, or one that is not sticky", () => {
    const { addToast } = useToastStore.getState();
    addToast("check is off", "error", { sticky: true });
    addToast("check is off", "warning", { sticky: true });
    addToast("another", "error", { sticky: true });
    addToast("check is off", "error");
    addToast("check is off", "error");
    expect(useToastStore.getState().toasts).toHaveLength(5);
  });

  it("shows a sticky toast again once the last one was dismissed", () => {
    const { addToast, removeToast } = useToastStore.getState();
    const first = addToast("check is off", "error", { sticky: true });
    removeToast(first);
    expect(addToast("check is off", "error", { sticky: true })).not.toBe(first);
    expect(useToastStore.getState().toasts).toHaveLength(1);
  });
});
