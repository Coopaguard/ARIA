# First release

How to publish is in [AGENTS.md](../../AGENTS.md#releasing).

- [ ] Copy `~/.tauri/aria.key` off the machine. GitHub never shows a secret again: losing the key cuts
      updates for every installed copy, with no way to change it remotely.
- [ ] `TAURI_SIGNING_PRIVATE_KEY` secret on the repository (checked on 2026-11-25: not set yet).
- [x] Repository public. The application reads `latest.json` without a token, so a private one would serve
      updates to nobody.
- [x] `models-1` release, with the French voice. Marked as a pre-release, so that it is never the latest one.
      Downloaded without an account on 2026-11-25.
- [x] `develop` protected, administrators included: pull request required, required check
      **`Typage et tests`**, no forced push.
- [ ] A pull request runs the CI green on this repository.
- [ ] Install on a **clean machine**: no development tools, no models, no Claude Code. To watch: the
      3.7 GB download, the French voice speaking without any token, the CPU floor
      (Haswell / Zen — an older CPU gets an illegal instruction and starts nothing).
- [ ] Release `v0.1.0`: installer and `latest.json` are on the release, no longer a draft.
- [ ] A tag outside `develop`, or one that disagrees with the version files, is refused.
- [ ] Release the next version: the installed copy **offers** it, installs it and restarts. This is
      the only end-to-end check of the update system.
- [ ] "Later", then Settings → "check": the offer comes back.
- [ ] Authenticode signing: without it SmartScreen announces an unknown publisher. Not planned yet.
