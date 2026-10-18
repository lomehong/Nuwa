import { mkdir, writeFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import type { Plugin } from "vite";
import { defineConfig } from "vitest/config";

const distDir = fileURLToPath(new URL("./dist", import.meta.url));

/**
 * Vite empties `outDir` on every build. `web/dist/.gitkeep` is tracked so a
 * fresh clone can compile `web.rs` (rust-embed requires the folder), so put
 * it back after the bundle is written.
 */
function keepDistGitkeep(): Plugin {
  return {
    name: "nuwa:keep-dist-gitkeep",
    apply: "build",
    async closeBundle() {
      await mkdir(distDir, { recursive: true });
      await writeFile(fileURLToPath(new URL("./dist/.gitkeep", import.meta.url)), "");
    },
  };
}

export default defineConfig({
  base: "/",
  plugins: [react(), tailwindcss(), keepDistGitkeep()],
  build: {
    outDir: "dist",
    emptyOutDir: true,
    // Vite injects the modulepreload polyfill as an inline <script>. The
    // production CSP is `script-src 'self'` with no inline scripts, so rely
    // on the native feature instead (all supported targets have it).
    modulePreload: { polyfill: false },
  },
  server: {
    proxy: {
      // Keep the browser's Host and Origin intact: the API rejects writes
      // whose Origin host differs from Host, and `changeOrigin: true` would
      // rewrite Host to 127.0.0.1 while the browser keeps localhost:5173.
      "/api": { target: "http://127.0.0.1:7321", changeOrigin: false },
      "/ingest": { target: "http://127.0.0.1:7321", changeOrigin: false },
    },
  },
  test: {
    environment: "jsdom",
    globals: true,
    setupFiles: ["./src/test/setup.ts"],
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
    css: true,
    restoreMocks: true,
  },
});
