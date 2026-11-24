# Codex CLI (OpenAI)

> **Read from the official sources on 2026-11-24, not by running the CLI.** Documentation describes the intention; running
> it describes the program. Run it before writing a provider.

Sources: [repository](https://github.com/openai/codex) (Apache-2.0, 0.172.0),
[non-interactive mode](https://developers.openai.com/codex/noninteractive),
[hooks](https://developers.openai.com/codex/hooks), [app server](https://developers.openai.com/codex/app-server),
[MCP](https://developers.openai.com/codex/mcp).

## Two ways to drive it

| | `codex exec` | `codex app-server` |
|---|---|---|
| Protocol | `--json`: one JSON event per line on stdout | JSON-RPC, requests both ways |
| Asks for permission | through a `PermissionRequest` hook | **yes**: the server sends requests to the client |
| Interrupt a turn | kill the process | `turn/interrupt`; the turn ends with `status: "interrupted"` |
| Resume | `codex exec resume <id>`, `--last` | `thread/resume` |

**The app server is the way for ARIA**: it is the only one that asks and interrupts.

## Permissions

- App server: `item/commandExecution/requestApproval` (`command`, `cwd`, `reason`), `item/fileChange/requestApproval`,
  `item/permissions/requestApproval`.
- Hook `PermissionRequest`: runs when Codex is about to ask; answers allow or deny, or declines to decide. Any deny wins.
  It does **not** run for commands that need no approval. Hooks need to be trusted;
  `--dangerously-bypass-hook-trust` skips that for one run.
- `PreToolUse` hooks, `.rules` files of the execution policy, and a sandbox: `read-only` (default of `exec`),
  `workspace-write`, `danger-full-access`. `--full-auto` is no longer supported.

## MCP

In `~/.codex/config.toml`, or a **trusted** project's `.codex/config.toml`. A server marked `required = true` that fails
makes `exec` exit with an error. Hooks see our tools as `mcp__server__tool`, the spelling of Claude Code.

## Other

- Events of `exec --json`: `thread.started`, `turn.started`, `turn.completed` (with `usage`), `turn.failed`, `item.*`, `error`.
- `--output-schema <file>` constrains the final answer to a JSON Schema: a possible replacement for our marked
  "À DIRE" line.
- `--model`, `--ephemeral` (nothing written), `--ignore-user-config`, `--skip-git-repo-check`, `-o <file>`.
- Instructions: `AGENTS.md`.

## To check by running it

Passing MCP servers per session (configuration overrides on `exec`, `config` on the app server); the exact name of our
tools in the event stream; the list of models.
