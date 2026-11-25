# AGENTS.md

Guidance for coding agents and contributors.

**ARIA** is a local voice assistant for Windows. What is left to do is in [docs/ROADMAP.md](docs/ROADMAP.md),
one page per task in [docs/todos/](docs/todos/). A task that is done leaves both.

| Folder | What it holds |
|---|---|
| `app/` | The Tauri application: configuration, shell, IPC, capture and speech |
| `crates/core/` | Everything that runs **without a window** |
| `crates/voice/` | Ear and voice: ASR, end of turn, wake word, echo, TTS. Separate because it drags ONNX; `core` must never depend on it |
| `crates/memory/` | The embedder of the memory, and nothing else |
| `ui/` | The front end: Vue 3, TypeScript, Vite |
| `docs/cli/` | What each agent CLI does. [Claude Code](docs/cli/claude-code-cli.md) was **measured**, the others read from documentation. Refer to it or measure again and re-date; never restate from memory |

"Decision N" and "epic N" in comments refer to a log kept by the maintainer and not published.

## Rules

- **The repository is public.** No name, address, personal path or voice recording. Tests use made-up names;
  commits use a GitHub `noreply` address.
- **Code, comments and documentation in English. What the user reads or hears is in French.** A global rename also
  catches displayed strings: read the neighbours of any French.
- **Never split a crate ahead of need.** The boundary that pays is "the core compiles without a window".
- Integration branch: `develop`, by pull request.
- After removing a folder, look at what its `.gitignore` was hiding before `git add -A`.

## Commands

From the repository root, never from `app/`:

```bash
npm install && npm --prefix ui install   # once
npm run dev                              # Vite, then the application
cargo test -p aria-core                  # no Tauri, fast
cargo test -p aria --lib                 # before aria-voice: app/build.rs drops ONNX Runtime
cargo test -p aria-voice
cargo test -p aria-memory
cargo check                              # the whole workspace
npm --prefix ui run check                # front-end types
```

`cargo run -p aria` alone shows a blank window in debug: it loads the Vite server. Vite alone renders the front end
(`/` and `/overview.html`) in the "core unreachable" state; the scenarios tab, in debug builds, shows it with data.

Ignored tests are not flaky: some run the real CLI and **consume subscription quota**, some load model weights, one
serves a tile for twenty seconds.

```bash
cargo test -p aria-core  --test bridge -- --ignored --nocapture
cargo test -p aria-core  --test relay  -- --ignored --nocapture --test-threads=1
cargo test -p aria-voice --lib speed   -- --ignored --nocapture   # audio seconds per compute second
cargo test -p aria-memory -- --ignored --nocapture                # real recall, in French
```

**Two build settings that matter.** `.cargo/config.toml` sets `target-cpu=x86-64-v3`: `xn` picks its SIMD kernels at
compile time, and without it speech is produced at half real time. The price is Haswell (2013) or Zen (2017) at
least. `RUSTFLAGS` overrides it and rebuilds everything, so do not set it in CI. And
`[profile.dev.package."*"] opt-level = 3` optimises dependencies in debug.

## Where things live

- `%LOCALAPPDATA%/aria`: derived, large, downloadable again — models (3.7 GB) and index. Resolved by **one** function,
  `aria_voice::models_root()`. Never `app_local_data_dir()` for a model, and never rename the model folders.
- `%LOCALAPPDATA%/bin/ARIA`: the program. Never `%LOCALAPPDATA%/ARIA`, Tauri's default: Windows ignores case, so it is
  the folder above. `app/installer.nsh` sets it; a 0.1.0 already installed stays where it is.
- `~/.aria`: what the user owns and opens — `memories/`, `AGENTS.md` and `subagents/AGENTS.md` (instructions, not this
  file), `db/aria.db`, `logs/` (seven days). Resolved in `app/`, never in the core.

## Decisions not to reopen by accident

**Agents**

- Rust core driving the **Claude Code CLI as a subprocess** (`stream-json`): the Agent SDK has no Rust version.
- **Permissions go through a relay** (`--permission-prompt-tool`); a pattern registry in the core decides alone for
  known commands and sets the risk. `bypassPermissions` is excluded for good. A request **never expires**.
- Four profiles — Plan, Strict, Normal, Autonomous — mapped to native modes **by the provider**.
- Every process is started through `aria_core::process`, which hides its console; a test refuses any other way.
- A provider declares its `Capabilities`; an engine without a relay gets a `StaticPolicy`, and exactly one of the two.
  Tool naming goes through `McpToolNaming`.
- Models are chosen **by tier** (`rapide`, `standard`, `expert`), as a loose string. A delegated task runs at
  `standard` at least. A model set by hand is never replaced.
