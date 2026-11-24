//! The turn journal: how an answer that arrives late re-enters the conversation.
//!
//! A delegated task takes seconds or minutes; a tool call is a question that
//! wants an answer now. The two are reconciled the way epic 08 §3 sets out: the
//! tool returns **immediately**, marked pending, and the real result is pushed
//! back later as a fresh **user** message.
//!
//! # Why a user message, and never an orphan tool message
//!
//! A tool result only has a meaning next to the call it answers. Injected on
//! its own, later, after other turns have gone by, it is an orphan — and the
//! Bolt E4B chat template **fails outright** on one rather than degrading. Even
//! where a template tolerates it, the model has to guess what it answers.
//!
//! As a user message it carries its own context, reads as "here is the result
//! of what you asked for", and works identically on both tiers. One behaviour
//! to test instead of two.
//!
//! # Why the journal exists at all on tier A
//!
//! The CLI keeps its own history across turns, so nothing here rebuilds a
//! prompt today. What it does keep is **which delegations are still out**, which
//! nothing else knows — and [`Journal::render`] is what tier B will need the day
//! a local model holds the conversation instead. Writing it now costs a morning
//! and is tested; writing it then costs the same morning plus the bug.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::clock::{self, Stamp};

/// An identifier for one delegated task, as the tool call carried it.
pub type TaskId = String;

/// What happened, in order — and **when**, which is the half added on
/// 2026-11-12.
///
/// The time is on the entry and not derived at render time, because a journal
/// is replayed long after the fact and a clock read then would stamp every past
/// turn with the present. It is also what makes a duration testable: a test
/// places two entries fifteen minutes apart without waiting.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Entry {
    /// What the user said.
    Said { at: Stamp, text: String },
    /// What ARIA answered out loud.
    Answered { at: Stamp, text: String },
    /// A task was handed to a session, and the model was told so.
    Delegated {
        at: Stamp,
        id: TaskId,
        task: String,
    },
    /// A delegated task came back with something to say.
    Returned {
        at: Stamp,
        id: TaskId,
        spoken: String,
    },
}

impl Entry {
    /// When it happened.
    pub fn at(&self) -> Stamp {
        match self {
            Entry::Said { at, .. }
            | Entry::Answered { at, .. }
            | Entry::Delegated { at, .. }
            | Entry::Returned { at, .. } => *at,
        }
    }
}

/// One message in a legal role sequence.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub role: Role,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

/// What the model is told the moment a task is handed off.
///
/// Deliberately flat and dull. It is not an answer, and anything that reads
/// like one invites the model to repeat it out loud.
pub const PENDING: &str = "La tâche est lancée. Le résultat arrivera plus tard.";

/// Somewhere an entry is written down as well as held.
///
/// Attached rather than called, because the journal is mutated from four places
/// and a fifth will be added one day. A sink cannot be forgotten at a call
/// site; a `history.append(...)` next to each `entries.push(...)` can, and the
/// turn it is forgotten on is the one that vanishes at the next restart.
///
/// Failure is the sink's own business and never the journal's: a thread that
/// cannot be saved is still a thread, and losing the live conversation because
/// a disk is full would be the larger fault.
pub trait Sink: Send + Sync + 'static {
    fn append(&self, entry: &Entry);
}

/// The typed log of one conversation.
#[derive(Default)]
pub struct Journal {
    entries: Vec<Entry>,
    /// Tasks handed off and not yet returned, with what they were asked to do.
    outstanding: BTreeMap<TaskId, String>,
    /// Where entries are written down, when anywhere.
    sink: Option<std::sync::Arc<dyn Sink>>,
}

impl std::fmt::Debug for Journal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Journal")
            .field("entries", &self.entries)
            .field("outstanding", &self.outstanding)
            .field("sink", &self.sink.is_some())
            .finish()
    }
}

