# Other engines

The seam exists (`Capabilities`, `StaticPolicy`, `McpToolNaming`, `AgentProvider::models()`); Claude Code is the only real
engine. What each CLI can do is in [docs/cli/](../cli/): read from documentation, so **run it before writing**.

| | drive it through | asks per action | MCP per session | interrupt | names our tools |
|---|---|---|---|---|---|
| [Claude Code](../cli/claude-code-cli.md) | `stream-json` + relay | yes, not trivial shell | yes | yes | `mcp__server__tool` |
| [Codex](../cli/codex-cli.md) | app server | yes | to check | yes | `mcp__server__tool` (hooks) |
| [Gemini CLI](../cli/gemini-cli.md) | [ACP](../cli/acp.md) | yes | yes, at the handshake | yes | given apart in ACP |
| [Antigravity CLI](../cli/antigravity-cli.md) | ACP | to check: soft-denied under `-p` | through ACP only | to check | to check |
| [Mistral Vibe](../cli/mistral-vibe-cli.md) | ACP, or its app server | to check | to check | yes | to check |
| [opencode](../cli/opencode-cli.md) | ACP | yes | yes | yes | `server_tool` |

- **One ACP provider could drive three of them.** It is the cheapest second engine. opencode first: documented in detail,
  MIT, and any model behind it.
- **The comments in the code are out of date**: they say Claude Code alone can ask. It was true of the non-interactive
  modes, not of ACP nor of Codex's app server. `StaticPolicy` stays for an engine driven without them.
- Gemini: Antigravity CLI for an individual, Gemini CLI for an enterprise licence (since 2026-06-18).
- Writing ARIA's servers into the user's global file is not done behind their back.

To record by running each one: every action asked or not, MCP per session, the exact name of our tools, the models, and
what an interruption leaves.
