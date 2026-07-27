/**
 * Terminal color palettes and the derivation that turns one into an xterm.js
 * theme.
 *
 * This lives outside the components so the settings preview and the live
 * terminal cannot drift apart: both call `resolveTerminalTheme()`, so what
 * the preview swatch shows is what a session actually renders.
 */
import type { ResolvedTheme } from "./hooks/useTheme";

/** xterm.js color themes keyed by resolved theme name. */
export const XTERM_THEMES: Record<ResolvedTheme, Record<string, string>> = {
  dark: {
    background: "#1e1e2e",
    foreground: "#cdd6f4",
    cursor: "#f5e0dc",
    black: "#45475a",
    red: "#f38ba8",
    green: "#a6e3a1",
    yellow: "#f9e2af",
    blue: "#89b4fa",
    magenta: "#f5c2e7",
    cyan: "#94e2d5",
    white: "#bac2de",
    brightBlack: "#585b70",
    brightRed: "#f38ba8",
    brightGreen: "#a6e3a1",
    brightYellow: "#f9e2af",
    brightBlue: "#89b4fa",
    brightMagenta: "#f5c2e7",
    brightCyan: "#94e2d5",
    brightWhite: "#a6adc8",
  },
  light: {
    background: "#eff1f5",
    foreground: "#4c4f69",
    cursor: "#dc8a78",
    black: "#5c5f77",
    red: "#d20f39",
    green: "#40a02b",
    yellow: "#df8e1d",
    blue: "#1e66f5",
    magenta: "#ea76cb",
    cyan: "#179299",
    white: "#acb0be",
    brightBlack: "#6c6f85",
    brightRed: "#d20f39",
    brightGreen: "#40a02b",
    brightYellow: "#df8e1d",
    brightBlue: "#1e66f5",
    brightMagenta: "#ea76cb",
    brightCyan: "#179299",
    brightWhite: "#bcc0cc",
  },
};

/**
 * WCAG relative luminance of a `#RRGGBB` color. A malformed string yields
 * NaN, which propagates through `contrastRatio` — callers treat a
 * non-finite ratio as "no contrast" and fall back, so bad input degrades to
 * the safe branch instead of throwing.
 */
function relativeLuminance(hex: string): number {
  const channels = [1, 3, 5].map((offset) => {
    const value = parseInt(hex.slice(offset, offset + 2), 16) / 255;
    return value <= 0.03928 ? value / 12.92 : Math.pow((value + 0.055) / 1.055, 2.4);
  });
  return 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2];
}

/** WCAG contrast ratio between two `#RRGGBB` colors, from 1 to 21. */
export function contrastRatio(a: string, b: string): number {
  const lumA = relativeLuminance(a);
  const lumB = relativeLuminance(b);
  const lighter = Math.max(lumA, lumB);
  const darker = Math.min(lumA, lumB);
  return (lighter + 0.05) / (darker + 0.05);
}

/** WCAG minimum contrast for a non-text UI element such as the cursor. */
const MIN_CURSOR_CONTRAST = 3;

/**
 * Apply the user's color overrides (if any) on top of the active app-theme
 * palette, then derive the colors that depend on the result. `null` for
 * either override means "use the theme's default", so both are optional.
 *
 * Selection is rendered as reverse video: the effective foreground becomes
 * the selection background and vice versa, so selected text has exactly the
 * contrast of unselected text — it cannot be washed out the way a
 * translucent tint is against bright output. Deriving both from the resolved
 * palette rather than hard-coding them per theme is what keeps the selection
 * legible after the user overrides the terminal colors in settings.
 *
 * The selection colors are deliberately opaque. xterm.js clamps an opaque
 * selection to 30% alpha, but only for the `*Transparent` variants the DOM
 * renderer uses; the WebGL renderer this app installs reads the opaque ones,
 * which are computed before that clamp (ThemeService.ts). Inactive selection
 * is set to the same pair on purpose: `selectionForeground` applies whether
 * or not the terminal has focus, so dimming only the background would push
 * the forced text color toward the background and reintroduce the
 * unreadable case.
 *
 * The cursor is only second-guessed when the background actually differs
 * from the theme's own. Each theme's cursor is an accent chosen against that
 * theme's background, so it is left alone while that assumption holds; once
 * a different background is in effect the accent may no longer be visible
 * against it, and the substitute is the effective foreground — the color the
 * terminal already draws text in, so the cursor ends up exactly as readable
 * as the text around it, whatever the user chose.
 *
 * That comparison is on the resolved color, not on "is an override set":
 * the color picker opens pre-filled with the theme default, so confirming it
 * unchanged stores an override identical to the default, which has to behave
 * exactly like no override at all.
 */
export function resolveTerminalTheme(
  base: ResolvedTheme,
  bgOverride: string | null,
  fgOverride: string | null,
): Record<string, string> {
  const palette = XTERM_THEMES[base];
  const background = bgOverride ?? palette.background;
  const foreground = fgOverride ?? palette.foreground;
  // Case-insensitive: the store accepts either case, the palette is lower.
  const backgroundIsThemeDefault = background.toLowerCase() === palette.background.toLowerCase();
  const cursorContrast = contrastRatio(palette.cursor, background);
  const cursorSurvivesOverride =
    Number.isFinite(cursorContrast) && cursorContrast >= MIN_CURSOR_CONTRAST;
  return {
    ...palette,
    background,
    foreground,
    cursor: backgroundIsThemeDefault || cursorSurvivesOverride ? palette.cursor : foreground,
    selectionBackground: foreground,
    selectionForeground: background,
    selectionInactiveBackground: foreground,
  };
}
