import { defineConfig } from "eslint/config";
import js from "@eslint/js";
import tseslint from "typescript-eslint";
import prettierConfig from "eslint-config-prettier";
import prettierPlugin from "eslint-plugin-prettier";
import securityPlugin from "eslint-plugin-security";

export default defineConfig(
  // 1. Base JS and Security Defaults
  js.configs.recommended,
  // eslint-plugin-security ships no .d.ts, so the whole import is `any`
  // upstream. The unsafe-* rules can't see its actual shape — disable them
  // on this line only. If a future typescript-eslint bump tightens
  // defineConfig()'s signature, `no-unsafe-assignment` may also start
  // firing here; add it to the list when that happens (it would currently
  // be flagged as an unused directive by ESLint's own no-unused-disable).
  // eslint-disable-next-line @typescript-eslint/no-unsafe-argument, @typescript-eslint/no-unsafe-member-access
  securityPlugin.configs.recommended,

  // 2. Strict TypeScript for Tauri IPC
  ...tseslint.configs.strictTypeChecked, // Heavily verifies types (best for invoke/emit)
  ...tseslint.configs.stylisticTypeChecked,

  {
    languageOptions: {
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
      globals: {
        // Essential for Tauri frontend globals
        window: "readonly",
        document: "readonly",
      },
    },
    plugins: {
      prettier: prettierPlugin,
    },
    rules: {
      // --- Pristine Logic ---
      "no-console": "warn",
      eqeqeq: ["error", "always"],
      curly: "error",
      "@typescript-eslint/no-explicit-any": "error", // Force types for Tauri commands
      "@typescript-eslint/no-floating-promises": "error", // Ensure Tauri 'invoke' is awaited

      // --- Tauri Security ---
      "security/detect-non-literal-fs-filename": "warn", // Critical if using tauri-plugin-fs
      "security/detect-object-injection": "off", // Often too noisy for state management

      // --- Formatting ---
      "prettier/prettier": "error",
      ...prettierConfig.rules,
    },
  },

  // 3. Ignore Build Artifacts
  {
    ignores: [
      "node_modules/",
      "dist/",
      "src-tauri/", // Rust code is handled by Clippy, not ESLint
      "target/",
    ],
  },
);
