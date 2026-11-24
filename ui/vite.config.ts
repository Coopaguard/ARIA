import { defineConfig } from "vite";
import vue from "@vitejs/plugin-vue";

// The shell's `tauri.conf.json` names `http://localhost:5173` as its `devUrl`,
// so the port is pinned rather than negotiated: a dev server that quietly moved
// to 5174 is a window that shows nothing with no error anywhere.
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    // Tauri ships its own WebView — Edge WebView2 on Windows — so there is no
    // old browser to support and no reason to down-level the output.
    target: "esnext",
    // The Rust build is the slow half; this one may as well be readable when
    // something goes wrong in it.
    sourcemap: true,
    rollupOptions: {
      // Two pages, because there are two windows. The floating overview is a
      // real second window — undecorated, transparent, always on top — and
      // `app/tauri.conf.json` loads it by URL, so it needs its own entry in the
      // built output. Listed explicitly: Vite only auto-detects `index.html`,
      // and a missing entry here means a window that loads a 404 in production
      // while working perfectly in dev.
      input: {
        main: "index.html",
        overview: "overview.html",
      },
    },
  },
});
