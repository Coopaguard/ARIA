# Feedback from real use, first round

What the first days of real use asked for, from November 2026. Ticked when merged; removed, like a todo page, once
released.

## Already released

- [x] 0.1.1: no console window for `claude` and the other processes it starts.
- [x] 0.1.1: installs in `%LOCALAPPDATA%\bin\ARIA`, no longer in the folder of the models.
- [x] 0.1.1: an icon of its own; `latest.json` gives the direct download link.
- [x] 0.1.2: updates wait for ARIA to close, and install themselves at start; messages in order; delegated sessions
      propose memories; delegation even when a detail is missing; ARIA's own words cut from the start of a turn;
      30 s of listening after an answer in named mode; the first-run screen; *tu* everywhere.

## In progress, for 0.1.3

- [x] **ARIA no longer heard, on an Intel card.** 0.1.1 downloaded Intel's runtime (OpenVINO) in the background, and
      the restart of the update loaded it: it refuses the listening model (`Attention`). Intel cards go back to
      DirectML, which ran it; and a card that refuses the model now hands it to the processor instead of leaving ARIA
      deaf.
- [x] **A logged-out Claude Code said nothing on screen.** A deaf ear or an agent that cannot start is now written
      under the orb; for an expired login, what to run, and a « Réessayer » button.
- [ ] **Session start-up.** About 7 s per delegated task, most of it loading connectors. To measure before choosing.
