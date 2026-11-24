//! Timestamped marks of what the main agent does, for whoever measures it.
//!
//! Not events: nothing here reaches the window or the replay ring. A turn's
//! answer to a returned result is never spoken, so no `spoken` event can time
//! it — and that reformulation is exactly one of the figures epic 10's latency
//! bench compares between engines. The pilot emits these whatever the engine;
//! with nobody subscribed, a send costs nothing.

use std::time::Instant;

#[derive(Clone, Debug, PartialEq)]
pub enum Mark {
    /// A message was handed to the agent. `aloud` is false for a returned
    /// result or a replayed thread.
    TurnBegan { aloud: bool },
    /// The first text of the turn.
    FirstText { aloud: bool },
    /// The agent reached for a tool.
    ToolCalled { name: String },
    /// The turn is over — or failed, with the reason as its text.
    TurnFinished { aloud: bool, text: String },
}

#[derive(Clone, Debug)]
pub struct Marked {
    pub at: Instant,
    pub mark: Mark,
}