- Claude Code is the default main agent; Gemma and Voxtral are the local ones that hear audio. Kept from measurement:
  `--reasoning off`, the copied timestamp removed, a tool call written as text caught. No lazy loading.
- The voice post answers alone or delegates. A result comes back as `Told::Returned`, already spoken. **Timestamps go
  on each message, never in the system prompt**, which is frozen at the first request.
- A follow-up goes to the same session (`delegate_task.follows`); a session answers its tasks in order.
- Three MCP posts: relay and tiles, voice, and a **read-only** memory post for sessions. What ARIA remembers is decided
  in the conversation. Sessions load the user's connectors; the voice post does not.
- Budgets, held by tests: tool catalogue 3150 bytes sent per turn, system prompt 1600.
- The voice post runs in `~/.aria`, sessions in `~/.aria/subagents`; a `CLAUDE.md` above would change the answers, and
  `claude_md_above` checks for it.

**Voice**

- Cutting ARIA off has two signals, and the fast one **never cuts on energy alone**: until the echo gate has learned
  the room, it cuts nothing.
- ARIA does not hear itself: `echo::SpokenLog` vetoes its own words, except when the coupling is `Absent`. The margin is
  8 dB, measured. The veto comes **before** the name, enforced by the type `voice::Allowed`.
- An interruption stops the CLI's turn, not a turn that called a tool, and not delegated sessions. STOP cuts everything.
- A breath is not a voice: cutting needs voiced frames, and a turn needs words.
- End-of-turn patience slows down, and never waits forever.
- Phonon is the only voice offered. The French model offers the 15 French voices, the multilingual one the 87 others.

**Tiles**

- MCP Apps (SEP-1865), pushed by a session, sandboxed on port **7420**, a distinct origin pinned by the CSP.
- Framing is governed by `frame-ancestors`, **never `X-Frame-Options`**: `DENY` refuses the window too.
- Adding to a tile refreshes it. `show_tile` and `refresh_tile` are the only tools answered without asking.

**Storage**

- The thread is **our journal**, replayed into a new session: the voice post keeps no CLI transcript. Changing thread
  changes process. A failed write never fails a turn.
- A session is disposable unless `project` is set. Retention is thirty days per line; `expired` is set after a failed
  resume, never computed; **the audit is never swept** and has no foreign key.
- `aria.db` is the only copy of nothing, uses WAL, and a failure to open is logged and shown, never fatal.

**Interface.** Each was learned by breaking something.

- One implementation per component.
- Nothing painted reaches the edge of the orb's canvas.
- `backdrop-filter` only on a rounded shape.
- The tile layer is inert to the mouse, and a tile's iframe is inert during a drag.
- Nothing important goes in a truncated container.

## Releasing

The CI (`ci.yml`) runs on every pull request, on Windows. Its job `Typage et tests` is the check to require on
`develop`: do not rename it. The steps run in `bash`, since PowerShell only reports the last command.

1. Set the version in `app/tauri.conf.json`, `Cargo.toml`, `Cargo.lock`, `package.json`, `ui/package.json` and both
   lock files; merge into `develop`.
2. Tag a commit of `develop`:

   ```bash
   git fetch origin && git tag v0.2.0 origin/develop && git push origin v0.2.0
   ```

`release.yml` refuses a tag outside `develop` or one that disagrees with the version files, builds the signed NSIS
installer and `latest.json`, adds `SHA256SUMS` and the attestation, then publishes. A refused tag stays: delete it
(`git push origin :refs/tags/v0.2.0`) before another. A failed publication is run again from the Actions page.

- **Signing key** (minisign): `~/.tauri/aria.key`, no password, and the secret `TAURI_SIGNING_PRIVATE_KEY`; the public
  half is in `app/tauri.conf.json`. Losing it ends updates everywhere; leaking it lets someone sign a false one.
- A local `npm run build` signs too: `$env:TAURI_SIGNING_PRIVATE_KEY = Get-Content ~/.tauri/aria.key -Raw`.
- The application checks at start and every six hours, **offers** the update, verifies its signature and restarts.
- Shipped: the program, `resources/wake`, `resources/voices`, `resources/phonon`, ONNX Runtime. Downloaded: models from
  Hugging Face, the French voice model from our release **`models-1`**, CUDA (1.3 GB, when enabled). Installed by the
  user: Claude Code, `llama-server`.
- **Two kinds of release.** `vX.Y.Z` holds the program, downloaded again at each update: nothing heavy goes in it.
  `models-N` holds data, never changes once published, and is **published as a pre-release**: the update check reads the
  latest one. A new model is `models-N+1`.
- ARIA is distributed **free of charge**: a condition of the French voice and of the wake-word models.
