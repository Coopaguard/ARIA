# Embedded browser panel

Postponed on 2026-11-01. `ui/open-link` opens the system browser meanwhile.

- Possible through Tauri's `Window::add_child`, a native top-level webview: `X-Frame-Options` does not apply.
- It is **not a tile**: it floats above the HTML, cannot be clipped by CSS, and escapes the sandbox CSP. It needs its
  own threat model.
- Behind Tauri's `unstable` flag. To measure: does WebView2 play protected video, and what does the layout cost?
