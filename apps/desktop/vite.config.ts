import { defineConfig, type Plugin } from "vite";
// @ts-expect-error type error without @types/node package
import process from "node:process";
import { renameSync } from "node:fs";
import { join } from "node:path";
const host = process.env.TAURI_DEV_HOST;

/** Publishes the web entry as index.html, the page the CLI embeds. */
function webIndex(): Plugin {
  return {
    name: "fetchpath-web-index",
    writeBundle(options) {
      const dir = options.dir ?? "dist-web";
      renameSync(join(dir, "web.html"), join(dir, "index.html"));
    },
  };
}

// https://vite.dev/config/
export default defineConfig(({ mode }) => ({
  // WebView2 is evergreen Chromium: keep light-dark() native instead of letting the
  // CSS minifier rewrite it (design system decision 4, no older-browser fallback).
  build:
    mode === "web"
      ? {
          cssTarget: "chrome123",
          // The browser entry: web.html becomes dist-web/index.html, which the CLI embeds.
          // Nothing is inlined, since the page runs under default-src 'self'.
          outDir: "dist-web",
          emptyOutDir: true,
          assetsInlineLimit: 0,
          rolldownOptions: { input: "web.html" },
        }
      : { cssTarget: "chrome123" },
  plugins: mode === "web" ? [webIndex()] : [],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    // design/tokens.css lives outside this package and is imported by styles.css.
    fs: { allow: ["../.."] },
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
}));
