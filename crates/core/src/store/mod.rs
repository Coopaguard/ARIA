//! Persistence. Two halves of the MVP under one roof, and the roof is the point.
//!
//! - [`memory`] — the long-term memory: markdown pages the user can open, and
//!   an index derived from them.
//! - [`history`] — the conversations, their project sessions, and the audit,
//!   in `aria.db`. Written 2026-11-13, decision 111. The broker's ring is not
//!   an alternative to it: a ring holds a replay window, not an archive
//!   ([`crate::events`]).
//!
//! # Why they share a module and not a file
//!
//! Decision 100 settled this, and corrected a mistake worth keeping visible:
//! decision 93 proved that the three **artefacts** must not share a *file* —
//! the pages are the truth, `aria.db` carries an audit nothing may delete, and
//! the index is a cache thrown away on a whim. That argument is about files. It
//! says nothing about where the code lives, and an earlier decision concluded
//! separate modules from it without showing it.
//!
//! What they do share is real, and writing it twice would be the actual
//! mistake: opening SQLite, keeping migrations, WAL and `busy_timeout`,
//! serialising the single writer (decision 91), and writing a file atomically
//! by temporary plus `rename` (decisions 91 and 94). The last two live in
//! [`write`]; the SQLite ones live in [`db`], which arrived with the
//! conversation history and not before — there was nothing to share with a
//! module that did not exist.
//!
//! What they do **not** share is a database. Decision 93's rule is about
//! files, and it has not moved: the pages, `aria.db` and the index are three
//! files, never one. Not the same connection, not the same schema, not the
//! same lifetime.
//!
//! # Where it all sits
//!
//! Decision 94: the pages under `~/.aria/memories`, a plain folder and **not** a
//! git repository; the index under `%LOCALAPPDATA%`, because a home directory
//! is often synchronised and an index brushed on every write would be copied
//! back and forth for nothing. The core does not look either path up — it
//! receives a [`write::Root`], which is what makes the confinement rules of
//! decision 90 testable.

pub mod db;
pub mod history;
pub mod memory;
pub mod write;

pub use history::History;

pub use memory::{Memory, MemoryError, Recalled};
pub use write::{PathRefusal, Root};
