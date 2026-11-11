/**
 * Toast store: a sticky toast only goes when dismissed, so asking for the
 * same one again must not stack up copies of it. The other toasts are
 * limited in number, and a sticky one never goes to make room.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { MAX_TOASTS, useToastStore } from "./toastStore";

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

  it("holds a limited number of toasts that are not sticky, dropping the oldest", () => {
    const { addToast } = useToastStore.getState();
    for (let i = 0; i < MAX_TOASTS + 3; i++) addToast(`warning ${String(i)}`, "warning");
    const messages = useToastStore.getState().toasts.map((t) => t.message);
    expect(messages).toHaveLength(MAX_TOASTS);
    expect(messages[0]).toBe("warning 3");
    expect(messages[MAX_TOASTS - 1]).toBe(`warning ${String(MAX_TOASTS + 2)}`);
  });

  it("never drops a sticky toast to make room, and does not count it", () => {
    const { addToast } = useToastStore.getState();
    addToast("not logged: a", "warning", { sticky: true });
    for (let i = 0; i < MAX_TOASTS; i++) addToast(`warning ${String(i)}`, "warning");
    addToast("not logged: b", "warning", { sticky: true });
    // Full of toasts that are not sticky: the next one takes the place of the
    // oldest of those, which is not the first in the stack.
    addToast("one more", "error");
    const { toasts } = useToastStore.getState();
    expect(toasts.filter((t) => t.sticky).map((t) => t.message)).toEqual([
      "not logged: a",
      "not logged: b",
    ]);
    const others = toasts.filter((t) => !t.sticky).map((t) => t.message);
    expect(others).toHaveLength(MAX_TOASTS);
    expect(others[0]).toBe("warning 1");
    expect(others[MAX_TOASTS - 1]).toBe("one more");
    expect(toasts[0].message).toBe("not logged: a");
  });

  it("keeps every sticky toast, however many", () => {
    const { addToast } = useToastStore.getState();
    for (let i = 0; i < MAX_TOASTS + 5; i++) {
      addToast(`not logged: ${String(i)}`, "warning", { sticky: true });
    }
    expect(useToastStore.getState().toasts).toHaveLength(MAX_TOASTS + 5);
  });

  it("gives a later toast the greater id", () => {
    const { addToast } = useToastStore.getState();
    const first = Number(addToast("one"));
    expect(Number(addToast("two", "info", { sticky: true }))).toBeGreaterThan(first);
  });
});

describe("clearToasts", () => {
  it("dismisses the sticky toasts, or the others, and never both", () => {
    const { addToast, clearToasts } = useToastStore.getState();
    const add = (): void => {
      addToast("passing");
      addToast("staying", "warning", { sticky: true });
      addToast("passing too", "info");
      addToast("staying too", "error", { sticky: true });
    };
    const messages = (): string[] => useToastStore.getState().toasts.map((t) => t.message);

    add();
    clearToasts(false);
    expect(messages()).toEqual(["staying", "staying too"]);

    add();
    clearToasts(true);
    expect(messages()).toEqual(["passing", "passing too"]);
  });

  it("leaves a toast that is closed alone, which still goes by itself", () => {
    const { addToast, clearToasts, removeToast } = useToastStore.getState();
    const alone = addToast("check is off", "error", { sticky: true, closeAlone: true });
    addToast("not logged: a", "warning", { sticky: true });
    addToast("not logged: b", "warning", { sticky: true });
    addToast("passing");
    clearToasts(true);
    clearToasts(false);
    expect(useToastStore.getState().toasts.map((t) => t.id)).toEqual([alone]);
    removeToast(alone);
    expect(useToastStore.getState().toasts).toHaveLength(0);
  });
});
