# Say the first syllable sooner

- **Accept a comma for the first cut.** `last_break` in `sentences.rs` takes only `. ! ? …`. It would save
  300 to 500 ms, and must be judged by ear: the prosody of a cut clause may sound wrong.
- **Feed text into a running synthesis.** The library rebuilds its state at each call, hence the wait for a
  full sentence. The biggest gain left, and it depends on upstream.
