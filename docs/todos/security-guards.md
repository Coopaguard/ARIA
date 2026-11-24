# Security guards that do not exist yet

What exists: the permission relay, the pattern registry and its risk levels, the four profiles, the audit,
high risk refused to the voice, the sandbox of tiles. **Not found in the code on 2026-11-24:**

- **Path perimeter.** Reads never reach the relay, so forbidden folders (system, secrets, keys, browser
  profiles) can only be passed as `permissions.deny` through `--settings`. Resolve links and `..` first.
- **Untrusted content.** What is read — file, page, mail, tool output — is data, never an instruction. Label
  it, and confirm any action it triggers even in Autonomous, quoting the source.
- **Outgoing actions** (send, publish, upload, push) are high risk by construction. To review: an unknown MCP
  tool is medium today, and sending a mail was accepted as medium.
- **Dry run** above N elements: the exact list before anything is applied.
- **Bin and rollback**: deletions through a bin, moves and renames that can be undone.
- **Secrets** redacted from logs, activity, history and tiles.
- **Limits** on commands, duration and cost per task (`--max-turns`, `--max-budget-usd` exist).
- **"Always allow"** limited to one action and one scope, revocable, never for high risk.

**What ARIA must never do**, whatever the wording: type passwords, card numbers or identity documents; create
accounts; pay; change system security settings; get past a CAPTCHA; download and run untrusted code; delete
for good without agreement; hide or erase its own trace.

**Before each release**: every auto-approved pattern is justified; no path bypasses the guard; the global stop
really stops everything; the audit covers what was added; tiles reach neither the network nor the core.
