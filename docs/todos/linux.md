# Linux

Wanted. Never built there: what follows was read in the code on 2026-11-24.

**Tied to Windows**

- ONNX Runtime: `app/build.rs` fetches DirectML only, and nothing outside Windows. The runtimes ARIA downloads
  (`aria_voice::runtime`) are Windows ones. A CPU build must be shipped instead, then CUDA; ROCm exists on Linux,
  which would give AMD cards more than they have today ([GPU](gpu.md)).
- The `directml` feature is always on in `crates/voice/Cargo.toml`, with no target.
- Card detection goes through DXGI (`crates/voice/src/gpu.rs`); elsewhere it needs its own.
- Folders read from `LOCALAPPDATA` and `USERPROFILE`, with or without a fallback to `HOME`. Expected:
  `~/.local/share/aria`, and `~/.aria` unchanged.
- `llama-server.exe` by name, and `taskkill` to stop it (`agent/local/sidecar.rs`). Two messages on screen
  quote `taskkill` too.
- The installer (NSIS), the updater's target and both workflows.

**To check there**: the global shortcut and the always-on-top transparent overview under Wayland, the tray icon,
the microphone and echo through PipeWire, the PDF viewer of WebKitGTK in a tile, and the origins of the CSP
(`http://ipc.localhost` is the Windows spelling).

**Order**: `cargo check` on `ubuntu` in the CI, then the models on the CPU, then the window, then a package
(AppImage is the one the Tauri updater knows how to update).
