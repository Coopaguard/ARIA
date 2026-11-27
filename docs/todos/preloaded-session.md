# A delegated session kept ready

Asked for on 2026-11-27. One `claude` process launched ahead, given the next ordinary task; another is launched as
soon as it is taken.

**What it costs**, measured ([CLI §12](../cli/claude-code-cli.md#12-a-session-kept-waiting-measured-on-2026-11-27-cli-21323)):
226 MB, 2 % of a core, **no quota** while nothing is sent. No warm-up turn: the MCP servers connect on their own.

**What it buys**: the connectors. A session spoken to at once starts its first turn with them `pending`; one that
waited 1.5 s has them all. The launch itself is 0.6 s.

- Ordinary tasks only: tier `standard`, the default engine, `~/.aria/subagents`. `expert`, a `project` or a
  `follows` launch as today. The flags are fixed at launch: `--model`, `--mcp-config` with the session's token,
  `--append-system-prompt-file`.
- **`AudioMode::Off` kills it**, and none is launched until ARIA leaves `Off`: switched off from the orb, ARIA is at
  its minimum.
- One that died, or failed its login, is not handed a task: it is launched again, and the task goes cold.
- To look at first: does the CLI add the connectors later in a turn that began without them?
