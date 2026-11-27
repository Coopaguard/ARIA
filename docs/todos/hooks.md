# Hooks: wake ARIA on any event

Defined on 2026-11-27. A hook is **a script, not a model**: it checks something on its rhythm and, when there is
news, prints one JSON line. ARIA then speaks first, or follows the answer she is giving. No token is spent while
nothing changes.

**Built**, in `crates/core/src/hooks/`, not yet tried in real use:

- `~/.aria/hooks/<id>/` holds `hook.toml`, the script and `state/`. The contract is in `output.rs`, and the
  sessions read it in the `aria-hooks` skill (`hooks/skill.md`), written to `~/.aria/subagents/.claude/skills/`
  when it is absent. A test holds the skill to the limits the code enforces.
- The core runs scripts through `aria_core::process`, only while ARIA is open. Secret values are masked in
  everything a run outputs, and PowerShell is read and written in UTF-8.
- What the user validated is a fingerprint of the script, its name and its secrets, kept in `aria.db`
  (`hook_approvals`) and not in the folder. A changed script is suspended. **The trial is the validation**
  (2026-11-28): one run, the user says whether it is what they expected, then the next run, judged the same way.
  Two « right » and the hook runs on its own; one « wrong » ends the trial, and a failed run ends it too. For a
  mailbox: « tu as reçu ce mail » — right; « pas de nouveau mail » — right.
- `inbox.rs` holds the timing. There is one pending event per hook, merged. During an exchange, the event waits for
  the end of the answer and follows it (« Au fait, … »). It also waits when that answer ended on a question. With
  nobody talking, the orb lights for two seconds and ARIA opens. `info` is shown, never said. `quiet` and
  `max_per_day` apply, and `urgent` passes both and wakes ARIA from sleep.
- The floor is read from what the pilot already publishes (`Mark::TurnBegan`, `TurnFinished`) and from the voice
  state. Speaking goes through `Conversation::emit`, like a delegated result, and the main agent is told it is data.
- The **Hooks** tab of the left menu shows the state, the next and last runs, the last results, and the actions try
  (run by run, « c'est bon » / « à corriger »), switch and delete. A hook that is modified or failing marks the tab.
- **By voice**, through delegation: the voice post's catalogue has no room left, so the five tools are on the
  sessions' post (`mcp/hooks.rs`). `hooks_list` and `ask_secret` are answered without asking (2026-11-27): the
  list holds names and never a value, and the form is itself the answer. **Each trial run and each deletion are
  asked by the core itself** (`Core::confirm`), the first run's card carrying the script. Sessions run Claude Code
  in `auto` mode, where the CLI approves calls without asking the relay, and a session validated its own hook
  that way on 2026-11-27. There is no tool to validate: only the user's verdicts do. Switching goes through the
  relay.
- Seen live on 2026-11-27: the session found and loaded the skill, the trial and the fingerprint held, and the
  news followed an answer (« Au fait, … »). The skill is rewritten on start when ARIA's copy is unchanged
  (`.shipped` keeps its fingerprint).

**Decided in building, against the first sketch:**

- The switch is held by the core with the fingerprint, not in `hook.toml`, so a session cannot switch a hook back
  on.
- The baseline is set by the trial. Outside a trial, news from an empty `state/` is shown, not said.
- Without `say`, an event is shown only. Having the main agent word it needs a `Told` whose answer is spoken and
  that the user did not say, and there is none yet.

**Left:**

- [ ] Secrets are built (`hooks/secrets.rs`, `app/src/hooks.rs`): `ask_secret(hook, name, label)` on the sessions'
      post, and a form of ARIA's own at the head of the Hooks tab. The value goes from that form to the store over
      the IPC, never into the event stream, and is masked in what a run outputs. The store is `keyring` 4.2:
      Credential Manager was round-tripped on Windows, and Secret Service on Linux is not tried yet. Without a
      store, saving refuses rather than writing a file. Deleting a hook removes its secrets. The tab lists each
      secret's name and whether it is stored, with « saisir » or « remplacer ».
- [ ] Try again with a secret: the first live test hard-coded a made-up value instead of asking for one. The skill
      now forbids it, and `delegate_task` says a task may need a password. The voice post's catalogue is then at
      3145 bytes of 3150.
- [ ] « Run now » in the tab. Cadences listed in the same tab once they exist ([cadence](cadence.md)).
- [ ] Later: other programs sending through a local endpoint with a token.
