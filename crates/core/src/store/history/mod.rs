//! The conversations, their project sessions, and the audit — `aria.db`.
//!
//! The other half of the MVP that [`super`] has announced since its first line,
//! and the one epic 05 specified without anybody writing it. What it holds is
//! **truth**: a conversation the user can reopen, and a journal of who allowed
//! what. Neither is derived from anything, and neither can be rebuilt by
//! rescanning a folder the way [`super::memory`]'s index can.
//!
//! # What a conversation is
//!
//! Epic 05: *an audio thread plus the list of its project sessions*. The thread
//! is [`crate::agent::journal::Entry`] — the same type the live journal holds,
//! persisted verbatim rather than translated into a second shape that would
//! then drift from it.
//!
//! A **project** session is one the main agent chose to keep: its native id is
//! stored, so it can be resumed. A throwaway session — the weather, the news —
//! never reaches this module, which is why there is nothing to clean up behind
//! one.
//!
//! # Why the thread is ours and not the engine's
//!
//! The voice post runs with `--no-session-persistence`
//! ([`crate::agent::session`]): the microphone is always open and its
//! transcript has no business on the disk. So there is no transcript to
//! `--resume`, and reopening a conversation means **replaying our own thread**
//! into a fresh session.
//!
//! That is the portable floor rather than a workaround. A thread is a list of
//! `(role, text)`: it is the first prompt of any CLI and the `messages` array
//! of any API, while `--resume` is Claude Code's own vocabulary — no other
//! engine shares the id format or the transcript's location, and two of the
//! four surveyed resume only from *inside* a session, which is not where we
//! are (`docs/todos/other-engines.md`).
//!
//! `--resume` keeps its place for the other half: a **project session**, where
//! the engine's own state is worth far more than text, and where
//! [`crate::providers::Capabilities::resumes`] already says whether it can be
//! had.
//!
//! # Retention, and the one thing it does not touch
//!
//! Thirty days since last use, **per row** and not per conversation (epic 05):
//! a thread reopened daily lives forever while its project sessions expire one
//! by one, each on the thirtieth day of its own silence.
//!
//! The audit is **never swept**. Decision 93 put it in this file for its
//! durability, and an audit journal that deletes itself after a month is not an
//! audit journal. It is also why nothing in [`audit`] has a foreign key onto a
//! conversation: a trace has to outlive the thread it came from.
//!
//! # Never deduce resumability from a date
//!
//! Epic 05's rule, and the reason [`ProjectSession::expired`] is a flag set
//! **after a failure** rather than computed from `last_used_at`. The engine's
//! own retention is `cleanupPeriodDays` in the user's settings: default 30,
//! minimum 1, and not ours to read. A session that looks fresh can be gone, so
//! the only honest test is to try it — and the entry stays in the thread either
//! way, marked, because a user who is told nothing simply asks again and
//! watches the agent start over.

pub mod audit;

use std::path::Path;

use rusqlite::{Connection, params};

use crate::agent::clock::Stamp;
use crate::agent::journal::Entry;
use crate::sessions::SessionId;
use crate::store::db;

/// How long anything here survives its last use (epic 05).
pub const RETENTION_DAYS: i64 = 30;

/// How much of a first utterance becomes the title in the left-hand menu.
const TITLE_CHARS: usize = 60;

/// What marks a title that was cut.
const ELLIPSIS: &str = "…";

/// The schema, one entry per version. Append only — see [`db::migrate`].
const MIGRATIONS: [&str; 1] = [include_str!("schema/001-initial.sql")];

/// One conversation, as the left-hand menu addresses it.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct ConversationId(uuid::Uuid);

impl ConversationId {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl std::fmt::Display for ConversationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for ConversationId {
    type Err = uuid::Error;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self)
    }
}

