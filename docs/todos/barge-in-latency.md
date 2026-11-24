# Cutting ARIA off is still slow

Observed on 2026-11-23: ARIA says two or three more sentences after an interruption. The fast path
(`echo::Live`, 60 ms) does not fire, and the slow one waits for a transcript.

The log has the diagnosis, at info level, after each sentence: **`porte d'écho, fin de phrase d'ARIA`**
— coupling, margin, frames judged, echo, barge, and the median, p90 and max of the excess.

- Coupling stays `Unresolved` → the lock is at fault.
- `Locked`, with a max excess under the margin while the user spoke → lower the margin.
- The excess never separates voice from echo → real echo cancellation, or cut on the first words.

Never cut on energy alone: ARIA would silence itself on its own voice.