impl Journal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Write everything from here on down to `sink` as well.
    ///
    /// What is already in the journal is **not** replayed into it: a sink is
    /// attached at the start of a conversation, and a journal that already held
    /// entries is one being restored from that very sink.
    pub fn attach(&mut self, sink: std::sync::Arc<dyn Sink>) {
        self.sink = Some(sink);
    }

    /// Put a thread back, without writing any of it down again.
    ///
    /// What reopening a conversation does: the entries come **from** the store,
    /// so sending them back to it would duplicate the whole thread. Outstanding
    /// delegations are deliberately not rebuilt — a task handed to a process
    /// that no longer exists will never return, and leaving it pending would
    /// make the agent wait for it forever.
    pub fn restore(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.outstanding.clear();
    }

    /// Hold an entry, and write it down.
    fn push(&mut self, entry: Entry) {
        if let Some(sink) = &self.sink {
            sink.append(&entry);
        }
        self.entries.push(entry);
    }

    pub fn said(&mut self, at: Stamp, text: impl Into<String>) {
        self.push(Entry::Said {
            at,
            text: text.into(),
        });
    }

    pub fn answered(&mut self, at: Stamp, text: impl Into<String>) {
        self.push(Entry::Answered {
            at,
            text: text.into(),
        });
    }

    /// Record a hand-off, and hand back what to answer the tool call with.
    ///
    /// # Why [`PENDING`] is the one message to the model that carries no stamp
    ///
    /// It would be nineteen bytes for information the context already holds.
    /// A delegation is always preceded, in the same turn, by the user message
    /// that caused it — and that one is stamped. The tool result sits
    /// immediately after it in a history the CLI replays in order, so the
    /// moment the task left is pinned to the second by its neighbour.
    pub fn delegated(
        &mut self,
        at: Stamp,
        id: impl Into<TaskId>,
        task: impl Into<String>,
    ) -> &'static str {
        let (id, task) = (id.into(), task.into());
        self.outstanding.insert(id.clone(), task.clone());
        self.push(Entry::Delegated { at, id, task });
        PENDING
    }

    /// A delegated task came back.
    ///
    /// Returns the text to inject as a user message, or `None` when nothing was
    /// waiting under that identifier. **Nothing is injected on a mismatch**, and
    /// that is the point of returning an option: a result with no call behind it
    /// is exactly the orphan this module exists to prevent, and inventing a home
    /// for it would be worse than dropping it.
    /// The text comes back **unstamped**, on purpose: the caller puts it into a
    /// [`super::conversation::Told`], and that is what stamps it. Stamping here
    /// too would put two times on one message.
    pub fn returned(&mut self, at: Stamp, id: &str, spoken: impl Into<String>) -> Option<String> {
        let task = self.outstanding.remove(id)?;
        let spoken = spoken.into();
        self.push(Entry::Returned {
            at,
            id: id.to_string(),
            spoken: spoken.clone(),
        });
        Some(injection(id, &task, &spoken))
    }

    /// When a task still out was handed off, so a caller can say how long it
    /// has been running without walking the entries itself.
    pub fn delegated_at(&self, id: &str) -> Option<Stamp> {
        self.entries.iter().rev().find_map(|entry| match entry {
            Entry::Delegated { at, id: d, .. } if d == id => Some(*at),
            _ => None,
        })
    }

    /// Tasks still out, oldest first.
    pub fn outstanding(&self) -> Vec<(&str, &str)> {
        self.outstanding
            .iter()
            .map(|(id, task)| (id.as_str(), task.as_str()))
            .collect()
    }

    /// The journal as a legal alternating role sequence.
    ///
    /// Two rules, and both exist because a template breaks on their absence:
    ///
    /// - a returned result is a **user** message, never a tool message;
    /// - consecutive messages of the same role are **merged**, because a
    ///   sequence with two user turns in a row is not legal in several chat
    ///   templates and silently truncates in others.
    ///
    /// Each piece keeps **its own** stamp through the merge, which is the whole
    /// point: two user turns twenty minutes apart merge into one message that
    /// still says they were twenty minutes apart. A stamp on the merged message
    /// would collapse that to one moment and make "depuis combien de temps"
    /// unanswerable — which is the fault this stamping exists against.
    pub fn render(&self) -> Vec<Message> {
        let mut out: Vec<Message> = Vec::new();
        for entry in &self.entries {
            let at = entry.at();
            let (role, text) = match entry {
                Entry::Said { text, .. } => (Role::User, text.clone()),
                Entry::Answered { text, .. } => (Role::Assistant, text.clone()),
                Entry::Delegated { id, task, .. } => (Role::Assistant, delegated_note(id, task)),
                Entry::Returned { id, spoken, .. } => {
                    let task = self
                        .entries
                        .iter()
                        .find_map(|e| match e {
                            Entry::Delegated { id: d, task, .. } if d == id => Some(task.as_str()),
                            _ => None,
                        })
                        .unwrap_or("la tâche");
                    (Role::User, injection(id, task, spoken))
                }
            };
            let text = clock::stamped(at, &text);
            match out.last_mut() {
                Some(last) if last.role == role => {
                    last.text.push_str("\n\n");
                    last.text.push_str(&text);
                }
                _ => out.push(Message { role, text }),
            }
        }
        out
    }
}


