# Calibrate from the settings

Only the first step exists: background noise → `speech_rms`, stored per input device
(`app/src/voice/calibration.rs`).

| Value | Depends on |
|---|---|
| speech floor (`speech_rms`) | microphone, gain, room — **done** |
| silence that closes a segment (300 ms) | the person: a slow speaker gets cut at every comma |
| end-of-turn threshold (0.85) | how the voice falls on a suspended word |
| echo margin (8 dB) and coupling | speakers, volume, room, microphone |
| aliases of the name | voice, and how the ASR spells it |

**Order matters**: silence, then a sentence, then labelled utterances, then echo, then the name three
times. The end-of-turn threshold is measured through the energy gate, so a wrong gate gives a wrong
threshold.

- **Check each utterance as it is said**: nobody can produce a suspension on demand. If the transcript
  comes back punctuated, offer to redo that one.
- The shipped default works without calibration, and a calibration never makes it worse: outside a sane
  band, keep the default and say so.
- Two levels: six utterances (one minute), twenty later. The twenty are in
  `crates/voice/calibration/turn-set.tsv`.
- Show the trade-off ("I would have cut you off 1 time in 20"), not a number.
- Store **per input device**. Recordings stay on the machine, with a visible button to erase them.
- Later: when ARIA cuts someone who goes on at once, it is a labelled false positive. Count them, and
  offer to recalibrate.

**Echo, from the screen.** Today it is a command line tool (`crates/voice/src/bin/echo.rs`). Keep what it
learned: an absolute level beside the ratio (digital silence looks like a heard signal), a **constant
spectrum** and not a sweep (a sweep recommended 36 dB, which makes ARIA deaf), and a diagnosis naming the
three causes of `Absent`, the OS echo cancellation among them.

**Development tools.** Recordings go to `samples/` (never tracked: personal voice), or to `ARIA_SAMPLES`.

```bash
cargo run --release -p aria-voice --bin dictate -- --save   # record
cargo run --release -p aria-voice --bin turn -- --set       # read the end-of-turn set
```

`utterance-01.wav` … `utterance-20.wav` follow the lines of `turn-set.tsv`: **the order is the protocol**.
