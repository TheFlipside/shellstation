/**
 * Global test setup: jest-dom matchers, Testing Library cleanup, and stubs
 * for the browser APIs jsdom does not implement but the components use.
 */
import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach, vi } from "vitest";

// jsdom ships no ResizeObserver. Terminal.tsx observes its container to
// re-fit xterm.js, so the constructor must at least exist and be observable.
// A function expression, not an arrow — the call site uses `new`.
globalThis.ResizeObserver = vi.fn(function (): ResizeObserver {
  return {
    observe: vi.fn(),
    unobserve: vi.fn(),
    disconnect: vi.fn(),
  };
});

// jsdom ships no scrollIntoView. TerminalTabs calls it to keep the active tab
// in view whenever the active tab changes.
Element.prototype.scrollIntoView = vi.fn();

// jsdom ships no matchMedia. useTheme() subscribes to the OS dark-mode
// preference through it; report "light" and never emit changes.
Object.defineProperty(window, "matchMedia", {
  writable: true,
  value: (query: string): MediaQueryList =>
    ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      dispatchEvent: vi.fn(() => false),
    }) as unknown as MediaQueryList,
});

// `globals: false` means Testing Library cannot auto-register this itself.
afterEach(() => {
  cleanup();
});