/// What a thread is capped at when it is replayed into a fresh session.
///
/// **Not a product decision, a crash guard.** The call on 2026-11-13 was to
/// send the whole thread and let the engine compact it, which is right: a CLI
/// compacts its own context far better than we could, and a summary we wrote
/// would be one more place to be wrong about what was said.
///
/// But automatic compaction triggers when the context fills **during** a
/// session, and a whole thread arriving as one first message can sail past it
/// before it has a say. So there is a ceiling, it keeps the **end** of the
/// thread — the recent is what a conversation is about — and it says so both in
/// the log and to the model, because a model told nothing about a gap will
/// confidently fill it.
///
/// Generous on purpose: at roughly four bytes a token this is some fifty
/// thousand tokens, far more than a spoken thread reaches in thirty days, so in
/// ordinary use it never bites.
pub const REPLAY_CEILING: usize = 200_000;

/// How the replay introduces itself to the agent.
const REPLAY_HEADER: &str = "Reprise d'une conversation précédente. Ce qui suit s'est dit \
     entre l'utilisateur et toi, et c'est du contexte : n'y réponds pas, ne le répète pas \
     à voix haute. Attends la prochaine phrase de l'utilisateur.";

/// How a replay says the oldest turns were left out.
const REPLAY_TRUNCATED: &str =
    "[…] Le début de cette conversation est trop long pour être rappelé ici et n'y figure pas.";

impl Journal {
    /// The whole thread as one message, for an engine that only takes user
    /// turns.
    ///
    /// [`Self::render`] is the shape an API wants — a list of roles. A CLI
    /// session has exactly one way in, so the roles become labels inside a
    /// single message, which is the only translation that loses nothing a model
    /// needs.
    ///
    /// `None` when there is nothing to replay, so a caller does not have to
    /// decide whether an empty thread is worth a turn.
    pub fn replay(&self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let lines: Vec<String> = self
            .entries
            .iter()
            .map(|entry| {
                let at = clock::stamp(entry.at());
                match entry {
                    Entry::Said { text, .. } => format!("{at} Utilisateur : {text}"),
                    Entry::Answered { text, .. } => format!("{at} Toi : {text}"),
                    Entry::Delegated { task, .. } => format!("{at} Toi : (tâche déléguée) {task}"),
                    Entry::Returned { spoken, .. } => format!("{at} Résultat : {spoken}"),
                }
            })
            .collect();

        // From the end, because the recent is what a conversation is about.
        let mut kept: Vec<&str> = Vec::new();
        let mut bytes = 0;
        for line in lines.iter().rev() {
            bytes += line.len() + 1;
            if bytes > REPLAY_CEILING && !kept.is_empty() {
                tracing::warn!(
                    turns = lines.len(),
                    kept = kept.len(),
                    "fil trop long pour être rejoué en entier : le début est omis"
                );
                kept.push(REPLAY_TRUNCATED);
                break;
            }
            kept.push(line.as_str());
        }
        kept.reverse();

        Some(format!("{REPLAY_HEADER}\n\n{}", kept.join("\n")))
    }
}

/// How a late result is worded when it comes back in.
///
/// It names the task it answers, because a bare result read on its own is
/// ambiguous once two delegations are out at the same time. And its
/// identifier, since 2026-11-23: it is what `delegate_task`'s `follows` takes,
/// and a model cannot send a follow-up to a task it cannot name.
fn injection(id: &str, task: &str, spoken: &str) -> String {
    format!("Résultat de la tâche {id} « {task} » : {spoken}")
}

