# Ship the licence texts of the dependencies

The installer links about 800 crates, mostly MIT, Apache-2.0 and BSD: their texts must travel with
the binary. [THIRD-PARTY-NOTICES.md](../../THIRD-PARTY-NOTICES.md) covers models and main engines only.

- Generate the list with `cargo-about` at build time, and the same for the npm dependencies; put it
  in `bundle.resources` and link it from the settings.
- 22 crates are MPL-2.0 (Symphonia, cssparser, selectors…): used unmodified, sources on crates.io.
- The terms of DirectML and of the CUDA redistributables were not checked at their source.
- openWakeWord's README puts its pre-trained models under CC BY-NC-SA 4.0. Ours are the two feature
  models, unmodified: see `app/resources/wake/NOTICE.txt`.
