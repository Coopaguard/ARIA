//! Hooks that let a result **come in** from outside, and the registry that
//! declares them.
//!
//! # The definition changed direction on 2026-11-13, and nothing is built yet
//!
//! Defined by the user, in these words: *"les actions sont des hook qui doivent
//! permettre à ARIA de recevoir un résultat depuis l'extérieur, et pourquoi le
//! relancer"* — hooks that let ARIA **receive** a result from outside, and the
//! reason to wake it. Decision 112.
//!
//! **That is the opposite arrow from everything written here before**, and the
//! reason this header starts with it: the original specification and the
//! paragraphs below specify an *outbound* short path — the core can do
//! something, the agent calls it, the result comes back in the tool's reply.
//! An action under the new definition starts outside: something produced a
//! result and hands it to ARIA, which did not ask for it this turn.
//!
//! So **`run_action` is probably not this tool.** Its declared description says
//! "le résultat est dans la réponse de cet outil" — synchronous, outbound, the
//! exact inverse. The 248 bytes it occupies in the catalogue without being
//! published are no longer "the tool we will not have to rewrite": they may be
//! a second mechanism that happens to share the name. Do not reach for it out
//! of habit because it is already there.
//!
//! What stays true is **what the registry is for**: a declared surface, where
//! an identifier absent from it does not exist. What each entry has to carry is
//! now an open question rather than a settled list, because a hook's risk is
//! not an executor's — read decision 112 before designing one, in particular
//! its three open questions: who may come in, whether ARIA may speak unasked,
//! and where the permission trap below moves to.
//!
//! # The old specification, kept because the trap in it is still live
//!
//! Each action declares an identifier, a natural-language description, a
//! parameter schema, a risk level, reversibility and an executor. A native
//! capability absent from the registry does not exist.
//!
//! The registry says nothing about what the *agent* may do: Claude Code keeps
//! its own tools, and the core holds no catalogue facing it.
//! See the original specification.
//!
//! # Empty, and deferred past the MVP
//!
//! Confirmed by the user on 2026-11-12. The canonical example of a short path
//! was "what time is it", and decision 107 answered it without a tool at all —
//! every message to the post now carries the date and the minute. What is left
//! for a registry is what **acts** rather than what is known.
//!
//! Before writing the first executor, read [`crate::mcp::voice::unbacked`]: the
//! post pre-approves its own tools by construction, so an executor without a
//! risk-aware `approve` is an action that runs without ever being asked. The
//! rest — how the model learns the action names under the catalogue's byte
//! budget, and why the system prompt cannot carry them — is in
//! `docs/todos/actions.md`.
