/**
 * terminalTheme tests.
 *
 * Two contracts are pinned here. Selection must always be the exact inverse
 * of the colors actually in effect, including the user's overrides, because
 * that is what guarantees selected text reads as well as unselected text.
 * The cursor must stay the theme's accent while it sits on the background
 * that accent was designed for, and step aside for the foreground once an
 * override makes it unreadable.
 */
import { describe, expect, it } from "vitest";
import { XTERM_THEMES, contrastRatio, resolveTerminalTheme } from "./terminalTheme";

describe("contrastRatio", () => {
  it("spans the full WCAG range", () => {
    expect(contrastRatio("#ffffff", "#000000")).toBeCloseTo(21, 5);
    expect(contrastRatio("#7f7f7f", "#7f7f7f")).toBeCloseTo(1, 5);
  });

  it("is symmetric regardless of argument order", () => {
    expect(contrastRatio("#1e1e2e", "#cdd6f4")).toBeCloseTo(contrastRatio("#cdd6f4", "#1e1e2e"), 5);
  });

  it("reports no usable contrast for a malformed color", () => {
    // NaN must not read as "contrast is fine" at a call site comparing >=.
    expect(Number.isFinite(contrastRatio("not-a-color", "#000000"))).toBe(false);
  });
});

describe("resolveTerminalTheme selection", () => {
  it("inverts the theme palette when nothing is overridden", () => {
    const theme = resolveTerminalTheme("dark", null, null);

    expect(theme.selectionBackground).toBe(XTERM_THEMES.dark.foreground);
    expect(theme.selectionForeground).toBe(XTERM_THEMES.dark.background);
    expect(theme.selectionInactiveBackground).toBe(XTERM_THEMES.dark.foreground);
  });

  it("inverts the overrides rather than the theme defaults", () => {
    const theme = resolveTerminalTheme("dark", "#102030", "#f0e0d0");

    expect(theme.selectionBackground).toBe("#f0e0d0");
    expect(theme.selectionForeground).toBe("#102030");
  });

  it("inverts a single override against the theme's other half", () => {
    const theme = resolveTerminalTheme("light", null, "#123456");

    expect(theme.selectionBackground).toBe("#123456");
    expect(theme.selectionForeground).toBe(XTERM_THEMES.light.background);
  });

  it("gives selected text the same contrast as unselected text", () => {
    const theme = resolveTerminalTheme("dark", "#000000", "#ffffff");

    expect(contrastRatio(theme.selectionBackground, theme.selectionForeground)).toBeCloseTo(
      contrastRatio(theme.background, theme.foreground),
      5,
    );
  });
});

describe("resolveTerminalTheme cursor", () => {
  it("keeps each theme's accent cursor when the background is not overridden", () => {
    // Even the light theme's low-contrast accent stays: it was chosen for
    // that background, and only the user changing the background invalidates
    // that choice.
    expect(resolveTerminalTheme("dark", null, null).cursor).toBe(XTERM_THEMES.dark.cursor);
    expect(resolveTerminalTheme("light", null, null).cursor).toBe(XTERM_THEMES.light.cursor);
    expect(resolveTerminalTheme("light", null, "#000000").cursor).toBe(XTERM_THEMES.light.cursor);
  });

  it("treats an override equal to the theme background as no override", () => {
    // The color picker opens pre-filled with the theme default, so confirming
    // it unchanged stores exactly this. It must not disturb the accent — the
    // light theme is the case that would otherwise flip, at 2.3:1.
    const light = XTERM_THEMES.light;

    expect(resolveTerminalTheme("light", light.background, null).cursor).toBe(light.cursor);
    expect(resolveTerminalTheme("light", light.background.toUpperCase(), null).cursor).toBe(
      light.cursor,
    );
    expect(contrastRatio(light.cursor, light.background)).toBeLessThan(3);
  });

  it("keeps the accent cursor on an override it still contrasts with", () => {
    const theme = resolveTerminalTheme("dark", "#000000", null);

    expect(theme.cursor).toBe(XTERM_THEMES.dark.cursor);
    expect(contrastRatio(theme.cursor, theme.background)).toBeGreaterThanOrEqual(3);
  });

  it("falls back to the foreground when an override hides the accent cursor", () => {
    // The dark theme's near-white cursor on a white background: the exact
    // case where the cursor would vanish.
    const theme = resolveTerminalTheme("dark", "#ffffff", "#202020");

    expect(contrastRatio(XTERM_THEMES.dark.cursor, "#ffffff")).toBeLessThan(3);
    expect(theme.cursor).toBe("#202020");
  });

  it("falls back to the theme foreground when only the background is overridden", () => {
    const theme = resolveTerminalTheme("dark", "#ffffff", null);

    expect(theme.cursor).toBe(XTERM_THEMES.dark.foreground);
  });
});
