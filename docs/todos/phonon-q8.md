# Try the 8-bit French voice

Gradium publishes `model.q8.gguf` beside the weights ARIA uses: 74.6 MB instead of 121.7 (seen on 2026-11-25, never run).

- `ptts` 0.5 keeps q8 GGUF weights quantized **on the CPU** (`ResolveOptions { quant: Quant::Q80, .. }`); ARIA loads
  f32 weights through `Synth::builder`.
- Compare by ear, then the first chunk and the audio seconds per compute second (`speed` test), and the memory.
- If kept: a `models-2` release, since `models-1` never changes.
- There is also `model.q4k.gguf`, 60.4 MB.
