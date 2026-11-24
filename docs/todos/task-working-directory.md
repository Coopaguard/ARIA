# Give a delegated task its project folder

`LaunchSpec::cwd` accepts it; `delegate_task` does not expose it, so every session starts in
`~/.aria/subagents`. Mind the catalogue budget (3081 of 3150 bytes), and what a folder chosen by a language
model is allowed to be: see [security guards](security-guards.md).
