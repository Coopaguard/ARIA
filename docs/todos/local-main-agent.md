# Local main agent: known limits

Gemma 4 E4B or Voxtral Mini 3B through `llama-server`, measured on 2026-11-18.

- End of turn rests on prosody alone: there is no punctuation to help.
- No word veto against echo: a turn heard while ARIA speaks is sent only if `echo::Live` called it an
  interruption, so one can be missed.
- Putting ARIA to sleep by voice does not work without a transcript.
- The first audio turn took 15.4 s at the microphone, against 140 to 300 ms later. The warm-up does not
  cover all of it — probably a Vulkan compilation for another audio shape.
- The audio prefill of a real turn is still to be measured.
- `[agent.local]` in `aria.toml` (`server`, `args`, `no_transcript`) has no screen.
- A local engine becomes a priority when an ordinary day uses more than half of the subscription window,
  or a ceiling is reached once a week.
