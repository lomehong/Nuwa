import js from "@eslint/js";
import globals from "globals";
import tseslint from "typescript-eslint";

export default [
  {
    ignores: ["dist/**", "node_modules/**", "coverage/**"],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["**/*.{ts,tsx}"],
    languageOptions: {
      ecmaVersion: 2022,
      globals: globals.browser,
    },
    rules: {
      "prefer-const": "error",
      "eqeqeq": ["error", "always", { "null": "ignore" }],
      // The production CSP allows `script-src 'self'` and `style-src 'self'`
      // with no inline exceptions, so keep the guard at lint time too.
      "no-restricted-syntax": [
        "error",
        {
          selector: "JSXAttribute[name.name='style']",
          message: "Inline styles are blocked by the strict CSP; use Tailwind classes or design tokens.",
        },
        {
          selector: "JSXAttribute[name.name='dangerouslySetInnerHTML']",
          message: "Raw HTML bypasses React escaping and is disallowed in the console.",
        },
        {
          selector: "JSXOpeningElement[name.name='style']",
          message: "Inline <style> elements are blocked by the strict CSP.",
        },
      ],
    },
  },
  {
    files: ["public/**/*.js"],
    languageOptions: {
      globals: globals.browser,
    },
  },
  {
    // Playwright smoke script: Node APIs at the top level, browser globals
    // inside `page.evaluate` callbacks.
    files: ["e2e/**/*.mjs"],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: "module",
      globals: { ...globals.node, ...globals.browser },
    },
  },
  {
    files: ["eslint.config.js"],
    languageOptions: {
      sourceType: "module",
      globals: globals.node,
    },
  },
];