/// What the assistant's side of a hand-off looks like when replayed.
fn delegated_note(id: &str, task: &str) -> String {
    format!("J'ai lancé la tâche {id} « {task} ».")
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::clock::tests::at;

    #[test]
    fn a_result_comes_back_as_a_user_message_naming_its_task() {
        let mut journal = Journal::new();
        journal.said(at(14, 30), "quel temps fait-il ?");
        assert_eq!(journal.delegated(at(14, 30), "t1", "chercher la météo"), PENDING);

        let injected = journal
            .returned(at(14, 31), "t1", "Il fera 23 degrés ce matin.")
            .unwrap();
        assert!(injected.contains("chercher la météo"));
        assert!(injected.contains("23 degrés"));
        // Unstamped: `Told` is what stamps a message on its way to the model,
        // and two times on one message is the fault this asserts against.
        assert!(!injected.contains('['), "{injected}");

        let rendered = journal.render();
        assert_eq!(rendered.last().unwrap().role, Role::User);
        assert!(rendered.last().unwrap().text.contains("23 degrés"));
    }

    /// The orphan this module exists to prevent.
    #[test]
    fn a_result_with_no_call_behind_it_is_dropped_not_invented() {
        let mut journal = Journal::new();
        journal.said(at(9, 0), "bonjour");
        assert_eq!(journal.returned(at(9, 1), "inconnu", "42"), None);
        assert_eq!(journal.render().len(), 1);
    }

    #[test]
    fn the_same_task_never_returns_twice() {
        let mut journal = Journal::new();
        journal.delegated(at(9, 0), "t1", "chercher la météo");
        assert!(journal.returned(at(9, 1), "t1", "23 degrés").is_some());
        assert_eq!(journal.returned(at(9, 2), "t1", "23 degrés"), None);
        assert!(journal.outstanding().is_empty());
    }

    #[test]
    fn two_delegations_stay_apart() {
        let mut journal = Journal::new();
        journal.delegated(at(9, 0), "t1", "la météo");
        journal.delegated(at(9, 1), "t2", "les factures");
        assert_eq!(journal.outstanding().len(), 2);

        let second = journal
            .returned(at(9, 5), "t2", "Trois factures en retard.")
            .unwrap();
        assert!(second.contains("les factures"));
        assert!(!second.contains("météo"));
        assert_eq!(journal.outstanding(), [("t1", "la météo")]);
    }

    /// Several chat templates reject two turns of the same role in a row, and
    /// the ones that do not truncate instead.
    #[test]
    fn roles_always_alternate() {
        let mut journal = Journal::new();
        journal.said(at(9, 0), "bonjour");
        journal.said(at(9, 1), "tu es là ?");
        journal.answered(at(9, 1), "oui");
        journal.delegated(at(9, 2), "t1", "la météo");
        journal.returned(at(9, 3), "t1", "23 degrés");

        let rendered = journal.render();
        for pair in rendered.windows(2) {
            assert_ne!(pair[0].role, pair[1].role, "{rendered:#?}");
        }
        assert_eq!(rendered[0].role, Role::User);
        assert!(rendered[0].text.contains("bonjour"));
        assert!(rendered[0].text.contains("tu es là ?"));
    }

    /// The scenario the user asked for on 2026-11-12, and the reason any of
    /// this is stamped.
    ///
    /// Fifteen minutes after asking for the time, "ça fait combien de temps que
    /// je t'ai demandé l'heure ?". Nothing here computes the answer — the model
    /// does, and it can only do so if both moments are in front of it. So what
    /// is asserted is exactly that: the question and the earlier turn are in
    /// the rendered context, each with its own time, fifteen minutes apart.
    #[test]
    fn a_question_about_elapsed_time_has_both_moments_in_front_of_it() {
        let mut journal = Journal::new();
        journal.said(at(14, 32), "Quelle heure est-il ?");
        journal.delegated(at(14, 32), "t1", "donner l'heure");
        journal.returned(at(14, 32), "t1", "Il est quatorze heures trente-deux.");
        journal.answered(at(14, 33), "Il est quatorze heures trente-deux.");
        journal.said(at(14, 47), "Ça fait combien de temps que je t'ai demandé l'heure ?");

        let rendered = journal.render();
        let whole: String = rendered
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(whole.contains("[2026-11-12 14:32] Quelle heure est-il ?"), "{whole}");
        assert!(
            whole.contains("[2026-11-12 14:47] Ça fait combien de temps"),
            "{whole}"
        );
    }

    /// The merge is where a stamp is easiest to lose: two user turns in a row
    /// become one message, and a single stamp on it would say they happened at
    /// the same moment.
    #[test]
    fn merged_turns_each_keep_their_own_time() {
        let mut journal = Journal::new();
        journal.said(at(14, 32), "Quelle heure est-il ?");
        journal.said(at(14, 52), "Tu es toujours là ?");

        let rendered = journal.render();
        assert_eq!(rendered.len(), 1, "two user turns in a row must merge");
        assert!(rendered[0].text.contains("[2026-11-12 14:32]"));
        assert!(rendered[0].text.contains("[2026-11-12 14:52]"));
    }

    /// So a caller can say how long a task has been running without walking
    /// the entries.
    #[test]
    fn a_task_still_out_knows_when_it_left() {
        let mut journal = Journal::new();
        journal.delegated(at(14, 32), "t1", "chercher la météo");
        assert_eq!(journal.delegated_at("t1"), Some(at(14, 32)));
        assert_eq!(journal.delegated_at("t2"), None);
    }
}

