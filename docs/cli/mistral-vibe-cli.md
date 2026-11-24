# Mistral Vibe

> **Read from the official sources on 2026-11-24, not by running the CLI.** Documentation describes the intention; running
> it describes the program. Run it before writing a provider.

Sources: [repository](https://github.com/mistralai/mistral-vibe) (Apache-2.0, 3.1.5):
[programmatic mode](https://github.com/mistralai/mistral-vibe/blob/main/docs/programmatic-mode.md),
[ACP setup](https://github.com/mistralai/mistral-vibe/blob/main/docs/acp-setup.md),
[app-server protocol](https://github.com/mistralai/mistral-vibe/blob/main/docs/reference/app-server-protocol.md).
"Mistral Code" is the product around it; the command is `vibe`.

## Three ways to drive it

| | `vibe --prompt` | `vibe-acp` | app server |
|---|---|---|---|
| Protocol | `--output streaming`: one JSON per message | [ACP](acp.md) | its own JSON-RPC |
| Asks for permission | **no: a call that would ask is denied** | ACP's `session/request_permission`, to check | approvals in the protocol |
| Interrupt a turn | `--time-limit`, SIGTERM | `session/cancel` | `turn/interrupt` |
| Resume | `--resume <session id>` | to check | `session/resume` |

## Permissions

- Approval policies `ask`, `accept-edits` (default), `smart-approve`, `auto-approve` (`--yolo`).
- Per agent, in `~/.vibe/agents/<name>.toml`: `permission` and an `allowlist`. A chained command is denied unless every
  part is listed.

## Other

- `--model <alias>`, `--thinking <level>`: new sessions only.
- `--max-turns`, `--max-price`, `--max-tokens`, `--time-limit`.
- `--system-prompt-file` replaces Vibe's instructions; the harness guidance and `AGENTS.md` files stay.
- `--enabled-tools` **disables every other tool** in programmatic mode.
- `--output-dir` writes `export.json`: usage, cost, steps, outcome. Exit codes: 0 finished, 1 usage or configuration,
  2 infrastructure, 3 stopped by a limit, 4 aborted.
- On Windows it needs [uv](https://docs.astral.sh/uv/). `--agent-socket` does not work there.

## To check by running it

Whether ACP mode lists MCP servers per session; the name it gives our tools; whether ACP asks for every call.
