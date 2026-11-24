# Agent Client Protocol

> Read on 2026-11-24 from [agentclientprotocol.com](https://agentclientprotocol.com) and the pages of each CLI. Not run.

JSON-RPC 2.0 over stdio between a client (an editor, or ARIA) and an agent. **Gemini CLI, Mistral Vibe and opencode speak
it**, so one ACP provider could drive the three.

| Need of ARIA | In ACP |
|---|---|
| ask before an action | the agent calls `session/request_permission`, with options of kind `allow_once`, `reject_once`… **An agent MAY ask**: whether it always does is its own business |
| give our MCP servers | `mcpServers` on `session/new` and `session/load` |
| follow the work | `session/update` notifications |
| interrupt | `session/cancel` |
| resume | `session/load` (replays) and `session/resume`, both optional capabilities |

Codex has its own protocol instead ([app server](codex-cli.md)), and Claude Code its stream and relay
([CLI](claude-code-cli.md)).
