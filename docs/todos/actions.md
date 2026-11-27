# Actions: what ARIA does on the machine

Native actions the voice post can run. Waking ARIA from outside is a [hook](hooks.md), not an action.

**`run_action` is declared as synchronous and outgoing, and is not published.**

**The trap, and it is silent.** `mcp::voice::approve()` is a rule, not a list: ARIA's own tools are allowed. The
day an executor backs `run_action`, **every action is approved without asking**, "turn the sound up" and "empty the
bin" alike. `approve()` must then read the `action` argument, look its risk up, and pre-approve only `Risk::Low`; an
unknown action is refused. Write that test **before** the executor.

An `enum` of identifiers in the schema is the way to publish names (12 to 15 bytes each): a `list_actions` tool is
a round trip inside a spoken turn, and the system prompt is frozen at the first request.

Native candidates: volume, sleep, windows and tiles, opening an application or a URL, timers.
