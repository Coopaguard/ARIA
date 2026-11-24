//! Intent routing: which of the three paths an intent takes.
//!
//! - short — a native action with extractable parameters, run directly;
//! - long — open-ended or multi-step, delegated to an agent provider;
//! - conversational — a plain answer, streamed, with no action.
//!
//! Holds the session context that makes follow-ups work ("rename them",
//! "no, the other one"). See the original specification.
