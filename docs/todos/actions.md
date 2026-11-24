# Actions: let the outside wake ARIA

Defined on 2026-11-13: an action is an **incoming hook**. Something outside produced a result, and hands it to
ARIA with the reason to wake it: a script that ends, a timer, another program. Nothing is written.

- Default reading: the payload is `(result, why)`, as a new `Told` beside `Returned` whose answer **is** spoken.
  The other reading — the hook starts a task again — is to be confirmed first.
- The open questions are about security: who may send, what it may trigger, how it is told from the user.
- **`run_action` is not this tool**: it is declared as synchronous and outgoing, and is not published.

**The trap, and it is silent.** `mcp::voice::approve()` is a rule, not a list: ARIA's own tools are allowed. The
day an executor backs `run_action`, **every action is approved without asking**, "turn the sound up" and "empty the
bin" alike. `approve()` must then read the `action` argument, look its risk up, and pre-approve only `Risk::Low`; an
unknown action is refused. Write that test **before** the executor.

An `enum` of identifiers in the schema is the way to publish names (12 to 15 bytes each): a `list_actions` tool is
a round trip inside a spoken turn, and the system prompt is frozen at the first request.

Native candidates: volume, sleep, windows and tiles, opening an application or a URL, timers.
