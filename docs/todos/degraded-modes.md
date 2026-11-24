# Say clearly what is degraded

| Situation | Expected |
|---|---|
| no microphone, or permission refused | text mode, one explicit message |
| no ASR | typing, and ARIA says so |
| no voice | written answers, with a visible sign |
| uncertain transcript | ARIA repeats what it understood **before** any action with a side effect |
| noisy room | tighter gate, more confirmations |
| quota reached, CLI down, no network | "I cannot answer for now", with the reason; pending approvals stay visible, running sessions go on |

Written answers and spoken ones differ: a path, a table or code is never read aloud.