/// A conversation without its thread — what the menu lists.
///
/// The thread is fetched separately and on purpose: the menu shows every
/// conversation and reads none of them, and loading thirty threads to draw
/// thirty lines is the mistake this split exists to prevent.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    pub started_at: Stamp,
    pub last_used_at: Stamp,
    /// The first thing the user said, trimmed — never a model's summary.
    ///
    /// Derived rather than generated because a title is worth one line in a
    /// menu, and a generated one would cost a model call, arrive late, and be
    /// able to be wrong about what was said. Empty until the user speaks.
    pub title: String,
    /// How many entries the thread holds. The menu wants it; the thread is not
    /// read to find out.
    pub turns: usize,
}

/// A session the main agent chose to keep, so a later turn can resume it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProjectSession {
    pub id: SessionId,
    /// What the agent named the project when it opened the session.
    ///
    /// Epic 05: several sessions can belong to one project, and this is the
    /// only thing that says which. Never empty for a row that reached this
    /// table — a session with no project never gets here at all.
    pub project: String,
    /// The engine's own identifier — what `--resume` takes. `None` while the
    /// session has not reported one yet.
    pub native: Option<String>,
    pub title: String,
    pub last_used_at: Stamp,
    /// Set after a resume **failed**, never computed from a date.
    pub expired: bool,
}

/// The store. One file, one connection, serialised by the caller.
pub struct History {
    db: Connection,
}

