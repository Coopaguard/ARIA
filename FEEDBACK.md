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
- [ ] **Delegated sessions can add to the memory, under the main agent's supervision.** A session *proposes* a fact;
      the proposals come back with its result, and the main agent keeps the ones worth keeping. Today the memory is
      read-only to them, and the user had to insist before the main agent wrote what they had found.
- [ ] **Delegation is not always used.** The main agent sometimes works on a request itself instead of delegating it,
      which makes it slow. To be analysed on a complete trace of an exchange.
- [x] **Named mode listens for 30 s after an answer**, counted from when ARIA stops talking. It was three minutes.
- [x] **Onboarding, quickly: testers are waiting.** The five-step first-run screen (`FirstRun.vue`) existed but was
      never mounted, so a new user landed on the orb with no guidance. Mounted; to be seen on a real first start.
