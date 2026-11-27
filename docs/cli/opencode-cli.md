# opencode

> **Read from the official sources on 2026-11-24, not by running the CLI.** Documentation describes the intention; running
> it describes the program. Run it before writing a provider.

Sources: [repository](https://github.com/anomalyco/opencode) (MIT, 1.19.6; formerly `sst/opencode`),
[CLI](https://opencode.ai/docs/cli), [ACP](https://opencode.ai/docs/cli/acp),
[permissions](https://opencode.ai/docs/permissions), [MCP](https://opencode.ai/docs/mcp-servers). The current pages
describe V2; V1 uses other field names (`permission`, `bash`, `task`).

## Two ways to drive it

| | `opencode run` | `opencode acp` |
|---|---|---|
| Protocol | text, for scripts | [ACP](acp.md) version 1, newline-delimited JSON-RPC over stdio |
| Asks for permission | not looked at | **yes**: `allow_once`, `allow_always`, `reject_once`, with a diff when it can |
| Interrupt a turn | kill the process | `session/cancel`; pending permission requests are rejected |
| Resume | `--continue`, `--session <id>` | `session/load` replays, `session/resume` does not |

`opencode acp` starts a **private** server and opens no network port. One process serves many sessions, and exits when
stdin closes. Without it, clients share one background server.

## Permissions

Ordered rules `{ action, resource, effect }` in `opencode.jsonc`, `effect` being `allow`, `deny` or `ask`; the last match
wins, and **no match means `ask`**. By default, paths outside the project are allowed without asking
(`external_directory`): add a rule.

## MCP

- The ACP client passes stdio and HTTP servers on `session/new`, `load`, `resume` and `fork` — **so per session**, though
  kept for the directory as long as the process lives. No SSE.
- A tool is named **`<server>_<tool>`**, unsupported characters becoming `_`: not the `mcp__server__tool` of Claude Code.

## Asking the user, read on 2026-11-27

`question`: questions with a short `header` and options, `multiple: true` for several, a free answer by default;
rejecting one stops the session. An issue says an ACP client cannot offer the same thing. Third-party notes, not the
official pages. Sources: [the tool's text](https://gitcode.com/openharmony-sig/deveco-code/blob/develop/packages/opencode/src/tool/question.txt),
[issue 30343](https://github.com/anomalyco/opencode/issues/30343).

## Other

- Models as `provider/model`; in ACP, `session/set_config_option` with `model`, `effort` or `mode`.
- Reports tokens and cost in USD. A session runs one prompt at a time, and **ignores audio**.
- Child sessions report on the parent's session, permission requests included.
- `opencode auth login` signs in, once, in a terminal.
