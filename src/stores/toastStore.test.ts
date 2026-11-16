/**
 * Toast store: a sticky toast only goes when dismissed, so asking for the
 * same one again must not stack up copies of it. The other toasts are
 * limited in number, and a sticky one never goes to make room. A toast is in
 * the window's stack unless an open dialog raised it as its own.
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

describe("a dialog's layer", () => {
  const stacks = (): [string, string | null][] =>
    useToastStore.getState().toasts.map((t) => [t.message, t.layer]);

  it("holds what the open dialog raises, and nothing else", () => {
    const { addToast, openLayer } = useToastStore.getState();
    openLayer("settings");
    addToast("save failed", "error", { layer: "settings" });
    addToast("connection failed");
    expect(stacks()).toEqual([
      ["save failed", "settings"],
      ["connection failed", null],
    ]);
  });

  it("is not there for a dialog that never opened one, or has closed", () => {
    const { addToast, openLayer, closeLayer } = useToastStore.getState();
    addToast("too early", "error", { layer: "settings" });
    openLayer("settings");
    closeLayer("settings");
    // An import that ends after Settings was closed still reports.
    addToast("too late", "warning", { layer: "settings" });
    expect(stacks()).toEqual([
      ["too early", null],
      ["too late", null],
    ]);
  });

  it("hands its toasts to the window's stack when the dialog closes", () => {
    const { addToast, openLayer, closeLayer } = useToastStore.getState();
    openLayer("settings");
    openLayer("profiles");
    addToast("imported", "success", { layer: "settings" });
    addToast("not logged: a", "warning", { sticky: true, layer: "settings" });
    addToast("delete failed", "error", { layer: "profiles" });
    closeLayer("settings");
    expect(stacks()).toEqual([
      ["imported", null],
      ["not logged: a", null],
      ["delete failed", "profiles"],
    ]);
    expect(useToastStore.getState().layers).toEqual(["profiles"]);
  });

  it("opens once, however often it is asked for", () => {
    const { openLayer, closeLayer } = useToastStore.getState();
    openLayer("settings");
    openLayer("settings");
    closeLayer("settings");
    expect(useToastStore.getState().layers).toEqual([]);
  });

  it("shares the limit with the window's stack: the oldest goes, wherever it is", () => {
    const { addToast, openLayer } = useToastStore.getState();
    openLayer("settings");
    addToast("connection failed");
    addToast("save failed", "error", { layer: "settings" });
    for (let i = 0; i < MAX_TOASTS - 1; i++) {
      addToast(`warning ${String(i)}`, "warning", { layer: "settings" });
    }
    // One over the limit: the window's toast was the oldest.
    expect(useToastStore.getState().toasts).toHaveLength(MAX_TOASTS);
    expect(stacks()[0]).toEqual(["save failed", "settings"]);

    addToast("connection failed again");
    expect(useToastStore.getState().toasts).toHaveLength(MAX_TOASTS);
    expect(stacks()[0]).toEqual(["warning 0", "settings"]);
  });

  it("shows a sticky toast once, over the dialog that asks for it", () => {
    const { addToast, openLayer, closeLayer } = useToastStore.getState();
    openLayer("settings");
    const first = addToast("check is off", "error", { sticky: true });
    // Under the dialog it would be dimmed, and perhaps hidden behind it.
    expect(addToast("check is off", "error", { sticky: true, layer: "settings" })).toBe(first);
    expect(stacks()).toEqual([["check is off", "settings"]]);

    // Asked for from elsewhere, it stays where it can be seen.
    expect(addToast("check is off", "error", { sticky: true })).toBe(first);
    expect(stacks()).toEqual([["check is off", "settings"]]);

    closeLayer("settings");
    expect(stacks()).toEqual([["check is off", null]]);
    // A dialog that has closed moves nothing.
    expect(addToast("check is off", "error", { sticky: true, layer: "settings" })).toBe(first);
    expect(stacks()).toEqual([["check is off", null]]);
  });

  it("is cleared by itself, as the window's stack is", () => {
    const { addToast, clearToasts, openLayer } = useToastStore.getState();
    openLayer("settings");
    const add = (): void => {
      addToast("warning a", "warning", { layer: "settings" });
      addToast("connection failed");
      addToast("not logged: a", "warning", { sticky: true });
    };

    add();
    clearToasts(false, "settings");
    expect(stacks()).toEqual([
      ["connection failed", null],
      ["not logged: a", null],
    ]);

    add();
    clearToasts(false);
    expect(stacks()).toEqual([
      ["not logged: a", null],
      ["warning a", "settings"],
    ]);
  });

  it("is cleared of its sticky toasts alone, and keeps one that is closed alone", () => {
    const { addToast, clearToasts, openLayer } = useToastStore.getState();
    openLayer("settings");
    addToast("not logged: a", "warning", { sticky: true });
    addToast("staying", "warning", { sticky: true, layer: "settings" });
    addToast("check is off", "error", { sticky: true, closeAlone: true, layer: "settings" });
    addToast("warning a", "warning", { layer: "settings" });
    clearToasts(true, "settings");
    expect(stacks()).toEqual([
      ["not logged: a", null],
      ["check is off", "settings"],
      ["warning a", "settings"],
    ]);
  });
});
