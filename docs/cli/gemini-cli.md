# Gemini CLI (Google)

> **Read from the official sources on 2026-11-24, not by running the CLI.** Documentation describes the intention; running
> it describes the program. Run it before writing a provider.

Sources: [repository](https://github.com/google-gemini/gemini-cli) (Apache-2.0, 0.71.1):
[headless](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/headless.md),
[ACP](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/acp-mode.md),
[CLI reference](https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/cli-reference.md),
[hooks](https://github.com/google-gemini/gemini-cli/blob/main/docs/hooks/index.md),
[policy engine](https://github.com/google-gemini/gemini-cli/blob/main/docs/reference/policy-engine.md).

> **Choose the target first.** Since 2026-06-18, free individual access and Google AI Pro / Ultra moved to
> [Antigravity CLI](antigravity-cli.md) (`agy`); this page holds for enterprise licences and paid API keys.

## Two ways to drive it

| | `gemini -p` | `gemini --acp` |
|---|---|---|
| Protocol | `--output-format stream-json`: `init`, `message`, `tool_use`, `tool_result`, `error`, `result` | [ACP](acp.md), JSON-RPC over stdio |
| Asks for permission | no: policy and hooks only | **yes**: `session/request_permission` |
| Interrupt a turn | kill the process | `cancel` |
| Resume | `--resume latest`, an index or an id | `loadSession` |

The CLI reference still lists `--experimental-acp`; the ACP page says `--acp`. Exit codes of `-p`: 0, 1, 42 (input),
53 (turn limit).

## Permissions

- `--approval-mode default | auto_edit | yolo | plan`.
- Policy engine: TOML files in `~/.gemini/policies/`, each rule `allow`, `deny` or `ask_user` with a priority. MCP tools
  match as `mcp_<server>_*`.
- `BeforeTool` hook: allow or deny. **A hook that prints anything else than JSON counts as allow**, so it is not a safe
  relay on its own.

## MCP

`mcpServers` in `settings.json` (user or project), and in ACP mode the client gives its server at the handshake. In a
permission request, `toolCall._meta['gemini-cli'].mcp` carries `server_name` and `tool_name`: no name to parse.
`trust: true` on a server skips every confirmation: never set it. `--allowed-mcp-server-names` only restricts.

## Asking the user, read on 2026-11-27

`ask_user`: one to four questions, of type `choice` (the default, `multiSelect` for several), text, or yes/no, each
option with a label and a description. **Off under ACP** unless the client advertises
`clientCapabilities._meta.geminiCli.hostInput` with `requestUserInput: true`, Gemini's own extension; read from a
mirrored pull request. No answer path under `-p`. Sources: [ask_user](https://geminicli.com/docs/tools/ask-user/),
[ACP opt-in](https://github.xiaohuzi123231346.workers.dev/google-gemini/gemini-cli/pull/24664).

## Other

- `--model` takes a name or an alias (`auto`, `pro`, `flash`, `flash-lite`); `unstable_setSessionModel` in ACP.
- `GEMINI_SYSTEM_MD=<path>` replaces the system prompt; `GEMINI.md` adds to it.
- Sessions are stored per project, under `~/.gemini/tmp/<project_hash>/chats/`.
