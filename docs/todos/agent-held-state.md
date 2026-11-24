# The agent holds its own active / asleep state

The two directions are not symmetrical.

- **Waking stays local.** In hybrid mode nothing leaves before the name is heard: this is the privacy guarantee.
  It would also cost a model turn per noise.
- **Going to sleep can be the agent's.** It already receives the text, and recognises an intention no list holds
  ("never mind", "back in five minutes").

The state travels as an inline marker the engine removes before the voice, not as a tool (a round trip). **No
marker means "change nothing"**, never "sleep". The local phrases and the timer stay, since they work without the
agent. The core owns the state; the agent proposes.
