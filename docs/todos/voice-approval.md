# Approve by voice

Decided on 2026-11-13, half done: the voice agent knows what delegated sessions can do (`agent::skills`).
Left, in order:

1. **The card shows what it approves.** `command_in` reads only `command` and `path`, so for `send_message` the
   user approves a tool name, with neither recipient nor text.
2. **The voice agent is told** that a request waits (on the channel of `Told::Returned`) and has a tool to answer
   it, choosing which when there are several. `Channel::Voice` exists and nothing uses it. The tool costs bytes
   in a catalogue bounded at 3150, 3081 used.
3. **A refusal can carry an instruction**: the `deny` message goes back to the session, which writes another version.

The voice never approves high risk, and a spoken request names its exact target.
