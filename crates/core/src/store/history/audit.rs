//! Who allowed what — the half of `aria.db` that is never swept.
//!
//! Decision 93 put the audit in this file for its durability, and the rule that
//! follows from it is short: **retention does not touch this table**. A journal
//! that deletes itself after a month is not a journal. That is also why nothing
//! here has a foreign key onto a conversation — a trace has to outlive the
//! thread it came from, and a cascade would quietly take it with it.
//!
//! # Why an auto-approval is recorded like any other
//!
//! Decision 6 lets the pattern registry answer without asking anybody, which
//! removes the **confirmation** and not the **trace**. A row whose channel is
//! `registry` is therefore written exactly as one a human answered, and can be
//! told apart from it by that one column. Leaving them out would make the
//! journal say that nothing happened on the sessions where the most happened.
//!
//! # Why the scope is stored as it was shown
//!
//! An `AllowAlways` carries the wording the user was shown, so the journal
//! records what they actually agreed to rather than what was inferred from it
//! afterwards. It is one column, and it is the difference between "they said
//! yes" and "they said yes to this".

use rusqlite::{Connection, params};

use crate::agent::clock::Stamp;
use crate::permissions::{Channel, Decision, Risk};
use crate::sessions::SessionId;
use crate::store::db;

/// One decision, as it is written down.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Decided {
    pub at: Stamp,
    /// The session that asked. `None` for a decision taken outside one.
    pub session: Option<SessionId>,
    pub tool: String,
    /// The shell command, when the tool was one that runs one.
    pub command: Option<String>,
    pub risk: Risk,
    pub decision: Decision,
    pub channel: Channel,
}

/// Write one row.
pub fn record(db: &Connection, entry: &Decided) -> rusqlite::Result<()> {
    let (decision, scope) = match &entry.decision {
        Decision::Allow => ("allow", None),
        Decision::AllowAlways { scope } => ("allow_always", Some(scope.clone())),
        Decision::Deny => ("deny", None),
    };
    db.execute(
        "INSERT INTO audit(at, session, tool, command, risk, decision, scope, channel)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            db::stamp_to_sql(entry.at),
            entry.session.map(|id| id.to_string()),
            entry.tool,
            entry.command,
            risk_word(entry.risk),
            decision,
            scope,
            channel_word(entry.channel),
        ],
    )?;
    Ok(())
}

/// The most recent rows, newest first.
pub fn read(db: &Connection, limit: usize) -> rusqlite::Result<Vec<Decided>> {
    let mut statement = db.prepare(
        "SELECT at, session, tool, command, risk, decision, scope, channel
         FROM audit ORDER BY at DESC, rowid DESC LIMIT ?1",
    )?;
    let rows = statement.query_map(params![limit as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<String>>(6)?,
            row.get::<_, String>(7)?,
        ))
    })?;
    let mut found = Vec::new();
    for row in rows {
        let (at, session, tool, command, risk, decision, scope, channel) = row?;
        // A row that cannot be read is skipped and said, never silently
        // dropped: this is the one table where a missing line is a question.
        let (Some(at), Some(risk), Some(decision), Some(channel)) = (
            db::stamp_from_sql(&at),
            risk_from(&risk),
            decision_from(&decision, scope),
            channel_from(&channel),
        ) else {
            tracing::warn!(tool, "entrée d'audit illisible, ignorée à la lecture");
            continue;
        };
        found.push(Decided {
            at,
            session: session.and_then(|id| id.parse().ok()),
            tool,
            command,
            risk,
            decision,
            channel,
        });
    }
    Ok(found)
}

// The words are written out rather than derived from `serde` so that renaming a
// Rust variant cannot silently change what is already on disk. A migration is
// how a stored word changes; a refactor is not.

fn risk_word(risk: Risk) -> &'static str {
    match risk {
        Risk::Read => "read",
        Risk::Low => "low",
        Risk::Medium => "medium",
        Risk::High => "high",
    }
}

fn risk_from(word: &str) -> Option<Risk> {
    Some(match word {
        "read" => Risk::Read,
        "low" => Risk::Low,
        "medium" => Risk::Medium,
        "high" => Risk::High,
        _ => return None,
    })
}

fn channel_word(channel: Channel) -> &'static str {
    match channel {
        Channel::Voice => "voice",
        Channel::Window => "window",
        Channel::Registry => "registry",
    }
}

