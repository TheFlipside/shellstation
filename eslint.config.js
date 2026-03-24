import js from "@eslint/js";
import tseslint from "typescript-eslint";
import prettierConfig from "eslint-config-prettier";
import prettierPlugin from "eslint-plugin-prettier";
import securityPlugin from "eslint-plugin-security";

export default tseslint.config(
  // 1. Base JS and Security Defaults
  js.configs.recommended,
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
      "eqeqeq": ["error", "always"],
      "curly": "error",
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
      "target/"
    ]
  }
);
