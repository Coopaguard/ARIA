# Wake word

`crates/voice/src/wake.rs` runs `app/resources/wake/wake.onnx`, trained in a separate project on the
same log-mel as `crates/voice/src/mel.rs`. A new model replaces the file, without rebuilding; its
threshold and `k` are settings.

- **Not good enough alone**: about 16 false wakes per hour at threshold 0.95, k = 2, so the transcript
  confirms by default. Target: 95 % recall and at most one false wake in 8 hours.
- **Retrain without `kyutai-fr-cosette`**, a non-commercial voice that is in the shipped model's corpus.
- Other uses: settle a doubtful transcript at the right instant (`TimestampMode` of `parakeet-rs`), or
  replace **one** word with "Aria" where the detection overlaps it, never the sentence.
- Once it holds, **ASR int8** can be reconsidered: 1.88 GB less on disk and 34 % more throughput. Only
  the name ("A rien" for "Aria") ruled it out.
- `Config.name` can no longer be a free field once the trained name is the only one heard; the aliases
  still serve the transcript.
