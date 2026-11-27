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
- [x] 0.1.3: hearing back on Intel cards (DirectML, the processor if a card refuses the model); a deaf ear or a
      logged-out Claude Code written under the orb, with « Réessayer ».

## In progress

- [x] **Two subjects, one task.** A cinema and a drive to Lyon, asked in one breath, went to one session as a
      `follows`, and its single answer was cut off aloud (« sans… »). The prompt now keeps `follows` for a precision
      and gives another subject its own task.
- [x] **How long a session speaks.** No fixed cut any more (300 characters): everything after « À DIRE : » is said,
      and sessions aim under two hundred words. Longer — a plan, its risks — goes to a tile, and questions are asked
      one at a time, each answer coming back to the same session.
- [x] **A result said in the middle of an answer.** A delegated result that came back while the agent was answering
      was said between two of its sentences: both shared one channel. It now waits for the answer to end and its
      speech to be out, 60 s at most.
- [x] **A long request lost but for its last words.** The minute a turn may last was counted from the end of the
      previous one, silence included: 36 s of an empty room, then a 25 s request, and the turn restarted in the
      middle of it; only « La troisième. » reached the agent. Silence before the first word is now dropped after 5 s.
- [x] **« Je ne peux lancer qu'une tâche ponctuelle. »** The voice post did not know a session can set up a
      check that repeats: `delegate_task` now says so.
- [x] **A result held a whole minute after a cut.** Cut off, ARIA stays `Interrupted` until a next turn, and a
      result waited behind it until the ceiling. Cut and silent for 3 s now counts as quiet.
- [x] **A session validated its own hook.** In `auto` mode the CLI approves calls without asking the relay, so
      `hook_approve` went through without a card. Validating and deleting are now asked by the core itself, and
      the card carries the script.
- [ ] **Hooks.** A script that checks something on its rhythm and wakes ARIA only when there is news: she follows
      the answer she is giving (« Au fait, … ») or opens. Validated by the user run by run: one run, their verdict,
      the next run, their verdict. Tried once live: the skill and the follow-up worked; to try again with a secret
      typed in the form ([hooks](docs/todos/hooks.md)).
- [ ] **Answer in the tile.** A tile with the five questions, answered there: the host refuses `ui/message` today.
- [ ] **Session start-up.** About 7 s per delegated task, most of it loading connectors. To measure before choosing.  (why load a subsession 

## Todo

- [ ] Gestion des langues (fr-en pour le moment puis liste des langue les plus parlés
- [ ] Démarrage auto avec le system (boolean settings + fenetre première ouverture)