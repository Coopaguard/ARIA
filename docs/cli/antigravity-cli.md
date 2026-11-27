# Antigravity CLI (Google)

> **Read from the official documentation on 2026-11-27, not by running the CLI.** Run it before writing a provider.

Sources: [headless](https://antigravity.google/docs/cli/headless/), [CLI reference](https://antigravity.google/docs/cli/reference/),
[ACP](https://antigravity.google/docs/cli/acp/), [MCP](https://antigravity.google/docs/mcp/),
[permissions](https://antigravity.google/docs/permissions/),
[the transition](https://github.com/google-gemini/gemini-cli/discussions/27274).

## Which of the two

Announced on 2026-05-19: from **2026-06-18**, [Gemini CLI](gemini-cli.md) stopped serving free accounts and Google AI
Pro and Ultra, which moved to `agy`. Gemini CLI stays for Code Assist Standard or Enterprise licences, Google Cloud and
paid API keys, Apache-2.0 and maintained for them. **For an individual user, the target is `agy`.** It is written in
Go; whether its code is open is not said. Configuration in `~/.gemini/antigravity-cli/`; extensions became plugins.

## Two ways to drive it

| | `agy -p` | `agy acp` |
|---|---|---|
| Protocol | `--output-format stream-json`: `init`, `step_update`, `result` | [ACP](acp.md), JSON-RPC over stdio |
| Several messages | **yes**: `--input-format stream-json`, `{"event":"user","message":{"content":"..."}}` per line, one `result` per turn; closing stdin ends it | sessions of ACP |
| Asks for permission | **no**: soft-denied, the run goes on and says so on stderr | not described on the ACP page |
| Resume | `--conversation <id>`, `--continue` | not described |

A `control_request` on stdin ends the session in error: there is no control channel. `step_update` carries
`step_type` (`user_input`, `agent_response`, `tool`, `checkpoint`), `text_delta`, `tool_info`, `usage`. `result.status`:
`SUCCESS`, `ERROR`, `CANCELED`, `INTERRUPTED`, `INVALID`, `WAITING`, `RUNNING`. `--json-schema` constrains the answer,
in `structured_output`.

## Permissions

- Rules `action(target)` in `permissions.allow` of `settings.json`: `command(git)`, `write_file(src/)`,
  `mcp(server/tool)`. Deny rules apply in every mode. An MCP tool not configured asks, so is denied headless.
- Modes `default`, `accept-edits`, `auto`, `plan` (`--mode`); `agy -p` runs in `auto` when available since 1.5.7.
  `--dangerously-skip-permissions` approves everything.
- **No external approver** is documented under `-p`: our relay would go through ACP, to check.

## MCP

Only in files: `~/.gemini/config/mcp_config.json`, or `.agents/mcp_config.json` in the workspace, key `mcpServers`;
stdio or Streamable HTTP (`serverUrl` or `url`, `headers`). **No flag per run**: a session's own URL would go through
ACP's `mcpServers` or the session folder's file. `timeoutSeconds`: 180 per call, 300 to connect.

## Other

- Models: `agy models`, `--model <slug>`, `--effort`; the documented list holds Gemini models only. An unknown model is
  an error, not a fallback.
- No tool to ask the user a question is documented.
- Not read: hooks, the full ACP guide, models.
