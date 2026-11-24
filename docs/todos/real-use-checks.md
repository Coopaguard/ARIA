# Checks in real use

Written and tested, never confirmed in a room:

- an interruption stops the turn of the CLI, and spares a turn that has called a tool;
- ARIA's echo no longer reaches the transcript. **Risk**: the gate lowers the frames it calls echo, so
  it can lose the user's voice if it is wrong;
- a breath or a puff near the microphone does not cut ARIA off;
- end-of-turn patience: 900 ms after a word that leads on, 500 ms after 4 s of speech, 900 ms after
  10 s, nothing for a short turn. The values are guesses to adjust by ear;
- the model uses `follows` by itself instead of opening another session;
- adding to a tile refreshes it. `refresh_tile` changes the body only, not the title or the files.

The echo margin (8 dB) was measured on one machine, one room, one pair of speakers. Elsewhere, check
in the log that the coupling reaches `Locked` with a plausible lag and a negative `coupling_db`, that
ARIA does not cut itself off on a long answer, and that a real interruption gets through.

An echo reduced to the name alone still wakes the mode. This is a choice: `is_self_echo` needs two
words, and "Aria" said over the voice is also how one interrupts.
