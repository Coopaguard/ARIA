# Custom voices

A Phonon voice is a `[1, 512, 125]` speaker latent, and **no published repository carries its encoder**.

Tried on 2026-11-22 (`crates/voice/examples/clone_voice.rs`):

- the encoder of `pocket-tts` has 32 dimensions where Phonon expects 512;
- Mimi (`kyutai/moshiko-pytorch-bf16`) has the right architecture and other weights: gender gets through,
  identity does not (similarity 0.03). Rescaled without recentring, the latent makes Phonon stutter.

Leads: mix existing Apache-2.0 latents and keep the closest, ask Gradium for the encoder, or another engine.
