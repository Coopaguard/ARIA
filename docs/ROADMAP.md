# Roadmap

What is left to do and what may come. **The order is not a priority.** A linked task has a page with
what is already known; delete both when the task is done.

## Releases

- [ ] Install on a machine without Claude Code: what the first-start screen says
- [ ] See a tag outside `develop`, or one that disagrees with the version files, refused
- [ ] Authenticode signing: without it SmartScreen announces an unknown publisher. Not planned yet
- [ ] [Ship the licence texts of the dependencies](todos/dependency-licences.md)

## To check in real use

- [ ] [Interruption, patience, follow-ups, tile refresh](todos/real-use-checks.md)
- [ ] [Cutting ARIA off is still slow](todos/barge-in-latency.md)

## Voice

- [ ] [Calibrate from the settings, echo included](todos/voice-calibration.md)
- [ ] [Wake word: retrain, and use it beyond waking](todos/wake-word.md)
- [ ] [Say the first syllable sooner](todos/speech-latency.md)
- [ ] [Say clearly what is degraded](todos/degraded-modes.md)
- [ ] [Local main agent: known limits](todos/local-main-agent.md)
- [ ] [Licences of the Phonon voices](todos/phonon-licences.md)
- [ ] [Try the 8-bit French voice](todos/phonon-q8.md)
- [ ] [Custom voices](todos/custom-voices.md)
- [ ] [Move `pocket-tts` to the official package](todos/pocket-tts-official.md)
- [ ] [Native speech-to-speech](todos/speech-to-speech.md)
- [ ] [Other languages, English first](todos/other-languages.md)

## Agent and security

- [ ] [Security guards that do not exist yet](todos/security-guards.md)
- [ ] [Approve by voice, and show what is approved](todos/voice-approval.md)
- [ ] [Give a delegated task its project folder](todos/task-working-directory.md)
- [ ] [The agent holds its own active / asleep state](todos/agent-held-state.md)
- [ ] [Actions: let the outside wake ARIA](todos/actions.md)
- [ ] [Cadence: scheduled tasks and watch](todos/cadence.md)
- [ ] [Connectors and skills](todos/connectors-and-skills.md)
- [ ] [Other engines: opencode, Gemini, Vibe, Codex](todos/other-engines.md)

## Memory

- [ ] [Finish the long-term memory](todos/memory.md)

## Interface

- [ ] [Embedded browser panel](todos/browser-panel.md)
- [ ] [Tiles: what the specification leaves open](todos/tiles.md)
- [ ] [Feature ideas](todos/feature-ideas.md)

## Maintenance

- [ ] [Linux](todos/linux.md)
- [ ] [GPU: AMD and macOS](todos/gpu.md)
- [ ] Run `cargo fmt --check` and clippy in CI, once the code passes them
- [ ] Shorten the CI: 2 h 11 without a cache, measured on 2026-11-23
- [ ] Measure the cost of permanent listening: the whole turn is transcribed again at each pause, so a long turn is quadratic
- [ ] Remove or write the two modules left empty: `orchestrator/`, `actions/`
