# Feedback from real use, first round

What the first days of real use asked for, from November 2026. Ticked when merged; removed, like a todo page, once
released.

## Already released

- [x] 0.1.1: no console window for `claude` and the other processes it starts.
- [x] 0.1.1: installs in `%LOCALAPPDATA%\bin\ARIA`, no longer in the folder of the models.
- [x] 0.1.1: an icon of its own; `latest.json` gives the direct download link.

## In progress, for 0.1.2

- [x] An update no longer aborts with "Failed to kill ARIA": the installer waits for ARIA to be gone.
- [x] **Messages in order.** On screen, top to bottom: ARIA's line before the user spoke (faded), the user's sentence,
      ARIA's answer in amber. Today the user's sentence is above both of ARIA's lines, even the one that came before it.
- [x] **Delegated sessions can add to the memory, under the main agent's supervision.** A session *proposes* a fact;
      the proposals come back with its result, and the main agent keeps the ones worth keeping. Today the memory is
      read-only to them, and the user had to insist before the main agent wrote what they had found.
- [x] **Delegation is not always used.** Analysed on a 50-minute trace: thirteen tasks were delegated, and the main
      agent answered in 1.5 to 5 s. Twice it asked the user for a detail a session could have found; its prompt now
      says to delegate anyway. The slowness is the sessions: about 7 s to start, with every connector loaded.
- [x] **ARIA acted on its own words.** On a headset, turns opened with ARIA's last answer, mangled, and three were acted
      on. The opening that repeats ARIA is now cut, whatever the echo gate concluded; what the user adds is kept.
- [ ] **Session start-up.** About 7 s per delegated task, most of it loading connectors. To measure before choosing.
- [x] **Named mode listens for 30 s after an answer**, counted from when ARIA stops talking. It was three minutes.
- [x] **Updates install themselves at start.** A newer version found when ARIA starts is installed without asking;
      one found later, every six hours, is still only offered, so a running task is not cut.
- [x] **Onboarding, quickly: testers are waiting.** The five-step first-run screen (`FirstRun.vue`) existed but was
      never mounted, so a new user landed on the orb with no guidance. Mounted; to be seen on a real first start.
