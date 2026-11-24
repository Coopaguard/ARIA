# Claude Code CLI

Verified facts about what the CLI does. **§2 to §7 were read from the official documentation on 2026-10-20; §8 to §11
were measured by running it.** The CLI moves fast: check again, and re-date, before relying on them.

Sources: [permission modes](https://code.claude.com/docs/en/permission-modes),
[CLI reference](https://code.claude.com/docs/en/cli-reference),
[auto mode configuration](https://code.claude.com/docs/en/auto-mode-config).

> This page holds **no ARIA decision**, only what the CLI does. The decisions are in [AGENTS.md](../../AGENTS.md).
> Section numbers are cited in the code: do not renumber.

## 1. The Claude Agent SDK has no Rust version

It is published in **Python** and **TypeScript** only. The Rust core therefore drives the CLI as a subprocess.

## 2. The six permission modes

`--permission-mode` accepts exactly `default`, `acceptEdits`, `plan`, `auto`, `dontAsk`, `bypassPermissions`. `manual`
is an alias of `default`.

| Mode | Passes without asking | Still asks or blocks |
|---|---|---|
| `default` / `manual` | Reads | Edits, shell commands, network access |
| `acceptEdits` | Reads, edits, and common file commands (`mkdir`, `touch`, `mv`, `cp`, `rm`, `sed`) **in the working directory** | Other shell commands, writes to protected paths, deletion of critical paths |
| `plan` | Reads and exploration | Any edit, until a plan is approved |
| `auto` | Almost everything — **a classifier reviews each action before it runs** | What the classifier blocks, explicit `ask` rules, deletion of critical paths |
| `dontAsk` | Reads in working directories, pre-approved tools | **Nothing asks: what would have asked is refused** |
| `bypassPermissions` | Everything, unchecked | `ask` rules, interactive tools, deletion of critical paths |

**`auto` is not `bypassPermissions`.** It keeps the classifier, the protected-path checks (`.git`, `.claude`, `.bashrc`)
and the `ask` rules. On entering `auto`, broad allow rules that grant arbitrary execution (`Bash(*)`, `Agent`) are
**removed**; narrow ones such as `Bash(npm test)` stay.

`deny` rules block in **every** mode, `bypassPermissions` included, and are evaluated **before** the classifier.

## 3. Permission rules

Two layers, in this order.

### Pattern rules — `permissions.allow` / `ask` / `deny`

Tool patterns: `Bash(git log *)`, `Bash(grep *)`, `Read`. Evaluated before the classifier.

- `deny` blocks unconditionally, in every mode.
- `ask` forces a request, **even in `auto`**, and the classifier cannot approve it.
- `allow` passes without asking.

**Limit of shell patterns**: a pattern matches the command *as written*. `Bash(git push *)` covers neither
`git -C <dir> push` nor `git -c <key>=<value> push`. A check that must see the whole command goes through a
`PreToolUse` hook.

### Classifier rules — `autoMode`

Four lists of rules **in prose**, read by the classifier of `auto`: `environment` (what is trusted: repositories,
buckets, domains), `allow`, `soft_deny` (which an explicit intention of the user can lift) and `hard_deny`
(unconditional).

The literal string `"$defaults"` in a list inserts the native rules; **a list without it replaces the native one
entirely**. `claude auto-mode defaults` prints the native rules, `claude auto-mode config` the effective ones.

The classifier does **not** read `autoMode` from a repository's `.claude/settings.json` — a cloned repository could
otherwise authorise itself. Only `~/.claude/settings.json`, managed settings, and `--settings`.

### Where ARIA writes its rules

`--settings` accepts **a file path or an inline JSON string**, and wins over `settings.json` for the session. ARIA can
compose its rules per session without touching the user's file.

## 4. The permission relay

`--permission-prompt-tool mcp__<server>__<tool>` names an MCP tool that answers authorisation requests in
non-interactive mode.

**Three hard constraints:**

1. **Reads never go through the relay.** In every mode, reads of files in the working directories run without asking.
   A read perimeter can therefore not be applied from the relay: it is expressed in `permissions.deny`, or it does not
   exist.
2. **The relay cannot approve an MCP tool marked "requires user interaction"**: an `allow` on such a tool is turned into
   a refusal.
3. **Deletions of critical paths are refused** wherever no terminal is available, which is ARIA's case. No interface can
   allow them.

Claude Code waits for the relay's MCP server before the first turn, within `MCP_TIMEOUT` (30 s by default).

`--permission-prompts none` refuses requests instead of asking, for runs where nobody can answer.

## 5. Sessions: launch, resume, limits

```
claude -p --input-format stream-json --output-format stream-json --verbose
```

`--verbose` is mandatory with `--output-format stream-json`. The protocol is one JSON object per line on stdin and
stdout.

| Flag | Effect |
|---|---|
| `--session-id <uuid>` | Imposes the session identifier; must be a valid UUID |
| `--resume <id or name>` | Resumes a session. Also accepts the absolute path of the `.jsonl` transcript |
| `--fork-session` | On resume, creates a new identifier |
| `--replay-user-messages` | Emits again on stdout the messages received on stdin, as an acknowledgement. Needs both `stream-json` formats |
| `--include-partial-messages` | Includes partial streaming events |
| `--max-turns <n>` | Caps the number of turns; exits with an error at the limit |
| `--max-budget-usd <n>` | Stops the session at this estimated amount. `-p` only; the estimate is not the bill |
| `--add-dir <path>` | Additional working directories. **Grants file access** |
| `--settings <json or path>` | Session settings; win over the files |
| `--no-session-persistence` | The session is not written to disk and cannot be resumed |

## 6. The native supervisor of background sessions

The CLI has its own manager of detached sessions: `claude --bg`, `claude agents --json`, `claude attach`,
`claude logs`, `claude stop`, `claude respawn`, `claude rm`, and a daemon (`claude daemon`).

**ARIA does not use it**: `--bg` is incompatible with `-p`, and `claude logs` returns recent output, not an event stream
that could drive an interface and a voice. To reconsider if ARIA ever launches detached tasks it does not follow live.

## 7. Retention on the CLI side

`cleanupPeriodDays` in `~/.claude/settings.json`: an integer, **30 by default**, at least 1. It covers sessions, orphan
sub-agent worktrees, tasks, shell snapshots and backups. The sweep runs **when the CLI starts**, and counts from last
use, not from creation ([official schema](https://www.schemastore.org/claude-code-settings.json)).

The setting belongs to the user and can be 1 day: never deduce from a date that a session can be resumed. Try, and
handle the failure.

`claude purge` removes all the local state of a project. A purged session cannot be resumed.

## 8. The spoken turn: flags and stream, **measured** on 2026-10-24

From running `claude --help` on CLI **2.1.165** and a measuring bench since removed. Windows 11, native `claude.exe`.

### Flags useful to a conversation

| Flag | Observed effect |
|---|---|
| `--model <alias or name>` | **`haiku` resolves to `claude-haiku-4-5-20251001`**, the previous generation. The full name `claude-haiku-5-5` is accepted and honoured |
| `--effort low\|medium\|high\|xhigh\|max` | Accepted. **Does not switch reasoning off**: on Haiku 4.5, `--effort low` left 1.5 s of `thinking_delta` before the first word |
| `MAX_THINKING_TOKENS=0` (environment) | Strongly reduces reasoning — 220 output tokens drop to 55 — **without removing it**: 231 reasoning tokens remain over nine turns |
| `--system-prompt <text>` | Replaces the default system prompt. 345 to 438 input tokens in all, so the coding agent's prompt is gone. `--exclude-dynamic-system-prompt-sections` is then **ignored** |
| `--tools ""` | Disables every native tool: `init` returns `"tools":[]` |
| `--strict-mcp-config` | Without `--mcp-config`, **no MCP server is loaded**: `"mcp_servers":[]`. Without this flag the user's connectors are loaded |
| `--bare` | Minimal mode, but **authentication becomes strictly `ANTHROPIC_API_KEY` or `apiKeyHelper`; OAuth and the keychain are never read** |
| `claude setup-token` | Long-lived token, "requires Claude subscription" |
| *no* `claude usage` | Usage is read in the `result` event, or with `/usage` in an interactive session |

### The stream as it arrives

**One process holds several turns.** Ten turns sent one after the other on stdin, one process, context kept.

One input message per line:

```json
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"…"}]}}
```

Output event types: `system` (subtypes `init`, `status`, `thinking_tokens`), `stream_event` — wrapping the API's
streaming events (`message_start`, `content_block_start`, `content_block_delta`, `message_stop`) — and `result`.

| Field | Where | Content |
|---|---|---|
| `ttft_ms` | `stream_event`/`message_start`, and `result` | Time to first token |
| `duration_ms`, `duration_api_ms`, `time_to_request_ms` | `result` | Breakdown of the turn |
| `usage` | `result` | `input_tokens`, `output_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`, `iterations[]` |
| `modelUsage` | `result` | Per model, with `contextWindow` and `maxOutputTokens` |
| `total_cost_usd` | `result` | **Does not reconcile** with the counters for `claude-haiku-5-5`: about 10× above the published rate. The token counters are coherent |
| `stop_reason`, `terminal_reason`, `permission_denials` | `result` | End of turn |

### Measured latencies

`claude-haiku-5-5`, ~400-token system prompt, `MAX_THINKING_TOKENS=0`, **30 turns** in French in one session (29 warm).

| | |
|---|---|
| Session start | 870 ms to `init` |
| First text, warm | **median 1,113 ms, p90 1,413 ms**, worst turn 1,595 ms |
| Whole turn, warm | median 1,298 ms, p90 1,518 ms |
| Reported `ttft_ms` | median 1,271 ms, p90 1,480 ms |
| Cache | **effective**: 41,850 tokens read, 2,494 written, only **18 new input tokens per turn** |
| Output | 53 tokens per turn |
| `inference_geo` | `global` |

> **Method.** A first series of ten turns gave a p90 of 2,800 ms. On nine warm turns, the p90 **is** the slowest one. A
> latency measured on fewer than about twenty samples measures noise.

## 9. The relay, **measured** on 2026-10-25, **replayed** on 2026-11-13

> **Checked again on 2026-11-13, CLI 2.1.323** (`cargo test -p aria-core --test relay -- --ignored --test-threads=1`, and
> `--test bridge`). The relay's tool is found, an approval lets through, a refusal stops, two parallel sessions do not
> cross their requests, and a session resumed in a **new process** remembers. Three more facts:
>
> - **a delegated session sees 83 tools**, the user's claude.ai connectors included, since `--strict-mcp-config` is not
>   passed to it. Wanted, and paid in context by each session;
> - two of these connectors return `needs-auth`, and the session starts anyway;
> - **the CLI refused a request by itself** ("the CLI refused 1 request(s) on its own").
>
> MCP protocol version announced by the CLI: `2026-07-28`.

Measured on CLI **2.1.165**, with ARIA's MCP server over HTTP on `127.0.0.1:7421`. The official page of the flag is thin
([anthropics/claude-code#1175](https://github.com/anthropics/claude-code/issues/1175)) and third-party sources disagree.

### The inline configuration, as the CLI writes it

`claude mcp add --transport http <name> <url>` then `claude mcp get <name>` print `Type: http`, `URL: …`. `--transport`
accepts exactly **`stdio`, `sse`, `http`**. Guides that write `"transport"` instead of `"type"`, or `streamable-http`,
do not match. The payload accepted by `--mcp-config`:

```json
{"mcpServers":{"aria":{"type":"http","url":"http://127.0.0.1:7421/mcp?s=<token>"}}}
```

**An inline JSON string is enough**, and **no approval is asked**, unlike a server declared in a project's `.mcp.json`.

### The status in `init` is transient

`init` returns `"mcp_servers":[{"name":"aria","status":"pending"}]`, **then** the server becomes `connected`. Reading the
`name` alone and concluding "connected" is wrong.

### The relay's answer

The tool returns a **text content block whose text is a JSON object**:

```json
{"behavior":"allow","updatedInput": { … }}
{"behavior":"deny","message":"…"}
```

Verified: a `deny` produced a `tool_result` with `is_error: true` and our message, **the file was not created**, and
`result` recorded the action in `permission_denials` with its `tool_name`, `tool_use_id` and full `tool_input`.

### What the relay does not see

Measured in **`default`** (ARIA's Strict profile):

| Action | Goes through the relay? |
|---|---|
| `Write` to a file | **yes**, always |
| `Bash(echo …)` | **no** — run without asking |

§2 describes `default` as asking for shell commands. For **trivial ones it does not**: the CLI clears some before
consulting the relay. The pattern registry therefore sees fewer actions than expected, and some are approved upstream,
outside our audit. An accepted limit.

### An undocumented event: `rate_limit_event`

```json
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning",
 "rateLimitType":"five_hour","utilization":0.9,"surpassedThreshold":0.9,
 "resetsAt":…,"isUsingOverage":false}}
```

`utilization` gives the state of the sliding window as it goes, without `/usage`.

## 10. The voice agent in a persistent session, **measured** on 2026-10-26, **replayed** on 2026-11-13

> **Replayed on 2026-11-13, CLI 2.1.323** (`cargo test -p aria-core --test agent --test pilot -- --ignored`). Three turns
> in one process, French without Markdown, and the identity rule **holds**: asked about its user, the model answers that
> it knows neither name nor address. The context below still arrives; the model no longer uses it.
>
> First text **1.34 s** on the first turn, **483 ms** and **541 ms** next; whole turn 1.63 s then ~800 ms. Input 2 tokens,
> cache 779 then 1,507, output 33 to 89.
>
> **Closing stdin does not make the CLI exit.** `MainAgent::stop` waits five seconds then kills it. This is the normal
> path, not a leak.

Measured on CLI **2.1.165**: three turns in one process, `claude-haiku-5-5`, `--effort low`, `MAX_THINKING_TOKENS=0`,
voice system prompt.

### The flags, checked again

`--exclude-dynamic-system-prompt-sections`, `--setting-sources`, `--disallowedTools`, `--tools`,
`--no-session-persistence`, `--effort`, `--model`: all present in `claude --help` at this version.

### The stream carries the state of the quota, and nothing else does

`rate_limit_event` arrives **unasked**, mid-turn. Useful fields: `utilization` (0 to 1), `rateLimitType` (the window),
`surpassedThreshold` (the threshold just crossed) and `isUsingOverage`.

### Latency: three turns, which do not contradict §8

| Turn | First text | Input | From cache | Output |
|---|---|---|---|---|
| 1 | 2,403 ms | 475 | 0 | 67 |
| 2 | 1,672 ms | 4 | 0 | 71 |
| 3 | 1,932 ms | 4 | 558 | 32 |

Three turns do not compare with twenty-nine. The cache column is clear though: it bites only on the third.

### `system/init` comes back on every turn

The CLI emits it at each turn, with the **same** `session_id`. Code that treats it as "the session starts" counts three
starts for one session.

### It knows things it was not told

On the third turn the session called the user **by first name**, given neither by the system prompt nor by the working
directory. Asked, it quoted the account's e-mail address, said it came from a **system context provided with the
conversation**, and that it had deduced the name.

So **the CLI injects the account's address into the session context**, whatever `--system-prompt` says and despite
`--exclude-dynamic-system-prompt-sections`. It is not a cache problem, since the address is constant; it is a behaviour
problem, corrected in the prompt and held by a test.

### Interrupting a turn without killing the session — **measured** on 2026-11-23, CLI 2.1.323

One line on stdin, at any moment of the turn:

```json
{"type":"control_request","request_id":"aria-interrupt-1","request":{"subtype":"interrupt"}}
```

Measured on a 400-word story cut at its fifteenth fragment, followed at once by another message:

- **12 ms** later: `control_response` / `success`, with `still_queued: []`;
- the **partial** `assistant` message is emitted, followed by a synthetic `user` turn `[Request interrupted by user]`:
  **the model keeps what it had begun**;
- the turn closes with a `result` of subtype **`error_during_execution`**. Read without context it is a failure: this is
  the trap;
- the next message **starts at once**, in the same process and with the cache.

A turn that has called a tool was not measured here, and ARIA does not interrupt it.

## 11. Serving our own tools to the voice agent, **measured** on 2026-10-26, **replayed** on 2026-11-13

> **Replayed on 2026-11-13, CLI 2.1.323** (`--test agent_tools -- --ignored`). The session carries exactly ARIA's tools,
> and the model calls `delegate_task` by itself, with a tier written in French and a `context` of its own.
>
> **The CLI returns the list of tools in alphabetical order**, not in declaration order, and promises nothing: compare
> **sets**.
>
> Cost of a turn with a tool call: 6 input tokens, 406 output, 10,178 read from cache, 699 written,
> **`total_cost_usd` = 0.00128** — the only monetary measure the stream gives.

Measured on CLI **2.1.165** with the MCP server of `crates/core/src/mcp/voice.rs`.

### `--tools` makes a tool exist, it does not make it runnable

| Flag | Answers |
|---|---|
| `--tools` | which tools **exist** in the session |
| `--allowed-tools` | which may run **without asking** |
| `--permission-prompt-tool` | whom to ask for the others |

With `--tools mcp__aria__delegate_task` alone, the model called the tool and the CLI refused it. The refusal came out
**through the speakers**. `--allowed-tools` is a blanket pre-approval, where this post's policy is a *rule*: the only
tools that exist here are ARIA's, allowed by construction, and any other name is refused. What `delegate_task` *does* is
open a session, which goes through the relay like any other: the user's question is asked one level below.

### The CLI starts the turn without waiting for its MCP servers

In `system/init`, **at every try**:

```json
"tools":[],"mcp_servers":[{"name":"aria","status":"pending"}]
```

The first utterance sent to a new process is therefore answered by a model **without tools**, which explains at length
that it cannot look up the weather. Waiting before speaking does not help: the connection starts with the first message.
Hence a **warm-up turn**, whose answer is thrown away.

### What `init` can say, and when

At the first turn it names no MCP tool; from the second, the list is right. It checks both ways: **no more**, since
`--tools` is the only thing between a voice dispatcher and a coding agent; **no less**, since a model without tools only
apologises.

### `init` does not arrive at start

It arrives **after the first input message**. A probe with stdin closed prints nothing: hold the input open and speak.

### The reading that validates the chain

On "Quel temps fait-il demain à Lyon ?":

```text
tool   : mcp__aria__delegate_task {"context":"...","task":"Chercher la météo de demain..."}
turn   : input 81, cache_read 2266, cache_creation 261, output 195
says   : « Je regarde la météo de demain à Lyon et je reviens vers vous dès que j'ai le résultat. »
spoken : « Il fera vingt-trois degrés ce matin et vingt-sept degrés cet après-midi. »
```

The model delegated by itself, and the spoken sentence is the session's, word for word, figures included.
