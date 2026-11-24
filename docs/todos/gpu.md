# GPU: AMD and macOS

ONNX Runtime is loaded from one DLL per stack: DirectML shipped, CUDA and oneAPI downloaded.

- AMD has no Windows build of ONNX Runtime (ROCm, MIGraphX), so it runs on DirectML, and the screen says so.
- Metal is not looked at; ARIA runs on Windows only ([Linux](linux.md) is wanted).
- Phonon's `cuda`, `metal` and `vulkan` features are off on purpose: they need nvcc and their own toolchains.
