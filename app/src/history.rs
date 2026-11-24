//! Where `aria.db` lives, and who opens it.
//!
//! The same reason [`crate::memory`] exists: the core receives its paths and
//! never discovers them, because the path API that knows where `~` is belongs
//! to Tauri and Tauri lives here.
//!
//! # `~/.aria/db/aria.db`
//!
//! Decision 93 named three artefacts and gave two of them a home: the markdown
//! pages under `~/.aria/memories`, the derived index under
//! `%LOCALAPPDATA%/aria/index`. It said of `aria.db` only that it holds "the
//! state of the application", which left the question open until 2026-11-13.
//!
//! It goes under **`~/.aria`**, with the rest of what is the user's own, and
//! the argument is ownership rather than mechanics: a conversation and the
//! audit of what ARIA was allowed to do are *theirs*. `%LOCALAPPDATA%/aria`
//! holds what is derived, large and re-downloadable — 3.7 GB of model weights
//! and an index rebuilt from the pages — and it is therefore the folder
//! somebody empties to reclaim disk. A thread of conversation has no business
//! sharing a fate with a file you are invited to delete.
//!
//! **In its own `db/` sub-folder, and not loose beside the two `AGENTS.md`.**
//! WAL mode means the database is three files, not one: `aria.db`, plus
//! `aria.db-wal` and `aria.db-shm` appearing and vanishing next to it. Loose,
//! they would clutter the one folder the user is told to open and edit.
//!
//! # The cost, stated rather than hidden
//!
//! Decision 94's objection applies here and is not answered by this choice: a
//! home directory is frequently redirected to OneDrive on Windows 11, and a
//! SQLite file written on every turn of speech, inside a folder something is
//! synchronising behind your back, is the kind of thing that corrupts. Three
//! things make it bearable, and they should be read together rather than
//! trusted one at a time:
//!
//! - the database is **not** the only copy of anything the user cannot lose —
//!   the memory's truth is its markdown pages (decision 93), and a lost thread
//!   costs the history, never the assistant;
//! - a failure to open is logged and never fatal, and the window *says* that
//!   nothing is being saved ([`crate::ipc::Conversations::saving`]);
//! - WAL plus `busy_timeout` ([`aria_core::store::db`]) is what makes a second
//!   writer wait rather than corrupt, which is the failure mode a synchroniser
//!   actually produces.
//!
//! If a corruption is ever observed in the field, the fix is not to move the
//! file back: it is to exclude `~/.aria/db` from the synchroniser, which is a
//! folder the user can point at because it has a name.

use std::path::PathBuf;

use aria_core::Core;
use aria_core::agent::instructions::DIR;
use aria_core::store::history::History;
use tauri::{AppHandle, Manager};

/// The sub-folder of `~/.aria` the database and its WAL files live in.
pub const DB_DIR: &str = "db";

/// The file itself.
pub const DB_FILE: &str = "aria.db";

/// `~/.aria/db/aria.db`.
///
/// [`DIR`] comes from the core so the folder the user is told to open and the
/// folder ARIA writes to cannot drift apart — the same reason
/// [`crate::instructions`] borrows it.
pub fn path(app: &AppHandle) -> Result<PathBuf, tauri::Error> {
    Ok(app.path().home_dir()?.join(DIR).join(DB_DIR).join(DB_FILE))
}

/// Open the history, sweep it, and open the thread this run writes into.
///
/// Failure is not fatal, and that is the same rule as the memory's: a database
/// that will not open costs the saving, never the talking. ARIA still listens,
/// still answers and still delegates; the thread simply will not be there
/// tomorrow, the window says so, and the log says why.
pub fn attach(app: &AppHandle, core: &Core) {
    let path = match path(app) {
        Ok(path) => path,
        Err(e) => {
            tracing::error!("dossier personnel introuvable, historique ignoré : {e}");
            return;
        }
    };
    match History::open(&path) {
        Ok(history) => {
            core.attach_history(history);
            tracing::info!("historique ouvert : {}", path.display());
            // A fresh thread per run, which is what was asked for on
            // 2026-11-13: ARIA starts on a new exchange, and the previous ones
            // wait in the left-hand menu until one is clicked.
            core.start_conversation();
        }
        Err(e) => tracing::error!(
            "historique indisponible ({}) : les conversations ne seront pas \
             sauvegardées — {e}",
            path.display()
        ),
    }
}
