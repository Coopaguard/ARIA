# Move `pocket-tts` to the official package

Kyutai Pocket TTS is linked but **not offered on screen**. The dependency is a pinned revision of
[a public fork](https://github.com/Coopaguard/pocket-tts), which adds a library target, two features
(`cli`, `sentencepiece`) and a cache fix. Pushing the fork belongs to the maintainer.

**When**: upstream has a library target and drops `publish = false`. Its name will change, since
`pocket-tts` is taken on crates.io: keep our key with `package = "<published name>"`.

**Check before switching**

1. The Hub cache is still set by hand (`download::set_cache_dir`): `hf-hub` 1.0 reads no environment
   variable, and would download 400 MB in the wrong place without a word. Test:
   `the_library_is_pointed_at_the_directory_the_subprocess_filled`.
2. It remaps `flow_lm.bos_before_voice` and reads the primed voice cache. Otherwise it makes 1.6 s of noise
   whatever the text. Make it speak: no test catches this.
3. `grep -c '^name = "ptts"' Cargo.lock` is 1: two `ptts` are two `xn`, and tensors cross crates.
4. `default-features = false` still drops axum, clap and above all `sentencepiece`, which needs CMake.
5. The streaming callback is still `+ Send`: the decoder runs on its own thread.
6. **Can text be added to a running generation?** See [speech latency](speech-latency.md).

```bash
cargo test -p aria-voice
cargo run -p aria-voice --bin say -- --kyutai "Oui."
cargo run -p aria-voice --bin say -- --kyutai "Bonjour. Je suis Aria, et je parle français."
```

The length of the audio must follow the text.

**Known**: four low-rank configurations (`english_2m_*`, `english_7m_*`) need SentencePiece and were never
heard. `xn` is part of the public API, so a major version of it breaks ARIA too. One instance per thread.
