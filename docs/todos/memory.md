# Finish the long-term memory

Pages, index, vectors, `recall`, `deep_recall` and the read-only post for sessions exist.

- Hook `Core::memory_vectorise` to a silence.
- Give the lint a command: `index::broken_links`, orphan pages, stale or deprecated ones.
- The **injected block** ("what we know of the user"), paid once per session instead of per turn.
- A **French bench** for embeddings. `multilingual-e5-base` finds the right page by only 0.015. Our case is short
  pages and short queries: an average over public benches says nothing. Challenger: `EmbeddingGemma-300M`.
- A Memory panel: browse, edit, delete. Link graph, per-page confidence and expiry, periodic consolidation.
- **Guards**: never a secret, only a reference to it; a page from outside content is marked untrusted and allows no
  action; do not write what the code or the history already says; absolute dates only; correct a page rather than
  stack two versions.