fn channel_from(word: &str) -> Option<Channel> {
    Some(match word {
        "voice" => Channel::Voice,
        "window" => Channel::Window,
        "registry" => Channel::Registry,
        _ => return None,
    })
}

fn decision_from(word: &str, scope: Option<String>) -> Option<Decision> {
    Some(match word {
        "allow" => Decision::Allow,
        // A scope that did not survive the round trip would turn "yes to this"
        // into "yes to anything", so its absence refuses the row.
        "allow_always" => Decision::AllowAlways { scope: scope? },
        "deny" => Decision::Deny,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::clock;
    use crate::store::history::History;

    fn decided(decision: Decision, channel: Channel) -> Decided {
        Decided {
            at: clock::now(),
            session: Some(SessionId::new()),
            tool: "Bash".into(),
            command: Some("git status".into()),
            risk: Risk::Low,
            decision,
            channel,
        }
    }

    #[test]
    fn a_decision_comes_back_exactly_as_it_was_written() {
        let history = History::in_memory().expect("open");
        let entry = decided(
            Decision::AllowAlways {
                scope: "git status dans ce dossier".into(),
            },
            Channel::Window,
        );
        history.record(&entry).expect("record");

        let read = history.audit(10).expect("read");
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].decision, entry.decision, "la portée telle que montrée");
        assert_eq!(read[0].session, entry.session);
        assert_eq!(read[0].command.as_deref(), Some("git status"));
        assert_eq!(read[0].at.timestamp(), entry.at.timestamp());
    }

    /// Decision 6 removes the confirmation, not the trace.
    #[test]
    fn an_automatic_approval_is_written_down_and_tells_itself_apart() {
        let history = History::in_memory().expect("open");
        history
            .record(&decided(Decision::Allow, Channel::Registry))
            .expect("record");
        history
            .record(&decided(Decision::Allow, Channel::Voice))
            .expect("record");

        let read = history.audit(10).expect("read");
        assert_eq!(read.len(), 2);
        assert!(read.iter().any(|d| d.channel == Channel::Registry));
        assert!(read.iter().any(|d| d.channel == Channel::Voice));
    }

    /// Decision 93: a journal that deletes itself after a month is not a
    /// journal. This is the one table retention must never reach.
    #[test]
    fn the_audit_survives_a_sweep_that_takes_everything_else() {
        let history = History::in_memory().expect("open");
        let conversation = history
            .start(clock::now() - chrono::Duration::days(40))
            .expect("start");
        let mut old = decided(Decision::Deny, Channel::Window);
        old.at = clock::now() - chrono::Duration::days(400);
        history.record(&old).expect("record");

        let swept = history.sweep(clock::now()).expect("sweep");
        assert_eq!(swept.conversations, 1);
        assert!(history.conversation(conversation).expect("row").is_none());
        assert_eq!(
            history.audit(10).expect("read").len(),
            1,
            "une trace de plus d'un an est toujours là"
        );
    }

    #[test]
    fn the_journal_reads_newest_first_and_the_limit_holds() {
        let history = History::in_memory().expect("open");
        for minutes in 0..5 {
            let mut entry = decided(Decision::Allow, Channel::Window);
            entry.at = clock::now() - chrono::Duration::minutes(minutes);
            entry.tool = format!("outil-{minutes}");
            history.record(&entry).expect("record");
        }
        let read = history.audit(3).expect("read");
        assert_eq!(read.len(), 3);
        assert_eq!(read[0].tool, "outil-0");
        assert_eq!(read[2].tool, "outil-2");
    }

    /// A scope lost in transit would turn "yes to this" into "yes to anything".
    #[test]
    fn an_always_without_its_scope_is_refused_rather_than_widened() {
        assert!(decision_from("allow_always", None).is_none());
        assert_eq!(
            decision_from("allow_always", Some("ce dossier".into())),
            Some(Decision::AllowAlways {
                scope: "ce dossier".into()
            })
        );
    }

    #[test]
    fn every_word_written_can_be_read_back() {
        for risk in [Risk::Read, Risk::Low, Risk::Medium, Risk::High] {
            assert_eq!(risk_from(risk_word(risk)), Some(risk));
        }
        for channel in [Channel::Voice, Channel::Window, Channel::Registry] {
            assert_eq!(channel_from(channel_word(channel)), Some(channel));
        }
        assert!(risk_from("inconnu").is_none());
        assert!(channel_from("inconnu").is_none());
    }
}
