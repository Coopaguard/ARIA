# Questions to the user, answered in a tile

Asked for on 2026-11-27: a session that needs five answers shows them as questions with choices, and the user answers
in the tile or aloud.

**Every CLI has its own tool, none is driven the same way** (read on 2026-11-27, [Claude Code §13](../cli/claude-code-cli.md),
[Codex](../cli/codex-cli.md), [Gemini](../cli/gemini-cli.md), [Vibe](../cli/mistral-vibe-cli.md),
[opencode](../cli/opencode-cli.md)): one to four questions with options, single or multiple choice, but answered
through the relay, the app server, an extension of ACP, or not at all when driven. MCP and ACP elicitation is the
standard, young, and reported cancelled under `-p` and `exec`.

So **a tool of ARIA's**, `ask_user`, on the MCP post every session already has:

- the shape all of them share: questions, a short header, options with a description, `multiple`;
- it shows the tile and **returns at once**; the answers come back to that session as a `follows`. No call held open,
  so no timeout to meet, and one behaviour for every engine that calls an MCP tool, local models included;
- aloud, the same questions one at a time;
- the native tools disabled where they exist.

**`ui/message`** is refused today for no recorded reason other than the rule of the tiles: closed until something
needs it ([`may_call_from_tile`](../../crates/core/src/tiles/mod.rs)). Opening it is the risk to weigh: a tile's
content is written by a session that may have read the web, and its message would speak as the user. Bounded: on a
click only, to the session that showed the tile only, never to the voice post.
