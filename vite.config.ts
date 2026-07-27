/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const host: string | undefined = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],

  build: {
    // Tauri loads assets from local disk, so larger chunks are fine.
    // xterm.js (with WebGL addon) is ~500 kB and cannot be split further.
    chunkSizeWarningLimit: 600,
    rollupOptions: {
      output: {
        manualChunks(id: string) {
          if (id.includes("node_modules/@xterm/")) return "xterm";
          if (
            id.includes("node_modules/react/") ||
            id.includes("node_modules/react-dom/") ||
            id.includes("node_modules/zustand/") ||
            id.includes("node_modules/i18next/") ||
            id.includes("node_modules/react-i18next/")
          )
            return "vendor";
        },
      },
    },
  },

  // Component tests run under jsdom. `globals` stays off — test files import
  // describe/it/expect explicitly, matching the project's no-implicit-globals
  // stance, which also means Testing Library's auto-cleanup is registered
  // manually in the setup file.
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
    // clearMocks (not restoreMocks) — call history is reset between tests
    // while the module-mock factories keep their implementations.
    clearMocks: true,
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host ?? false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
});