#[cfg(test)]
mod replay_tests {
    use super::*;
    use crate::agent::clock::tests::at;

    fn a_thread() -> Journal {
        let mut journal = Journal::new();
        journal.said(at(14, 32), "quelle heure est-il");
        journal.answered(at(14, 32), "il est quatorze heures trente-deux");
        journal.delegated(at(14, 33), "t1", "regarder la météo");
        journal.returned(at(14, 35), "t1", "il fait 23 degrés");
        journal
    }

    #[test]
    fn an_empty_journal_has_nothing_to_replay() {
        assert_eq!(Journal::new().replay(), None);
    }

    #[test]
    fn a_replay_carries_every_turn_with_its_time_and_who_said_it() {
        let replay = a_thread().replay().expect("something to replay");
        assert!(replay.contains("Utilisateur : quelle heure est-il"));
        assert!(replay.contains("Toi : il est quatorze heures"));
        assert!(replay.contains("(tâche déléguée) regarder la météo"));
        assert!(replay.contains("Résultat : il fait 23 degrés"));
        assert!(replay.contains("[2026-11-12 14:35]"), "{replay}");
    }

    /// The agent must not take a replay for a cue. It is told so in the one
    /// place it will read: the message itself.
    #[test]
    fn a_replay_says_it_is_context_and_not_something_to_answer() {
        let replay = a_thread().replay().expect("something to replay");
        assert!(replay.starts_with("Reprise d'une conversation précédente"));
        assert!(replay.contains("n'y réponds pas"));
        assert!(replay.contains("ne le répète pas"));
    }

    #[test]
    fn the_turns_come_back_in_order() {
        let replay = a_thread().replay().expect("something to replay");
        let heard = replay.find("quelle heure").expect("found");
        let answered = replay.find("quatorze heures").expect("found");
        let returned = replay.find("23 degrés").expect("found");
        assert!(heard < answered && answered < returned, "{replay}");
    }

    /// The ceiling is a crash guard, not a product decision: a whole thread
    /// arriving as one first message can sail past automatic compaction before
    /// it has a say.
    #[test]
    fn a_thread_past_the_ceiling_keeps_its_end_and_says_so() {
        let mut journal = Journal::new();
        journal.said(at(9, 0), "la toute première phrase");
        let filler = "x".repeat(2_000);
        for _ in 0..200 {
            journal.said(at(10, 0), &filler);
        }
        journal.said(at(11, 0), "la toute dernière phrase");

        let replay = journal.replay().expect("something to replay");
        assert!(replay.len() < REPLAY_CEILING + 1_000, "{} octets", replay.len());
        assert!(
            replay.contains("la toute dernière phrase"),
            "le récent est ce dont une conversation parle"
        );
        assert!(!replay.contains("la toute première phrase"));
        assert!(
            replay.contains("Le début de cette conversation"),
            "un modèle à qui on ne dit rien du trou le comblera tout seul"
        );
    }

    #[test]
    fn a_thread_under_the_ceiling_keeps_everything_and_says_nothing() {
        let replay = a_thread().replay().expect("something to replay");
        assert!(!replay.contains("Le début de cette conversation"));
    }

    #[test]
    fn restoring_a_thread_drops_the_delegations_that_were_still_out() {
        let mut journal = a_thread();
        journal.delegated(at(15, 0), "t2", "une tâche lancée avant la fermeture");
        let entries = journal.entries().to_vec();

        let mut reopened = Journal::new();
        reopened.restore(entries);
        // The process that was going to answer it is gone, so leaving it
        // pending would make the agent wait for it forever.
        assert_eq!(reopened.returned(at(15, 1), "t2", "trop tard"), None);
        assert_eq!(reopened.entries().len(), 5);
    }
}