impl History {
    /// Open `aria.db`, creating and migrating it as needed.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let mut db = db::open(path)?;
        db::migrate(&mut db, &MIGRATIONS)?;
        Ok(Self { db })
    }

    /// A store that exists only for this process — what the tests use, and
    /// what a run with nowhere to write falls back to rather than refusing to
    /// start. A conversation that cannot be saved is still a conversation.
    pub fn in_memory() -> rusqlite::Result<Self> {
        let mut db = db::open_in_memory()?;
        db::migrate(&mut db, &MIGRATIONS)?;
        Ok(Self { db })
    }

    // ------------------------------------------------------- conversations

    /// Open a new thread and return its identifier.
    pub fn start(&self, at: Stamp) -> rusqlite::Result<ConversationId> {
        let id = ConversationId::new();
        let stamp = db::stamp_to_sql(at);
        self.db.execute(
            "INSERT INTO conversations(id, started_at, last_used_at, title) VALUES (?1, ?2, ?2, '')",
            params![id.to_string(), stamp],
        )?;
        Ok(id)
    }

    /// Every conversation, most recently used first — the order of the menu.
    pub fn conversations(&self) -> rusqlite::Result<Vec<Conversation>> {
        let mut statement = self.db.prepare(
            "SELECT c.id, c.started_at, c.last_used_at, c.title,
                    (SELECT COUNT(*) FROM turns t WHERE t.conversation = c.id)
             FROM conversations c
             ORDER BY c.last_used_at DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut found = Vec::new();
        for row in rows {
            let (id, started, used, title, turns) = row?;
            // A row whose identifier or stamps cannot be read is skipped rather
            // than failing the listing: one corrupt row must not cost the menu.
            let (Ok(id), Some(started_at), Some(last_used_at)) = (
                id.parse::<ConversationId>(),
                db::stamp_from_sql(&started),
                db::stamp_from_sql(&used),
            ) else {
                tracing::warn!(%id, "conversation illisible, ignorée dans la liste");
                continue;
            };
            found.push(Conversation {
                id,
                started_at,
                last_used_at,
                title,
                turns: turns.max(0) as usize,
            });
        }
        Ok(found)
    }

    /// One conversation's row, or `None` if it is gone.
    pub fn conversation(&self, id: ConversationId) -> rusqlite::Result<Option<Conversation>> {
        Ok(self.conversations()?.into_iter().find(|c| c.id == id))
    }

    /// Mark a conversation used **now**, which is what restarts its thirty days.
    pub fn touch(&self, id: ConversationId, at: Stamp) -> rusqlite::Result<()> {
        self.db.execute(
            "UPDATE conversations SET last_used_at = ?2 WHERE id = ?1",
            params![id.to_string(), db::stamp_to_sql(at)],
        )?;
        Ok(())
    }

    /// Drop every thread nothing was ever said in, except `keep`.
    ///
    /// Asked for on 2026-11-16: a fresh thread opens at every start, so a
    /// morning of restarts left the menu six « sans titre · 0 tours » deep for
    /// one real conversation. An empty thread holds nothing to resume, so it
    /// is not kept once it is left. `keep` is the thread being entered, which
    /// may legitimately still be empty.
    ///
    /// « Empty » means no turn **and** no project session, so a thread is
    /// never swept out from under something that hangs off it.
    pub fn sweep_empty(&self, keep: Option<ConversationId>) -> rusqlite::Result<usize> {
        let keep = keep.map(|id| id.to_string()).unwrap_or_default();
        self.db.execute(
            "DELETE FROM conversations
             WHERE id != ?1
               AND NOT EXISTS (SELECT 1 FROM turns t WHERE t.conversation = conversations.id)
               AND NOT EXISTS (SELECT 1 FROM project_sessions s WHERE s.conversation = conversations.id)",
            params![keep],
        )
    }

    /// Forget one thread and everything hanging off it. The audit is untouched.
    pub fn forget(&self, id: ConversationId) -> rusqlite::Result<()> {
        self.db.execute(
            "DELETE FROM conversations WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    // --------------------------------------------------------------- turns

    /// Append one entry to a thread, and touch the conversation.
    ///
    /// The sequence number is assigned here rather than taken from the caller:
    /// the live [`crate::agent::journal::Journal`] is the only writer, but it
    /// holds its entries in a `Vec` whose indices would silently disagree with
    /// the table the first time a load is partial.
    pub fn append(&self, id: ConversationId, entry: &Entry) -> rusqlite::Result<()> {
        let (kind, text, task) = split(entry);
        self.db.execute(
            "INSERT INTO turns(conversation, seq, at, kind, text, task)
             VALUES (?1,
                     (SELECT COALESCE(MAX(seq), 0) + 1 FROM turns WHERE conversation = ?1),
                     ?2, ?3, ?4, ?5)",
            params![
                id.to_string(),
                db::stamp_to_sql(entry.at()),
                kind,
                text,
                task
            ],
        )?;
        // The first thing said names the thread. The empty string and not NULL
        // so the condition is one comparison, and `Said` only: ARIA answering
        // into an empty room is not a title.
        if let Entry::Said { text, .. } = entry {
            self.db.execute(
                "UPDATE conversations SET title = ?2 WHERE id = ?1 AND title = ''",
                params![id.to_string(), title_from(text)],
            )?;
        }
        self.touch(id, entry.at())
    }

    /// A whole thread, in order.
    pub fn thread(&self, id: ConversationId) -> rusqlite::Result<Vec<Entry>> {
        let mut statement = self.db.prepare(
            "SELECT at, kind, text, task FROM turns WHERE conversation = ?1 ORDER BY seq",
        )?;
        let rows = statement.query_map(params![id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        let mut thread = Vec::new();
        for row in rows {
            let (at, kind, text, task) = row?;
            let Some(at) = db::stamp_from_sql(&at) else {
                tracing::warn!(%kind, "tour sans horodatage lisible, ignoré");
                continue;
            };
            if let Some(entry) = join(at, &kind, text, task) {
                thread.push(entry);
            }
        }
        Ok(thread)
    }

    // ----------------------------------------------------- project sessions

    /// Record a session as belonging to this conversation, or update it.
    pub fn remember_session(
        &self,
        conversation: ConversationId,
        session: &ProjectSession,
    ) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT INTO project_sessions(conversation, id, native, project, title, last_used_at, expired)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(conversation, id) DO UPDATE SET
                 native = excluded.native,
                 project = excluded.project,
                 title = excluded.title,
                 last_used_at = excluded.last_used_at,
                 expired = excluded.expired",
            params![
                conversation.to_string(),
                session.id.to_string(),
                session.native,
                session.project,
                session.title,
                db::stamp_to_sql(session.last_used_at),
                session.expired as i64,
            ],
        )?;
        Ok(())
    }

    /// The sessions of one conversation, most recently used first.
    pub fn sessions(&self, conversation: ConversationId) -> rusqlite::Result<Vec<ProjectSession>> {
        let mut statement = self.db.prepare(
            "SELECT id, native, project, title, last_used_at, expired
             FROM project_sessions WHERE conversation = ?1
             ORDER BY last_used_at DESC",
        )?;
        let rows = statement.query_map(params![conversation.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        let mut found = Vec::new();
        for row in rows {
            let (id, native, project, title, used, expired) = row?;
            let (Ok(id), Some(last_used_at)) =
                (id.parse::<SessionId>(), db::stamp_from_sql(&used))
            else {
                continue;
            };
            found.push(ProjectSession {
                id,
                native,
                project,
                title,
                last_used_at,
                expired: expired != 0,
            });
        }
        Ok(found)
    }

    /// Record the engine's own identifier, once it has reported one.
    ///
    /// An **update**, never an insert, and that is the whole design: a session
    /// is launched before the engine names it, so the row is written at launch
    /// with `native` empty and filled in when `SessionEngaged` arrives. A
    /// throwaway session has no row, so this quietly does nothing for it —
    /// which is what keeps "the weather left no trace" true without a second
    /// place having to remember which sessions are projects.
    pub fn attach_native(
        &self,
        conversation: ConversationId,
        session: SessionId,
        native: &str,
        at: Stamp,
    ) -> rusqlite::Result<()> {
        self.db.execute(
            "UPDATE project_sessions SET native = ?3, last_used_at = ?4
             WHERE conversation = ?1 AND id = ?2",
            params![
                conversation.to_string(),
                session.to_string(),
                native,
                db::stamp_to_sql(at)
            ],
        )?;
        Ok(())
    }

    /// Mark a session no longer resumable — **after** a resume failed.
    ///
    /// The row stays: epic 05 keeps the trace of what was done even when the
    /// capacity to continue it is gone.
    pub fn mark_expired(
        &self,
        conversation: ConversationId,
        session: SessionId,
    ) -> rusqlite::Result<()> {
        self.db.execute(
            "UPDATE project_sessions SET expired = 1 WHERE conversation = ?1 AND id = ?2",
            params![conversation.to_string(), session.to_string()],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------- the audit

    /// Write one decision to the journal nothing deletes.
    pub fn record(&self, entry: &audit::Decided) -> rusqlite::Result<()> {
        audit::record(&self.db, entry)
    }

    /// The audit, most recent first. `limit` bounds a journal that only grows.
    pub fn audit(&self, limit: usize) -> rusqlite::Result<Vec<audit::Decided>> {
        audit::read(&self.db, limit)
    }

    // ------------------------------------------------------------ retention

    /// Drop what has gone thirty days without use, and say what went.
    ///
    /// Per row and not per conversation: a thread reopened daily survives while
    /// its own idle sessions expire under it. Run at startup, which is also
    /// when the CLI does its own `cleanupPeriodDays` sweep.
    pub fn sweep(&self, now: Stamp) -> rusqlite::Result<Swept> {
        let cutoff = db::stamp_to_sql(now - chrono::Duration::days(RETENTION_DAYS));
        // Sessions first: a conversation dropped in the same pass would take
        // its own rows with it by cascade and inflate the count.
        let sessions = self.db.execute(
            "DELETE FROM project_sessions WHERE last_used_at < ?1",
            params![cutoff],
        )?;
        let conversations = self.db.execute(
            "DELETE FROM conversations WHERE last_used_at < ?1",
            params![cutoff],
        )?;
        let swept = Swept {
            conversations,
            sessions,
        };
        if !swept.is_empty() {
            tracing::info!(
                conversations = swept.conversations,
                sessions = swept.sessions,
                "rétention : {RETENTION_DAYS} jours sans usage"
            );
        }
        Ok(swept)
    }
}

/// What a sweep removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    pub conversations: usize,
    pub sessions: usize,
}

impl Swept {
    pub fn is_empty(&self) -> bool {
        self.conversations == 0 && self.sessions == 0
    }
}

/// An entry as three columns.
fn split(entry: &Entry) -> (&'static str, &str, Option<&str>) {
    match entry {
        Entry::Said { text, .. } => ("said", text, None),
        Entry::Answered { text, .. } => ("answered", text, None),
        Entry::Delegated { task, id, .. } => ("delegated", task, Some(id)),
        Entry::Returned { spoken, id, .. } => ("returned", spoken, Some(id)),
    }
}

/// Three columns back into an entry.
///
/// An unknown `kind`, or a delegation that lost its task id, yields `None`: a
/// row written by a version that knew more than this one is dropped from the
/// thread rather than guessed at.
fn join(at: Stamp, kind: &str, text: String, task: Option<String>) -> Option<Entry> {
    Some(match (kind, task) {
        ("said", _) => Entry::Said { at, text },
        ("answered", _) => Entry::Answered { at, text },
        ("delegated", Some(id)) => Entry::Delegated { at, id, task: text },
        ("returned", Some(id)) => Entry::Returned {
            at,
            id,
            spoken: text,
        },
        _ => {
            tracing::warn!(%kind, "tour d'un genre inconnu, ignoré");
            return None;
        }
    })
}

/// A line of the menu, out of a first utterance.
fn title_from(said: &str) -> String {
    let trimmed = said.trim();
    if trimmed.chars().count() <= TITLE_CHARS {
        return trimmed.to_string();
    }
    // On a character boundary, and cut at a space when one is near the end so
    // the menu does not show half a word.
    let head: String = trimmed.chars().take(TITLE_CHARS).collect();
    let cut = head.rfind(' ').filter(|at| *at > head.len() / 2);
    let kept = match cut {
        Some(at) => &head[..at],
        None => &head[..],
    };
    format!("{}{ELLIPSIS}", kept.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::clock;

    fn days_ago(days: i64) -> Stamp {
        clock::now() - chrono::Duration::days(days)
    }

    fn said(at: Stamp, text: &str) -> Entry {
        Entry::Said {
            at,
            text: text.into(),
        }
    }

    fn a_session(id: SessionId, title: &str, used: Stamp) -> ProjectSession {
        ProjectSession {
            id,
            native: Some(format!("natif-{id}")),
            project: "le projet".into(),
            title: title.into(),
            last_used_at: used,
            expired: false,
        }
    }

    #[test]
    fn a_thread_comes_back_in_the_order_it_was_written() {
        let history = History::in_memory().expect("open");
        let id = history.start(days_ago(0)).expect("start");
        let entries = [
            said(days_ago(0), "quelle heure est-il"),
            Entry::Answered {
                at: days_ago(0),
                text: "il est quatorze heures".into(),
            },
            Entry::Delegated {
                at: days_ago(0),
                id: "1".into(),
                task: "regarder la météo".into(),
            },
            Entry::Returned {
                at: days_ago(0),
                id: "1".into(),
                spoken: "il fait 23 degrés".into(),
            },
        ];
        for entry in &entries {
            history.append(id, entry).expect("append");
        }
        assert_eq!(history.thread(id).expect("thread"), entries);
    }

    #[test]
    fn a_thread_survives_the_application_being_closed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("aria.db");
        let id = {
            let history = History::open(&path).expect("open");
            let id = history.start(days_ago(0)).expect("start");
            history
                .append(id, &said(days_ago(0), "bonjour Aria"))
                .expect("append");
            id
        };
        let reopened = History::open(&path).expect("reopen");
        assert_eq!(reopened.thread(id).expect("thread").len(), 1);
        assert_eq!(
            reopened
                .conversation(id)
                .expect("row")
                .expect("present")
                .title,
            "bonjour Aria",
            "et le fil se retrouve par son titre dans le menu"
        );
    }

    #[test]
    fn the_title_is_the_first_thing_said_and_nothing_later_changes_it() {
        let history = History::in_memory().expect("open");
        let id = history.start(days_ago(0)).expect("start");
        // ARIA speaking into an empty room is not a title.
        history
            .append(
                id,
                &Entry::Answered {
                    at: days_ago(0),
                    text: "je vous écoute".into(),
                },
            )
            .expect("append");
        assert_eq!(
            history
                .conversation(id)
                .expect("row")
                .expect("present")
                .title,
            ""
        );
        history
            .append(id, &said(days_ago(0), "première phrase"))
            .expect("append");
        history
            .append(id, &said(days_ago(0), "seconde phrase"))
            .expect("append");
        assert_eq!(
            history
                .conversation(id)
                .expect("row")
                .expect("present")
                .title,
            "première phrase"
        );
    }

    #[test]
    fn a_long_first_sentence_is_cut_on_a_word_and_not_mid_character() {
        let long = "je voudrais que tu prépares un nouveau projet en Rust avec une interface web \
                    et tout le reste";
        let cut = title_from(long);
        assert!(cut.ends_with(ELLIPSIS), "{cut}");
        assert!(cut.chars().count() <= TITLE_CHARS + 1, "{cut}");
        assert!(!cut.trim_end_matches(ELLIPSIS).ends_with(' '), "{cut}");
        // Accented text has to survive the cut: a byte slice here would panic.
        let accents = "é".repeat(TITLE_CHARS * 2);
        assert!(title_from(&accents).ends_with(ELLIPSIS));
    }

    #[test]
    fn the_menu_lists_the_most_recently_used_first() {
        let history = History::in_memory().expect("open");
        let old = history.start(days_ago(10)).expect("start");
        let recent = history.start(days_ago(10)).expect("start");
        history.touch(old, days_ago(5)).expect("touch");
        history.touch(recent, days_ago(1)).expect("touch");
        let listed: Vec<_> = history
            .conversations()
            .expect("list")
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(listed, vec![recent, old]);
    }

    /// Epic 05's exit condition, word for word.
    #[test]
    fn a_conversation_keeps_the_session_used_yesterday_and_loses_the_one_left_a_month() {
        let history = History::in_memory().expect("open");
        let conversation = history.start(days_ago(60)).expect("start");
        // Reopened yesterday: every use restarts its own thirty days, whatever
        // its age.
        history.touch(conversation, days_ago(1)).expect("touch");

        let fresh = SessionId::new();
        let stale = SessionId::new();
        history
            .remember_session(conversation, &a_session(fresh, "le projet en cours", days_ago(1)))
            .expect("remember");
        history
            .remember_session(
                conversation,
                &a_session(stale, "un projet abandonné", days_ago(31)),
            )
            .expect("remember");

        let swept = history.sweep(clock::now()).expect("sweep");
        assert_eq!(
            swept.conversations, 0,
            "la conversation reste dans le menu de gauche"
        );
        assert_eq!(swept.sessions, 1);

        let left = history.sessions(conversation).expect("sessions");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, fresh);
        assert_eq!(
            left[0].native.as_deref(),
            Some(format!("natif-{fresh}").as_str()),
            "et elle a gardé de quoi être reprise"
        );
    }

    #[test]
    fn a_conversation_nobody_reopened_goes_and_takes_its_thread_with_it() {
        let history = History::in_memory().expect("open");
        let id = history.start(days_ago(40)).expect("start");
        history
            .append(id, &said(days_ago(40), "une vieille phrase"))
            .expect("append");
        history
            .remember_session(id, &a_session(SessionId::new(), "projet", days_ago(40)))
            .expect("remember");

        assert_eq!(
            history.sweep(clock::now()).expect("sweep"),
            Swept {
                conversations: 1,
                sessions: 1
            }
        );
        assert!(history.conversation(id).expect("row").is_none());
        assert!(
            history.thread(id).expect("thread").is_empty(),
            "la cascade a emporté le fil"
        );
    }

    #[test]
    fn appending_to_a_thread_restarts_its_thirty_days() {
        let history = History::in_memory().expect("open");
        let id = history.start(days_ago(40)).expect("start");
        history
            .append(id, &said(days_ago(0), "on en reparle"))
            .expect("append");
        assert_eq!(history.sweep(clock::now()).expect("sweep"), Swept::default());
        assert!(history.conversation(id).expect("row").is_some());
    }

    /// Epic 05: what disappears is the ability to resume, not the memory of
    /// what was done.
    #[test]
    fn an_expired_session_keeps_its_row_and_is_only_marked() {
        let history = History::in_memory().expect("open");
        let conversation = history.start(days_ago(0)).expect("start");
        let session = SessionId::new();
        history
            .remember_session(conversation, &a_session(session, "le projet", days_ago(0)))
            .expect("remember");

        history.mark_expired(conversation, session).expect("mark");

        let sessions = history.sessions(conversation).expect("sessions");
        assert_eq!(sessions.len(), 1, "l'entrée reste dans le fil");
        assert!(sessions[0].expired);
        assert_eq!(sessions[0].title, "le projet");
    }

    #[test]
    fn remembering_the_same_session_twice_updates_it_rather_than_doubling_it() {
        let history = History::in_memory().expect("open");
        let conversation = history.start(days_ago(0)).expect("start");
        let session = SessionId::new();
        let mut entry = a_session(session, "sans identifiant", days_ago(2));
        entry.native = None;
        history.remember_session(conversation, &entry).expect("first");

        // What `attach_native_id` does the moment the engine reports its own.
        entry.native = Some("sess-42".into());
        entry.last_used_at = days_ago(0);
        history
            .remember_session(conversation, &entry)
            .expect("second");

        let sessions = history.sessions(conversation).expect("sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].native.as_deref(), Some("sess-42"));
    }

    #[test]
    fn a_turn_of_an_unknown_kind_costs_its_line_and_not_the_thread() {
        let history = History::in_memory().expect("open");
        let id = history.start(days_ago(0)).expect("start");
        history
            .append(id, &said(days_ago(0), "avant"))
            .expect("append");
        history
            .db
            .execute(
                "INSERT INTO turns(conversation, seq, at, kind, text, task)
                 VALUES (?1, 99, ?2, 'chantonne', 'la la la', NULL)",
                rusqlite::params![id.to_string(), db::stamp_to_sql(days_ago(0))],
            )
            .expect("insert");
        history
            .append(id, &said(days_ago(0), "après"))
            .expect("append");

        let thread = history.thread(id).expect("thread");
        assert_eq!(thread.len(), 2, "le fil tient, la ligne inconnue saute");
    }

    #[test]
    fn forgetting_a_conversation_leaves_the_others_alone() {
        let history = History::in_memory().expect("open");
        let kept = history.start(days_ago(0)).expect("start");
        let dropped = history.start(days_ago(0)).expect("start");
        history
            .append(kept, &said(days_ago(0), "gardée"))
            .expect("append");
        history
            .append(dropped, &said(days_ago(0), "oubliée"))
            .expect("append");

        history.forget(dropped).expect("forget");

        assert_eq!(history.conversations().expect("list").len(), 1);
        assert_eq!(history.thread(kept).expect("thread").len(), 1);
    }

    /// The menu was six « sans titre · 0 tours » deep on 2026-11-16: one
    /// fresh thread per start, never spoken in.
    #[test]
    fn an_empty_thread_is_swept_but_the_one_entered_and_any_with_content_stay() {
        let history = History::in_memory().expect("open");
        let spoken = history.start(days_ago(1)).expect("start");
        history
            .append(spoken, &said(days_ago(1), "bonjour"))
            .expect("append");
        let with_project = history.start(days_ago(1)).expect("start");
        history
            .remember_session(with_project, &a_session(SessionId::new(), "code", days_ago(1)))
            .expect("session");
        let empty = history.start(days_ago(1)).expect("start");
        let entered = history.start(days_ago(0)).expect("start");

        let swept = history.sweep_empty(Some(entered)).expect("sweep");

        assert_eq!(swept, 1, "seul le fil vide qu'on quitte part");
        let left: Vec<ConversationId> = history
            .conversations()
            .expect("list")
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert!(!left.contains(&empty));
        // Empty too, but it is the one being entered.
        assert!(left.contains(&entered));
        assert!(left.contains(&spoken));
        // No turn, yet something hangs off it: never swept.
        assert!(left.contains(&with_project));
    }
}
