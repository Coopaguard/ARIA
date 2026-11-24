//! Handing work to a Claude subprocess, and getting a spoken answer back.
//!
//! The main agent calls `delegate_task`; a session runs; its answer has to come
//! back as **one sentence ready to be said**. Everything here exists to make
//! that last step not be a paraphrase.
//!
//! # Why the session says it, and not the main agent
//!
//! The scenario this project was sketched from carried the fault in itself: the
//! session finds **23 °C** and the main agent announces **24 °C**. A model that
//! paraphrases a payload corrupts it — above all numbers, above all aloud,
//! where nothing lets the listener check.
//!
//! So the delegated session is asked for the spoken sentence itself, on a
//! marked line, and the core pushes that line **straight to the TTS**. No
//! generation on the return path means no drift, no tokens and no latency.
//!
//! # Why a marker rather than a tool
//!
//! A tool call would be typed and tidy, and it would also be one more thing a
//! session can forget to do, leaving a finished task with nothing to say. A
//! marked last line degrades instead: when it is missing, [`spoken_from`]
//! **cleans** what the session did write rather than inventing something. The
//! fallback is lossy, never fabricated.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::sessions::SessionId;

use super::clock::Stamp;
use super::tools::DelegateTask;

/// The line a delegated session is asked to end on.
///
/// Uppercase and accented: it has to be something a coding agent would not
/// write by accident in the middle of an explanation.
pub const SPOKEN_MARKER: &str = "À DIRE :";

/// Longest a spoken answer may be before it stops being speakable.
///
/// Roughly fifteen seconds aloud. Past that the user has stopped listening and
/// the assistant is reading a document at them.
const MAX_SPOKEN: usize = 300;

/// What a delegated session is told, on top of its task.
///
/// Short on purpose: it is prepended to every delegation, so every word is
/// paid for on every task.
pub fn delegated_prompt(call: &DelegateTask) -> String {
    let mut prompt = String::new();
    if let Some(context) = &call.context
        && !context.is_empty()
    {
        prompt.push_str("Contexte : ");
        prompt.push_str(context);
        prompt.push_str("\n\n");
    }
    prompt.push_str(&call.task);
    prompt.push_str(
        "\n\nQuand tu as terminé, termine ta réponse par une dernière ligne, seule, \
         commençant par « À DIRE : » suivie d'UNE phrase en français qui sera prononcée \
         à voix haute : pas de Markdown, pas de liste, les nombres écrits tels qu'ils se \
         disent. Cette phrase doit se suffire à elle-même.",
    );
    prompt
}

/// The sentence to say, out of whatever the session finished with.
///
/// Three steps, in order, and the order is the whole design: take the marked
/// line if there is one; otherwise clean what was written; and never write
/// anything that was not there.
pub fn spoken_from(summary: &str) -> String {
    // The *last* marked line, not the first: a session that explains its plan
    // may well quote the instruction it was given before carrying it out.
    if let Some(line) = summary
        .lines()
        .rev()
        .find_map(|line| line.trim().strip_prefix(SPOKEN_MARKER))
    {
        let said = clean(line);
        if !said.is_empty() {
            return truncate(&said);
        }
    }

    // No marker. Fall back to the last paragraph that has words in it, cleaned
    // of everything that would be read aloud as punctuation noise.
    // `rsplit`, not `split().rev()`: splitting on a multi-character pattern
    // does not give a double-ended iterator.
    let fallback = summary
        .rsplit("\n\n")
        .map(clean)
        .find(|p| p.chars().any(char::is_alphanumeric))
        .unwrap_or_default();
    truncate(&fallback)
}

/// Strip what a text-to-speech engine would read out as noise.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let line = line.trim();
        // Bullets and headings, which are heard as "dash" and "hash".
        let line = line.trim_start_matches(['#', '>', '-', '*']).trim();
        if line.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        for c in line.chars() {
            // Emphasis and code fences carry no sound, only confusion.
            if !matches!(c, '*' | '`' | '_' | '#') {
                out.push(c);
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Cut at a sentence end rather than mid-word.
fn truncate(text: &str) -> String {
    if text.chars().count() <= MAX_SPOKEN {
        return text.to_string();
    }
    let cut: String = text.chars().take(MAX_SPOKEN).collect();
    match cut.rfind(['.', '!', '?']) {
        Some(end) => cut[..=end].to_string(),
        None => match cut.rfind(' ') {
            Some(end) => format!("{}…", &cut[..end]),
            None => cut,
        },
    }
}

/// What a delegated task produced, once it came back.
#[derive(Clone, Debug, PartialEq)]
pub struct Returned {
    /// Straight to the TTS, word for word. Never regenerated.
    pub speak: String,
    /// Pushed into the main agent as a user message, so it knows the task
    /// closed and can be asked about it afterwards.
    ///
    /// Unstamped: [`super::conversation::Told`] is what dates it, from
    /// [`Self::at`].
    pub inform_agent: String,
    /// When the result came back.
    ///
    /// Carried rather than read again downstream, so the journal entry and the
    /// message the agent receives name the same minute.
    pub at: Stamp,
}

/// Said when a delegated task fails.
///
/// Not a result — an admission. The alternative is silence, and silence after
/// "je regarde" is the worst outcome of the whole chain: the user waits for an
/// answer that is never coming and has no way to know.
pub const FAILED: &str = "Je n'ai pas réussi à faire ça.";

/// A session the main agent delegated to has finished.
///
/// `None` when the session was not one of ours — the user can launch sessions
/// from the interface, and those answer to the screen, not to the voice.
pub fn finished(
    delegations: &Delegations,
    journal: &mut super::journal::Journal,
    at: Stamp,
    session: SessionId,
    summary: &str,
) -> Option<Returned> {
    let task = delegations.claim(session)?;
    let speak = spoken_from(summary);
    // An empty answer is a failure wearing a success's clothes: the session
    // ended, said nothing usable, and the user would get silence.
    let speak = if speak.trim().is_empty() {
        FAILED.to_string()
    } else {
        speak
    };
    let inform_agent = journal.returned(at, &task, &speak)?;
    Some(Returned {
        speak,
        inform_agent,
        at,
    })
}

/// A session the main agent delegated to has failed.
pub fn failed(
    delegations: &Delegations,
    journal: &mut super::journal::Journal,
    at: Stamp,
    session: SessionId,
    message: &str,
) -> Option<Returned> {
    let task = delegations.claim(session)?;
    // The user hears the admission; the agent is told what actually went
    // wrong, because it may be able to say something more useful next turn.
    let inform_agent = journal.returned(at, &task, format!("échec : {message}"))?;
    Some(Returned {
        speak: FAILED.to_string(),
        inform_agent,
        at,
    })
}

/// Which session is doing which task.
///
/// Two maps rather than one, because both directions are asked for at
/// different moments: the journal knows a task id and wants its session, and a
/// finished session arrives knowing only its own id.
#[derive(Debug, Default)]
pub struct Delegations {
    inner: Mutex<Pairs>,
}

#[derive(Debug, Default)]
struct Pairs {
    by_task: HashMap<String, SessionId>,
    /// The tasks a session still owes an answer to, **in the order they were
    /// sent**. A queue and not one task since 2026-11-23: a follow-up can go
    /// to a session still working on the first instruction, the CLI answers
    /// them in turn, and each answer belongs to its own task.
    by_session: HashMap<SessionId, std::collections::VecDeque<String>>,
    /// Which session did a task that has come back. Kept so the agent can
    /// send a follow-up to that same session later — see
    /// [`Delegations::session_of`].
    ended: HashMap<String, SessionId>,
}

impl Delegations {
    pub fn new() -> Self {
        Self::default()
    }

    /// Remember that this task is running in this session.
    pub fn record(&self, task: impl Into<String>, session: SessionId) {
        let task = task.into();
        let mut pairs = self.inner.lock().expect("delegation map poisoned");
        pairs
            .by_session
            .entry(session)
            .or_default()
            .push_back(task.clone());
        pairs.by_task.insert(task, session);
    }

    /// The task a session has just answered — the oldest it still owed —
    /// forgetting the pairing.
    ///
    /// Forgetting is the point: an answer pairs with exactly one task, and a
    /// pairing that outlived it would make a second, unrelated event look like
    /// an answer to a question nobody asked.
    pub fn claim(&self, session: SessionId) -> Option<String> {
        let mut pairs = self.inner.lock().expect("delegation map poisoned");
        let queue = pairs.by_session.get_mut(&session)?;
        let task = queue.pop_front()?;
        if queue.is_empty() {
            pairs.by_session.remove(&session);
        }
        pairs.by_task.remove(&task);
        pairs.ended.insert(task.clone(), session);
        Some(task)
    }

    pub fn session_for(&self, task: &str) -> Option<SessionId> {
        self.inner
            .lock()
            .expect("delegation map poisoned")
            .by_task
            .get(task)
            .copied()
    }

    /// The session behind a task, running or come back.
    ///
    /// What a follow-up needs: « et ajoute la pop-up » belongs to the session
    /// that has been reading a project for a minute, not to a fresh one that
    /// would read it all again — three of those were opened for one subject
    /// on 2026-11-23.
    pub fn session_of(&self, task: &str) -> Option<SessionId> {
        let pairs = self.inner.lock().expect("delegation map poisoned");
        pairs
            .by_task
            .get(task)
            .or_else(|| pairs.ended.get(task))
            .copied()
    }

    pub fn outstanding(&self) -> usize {
        self.inner
            .lock()
            .expect("delegation map poisoned")
            .by_task
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two instructions to one session, answered in turn: each answer goes
    /// to its own task, and the session stays reachable once both are back.
    #[test]
    fn a_session_answers_its_tasks_in_order_and_stays_reachable() {
        let delegations = Delegations::new();
        let session = SessionId::new();
        delegations.record("t1", session);
        delegations.record("t2", session);
        assert_eq!(delegations.outstanding(), 2);

        assert_eq!(delegations.claim(session).as_deref(), Some("t1"));
        assert_eq!(delegations.session_of("t1"), Some(session), "a follow-up can still find it");
        assert_eq!(delegations.claim(session).as_deref(), Some("t2"));
        assert_eq!(delegations.claim(session), None, "nothing more was asked");
        assert_eq!(delegations.outstanding(), 0);
        assert_eq!(delegations.session_of("t2"), Some(session));
        assert_eq!(delegations.session_of("t9"), None);
    }

    #[test]
    fn the_marked_line_is_what_gets_said() {
        let summary = "J'ai consulté trois sources météo.\n\n\
             Les prévisions concordent.\n\
             À DIRE : Il fera vingt-trois degrés ce matin et vingt-sept cet après-midi.";
        assert_eq!(
            spoken_from(summary),
            "Il fera vingt-trois degrés ce matin et vingt-sept cet après-midi."
        );
    }

    /// A session that quotes its own instructions before following them.
    #[test]
    fn the_last_marker_wins_not_the_first() {
        let summary = "Je dois terminer par À DIRE : une phrase.\n\
             Voilà qui est fait.\n\
             À DIRE : Trois factures sont en retard.";
        assert_eq!(spoken_from(summary), "Trois factures sont en retard.");
    }

    /// Without the marker nothing is invented — what was written is cleaned.
    #[test]
    fn a_missing_marker_degrades_it_does_not_fabricate() {
        let summary = "## Résultat\n\n\
             - **Trois** factures en retard\n\
             - Total : `1 240 €`\n\n\
             Il reste trois factures en retard, pour mille deux cent quarante euros.";
        let spoken = spoken_from(summary);
        assert_eq!(
            spoken,
            "Il reste trois factures en retard, pour mille deux cent quarante euros."
        );
        for noise in ['*', '#', '`'] {
            assert!(!spoken.contains(noise));
        }
    }

    /// Markdown that reaches a TTS is read aloud as punctuation.
    #[test]
    fn markdown_never_reaches_the_voice() {
        let spoken = spoken_from("À DIRE : **Trois** factures, soit `1240` euros.");
        assert_eq!(spoken, "Trois factures, soit 1240 euros.");
    }

    #[test]
    fn a_long_answer_is_cut_at_a_sentence() {
        let long = format!("À DIRE : {} Fin.", "Une phrase de remplissage. ".repeat(30));
        let spoken = spoken_from(&long);
        assert!(spoken.chars().count() <= MAX_SPOKEN);
        assert!(spoken.ends_with('.'));
    }

    #[test]
    fn the_prompt_asks_for_the_line_and_carries_the_context() {
        let call = DelegateTask {
            task: "chercher la météo de demain à Lyon".into(),
            context: Some("l'utilisateur part tôt".into()),
            model: None,
            project: None,
            follows: None,
        };
        let prompt = delegated_prompt(&call);
        assert!(prompt.contains("chercher la météo"));
        assert!(prompt.contains("l'utilisateur part tôt"));
        assert!(prompt.contains(SPOKEN_MARKER));
        assert!(prompt.contains("pas de Markdown"));
    }

    use super::super::journal::Journal;
    use crate::agent::clock::tests::at;

    fn delegated(task: &str) -> (Delegations, Journal, SessionId) {
        let delegations = Delegations::new();
        let mut journal = Journal::new();
        let session = SessionId::new();
        journal.delegated(at(14, 32), "t1", task);
        delegations.record("t1", session);
        (delegations, journal, session)
    }

    #[test]
    fn the_whole_return_path_in_one_step() {
        let (delegations, mut journal, session) = delegated("chercher la météo");
        let out = finished(
            &delegations,
            &mut journal,
            at(14, 35),
            session,
            "J'ai consulté les prévisions.\nÀ DIRE : Il fera vingt-trois degrés ce matin.",
        )
        .expect("it was ours");

        // Verbatim to the voice, and the number survives.
        assert_eq!(out.speak, "Il fera vingt-trois degrés ce matin.");
        // And the agent learns the task closed, with what was said.
        assert!(out.inform_agent.contains("chercher la météo"));
        assert!(out.inform_agent.contains("vingt-trois"));
        // The moment of the return travels with it, so the journal entry and
        // the message the agent receives name the same minute.
        assert_eq!(out.at, at(14, 35));
    }

    /// A session the user launched from the interface answers to the screen.
    #[test]
    fn a_session_we_did_not_delegate_to_says_nothing_aloud() {
        let (delegations, mut journal, _) = delegated("la météo");
        let someone_else = SessionId::new();
        assert_eq!(
            finished(&delegations, &mut journal, at(14, 35), someone_else, "À DIRE : coucou"),
            None
        );
        assert_eq!(delegations.outstanding(), 1);
    }

    /// Silence after "je regarde" is the worst outcome in the chain.
    #[test]
    fn a_failure_is_admitted_rather_than_swallowed() {
        let (delegations, mut journal, session) = delegated("la météo");
        let out = failed(
            &delegations,
            &mut journal,
            at(14, 35),
            session,
            "le processus a disparu",
        )
        .expect("it was ours");
        assert_eq!(out.speak, FAILED);
        // The agent gets the reason even though the user does not.
        assert!(out.inform_agent.contains("le processus a disparu"));
    }

    #[test]
    fn an_empty_answer_is_a_failure_in_disguise() {
        let (delegations, mut journal, session) = delegated("la météo");
        let out = finished(&delegations, &mut journal, at(14, 35), session, "   \n\n  ")
            .expect("it was ours");
        assert_eq!(out.speak, FAILED);
    }

    #[test]
    fn a_pairing_is_claimed_once_and_only_once() {
        let delegations = Delegations::new();
        let session = SessionId::new();
        delegations.record("t1", session);
        assert_eq!(delegations.session_for("t1"), Some(session));
        assert_eq!(delegations.outstanding(), 1);

        assert_eq!(delegations.claim(session).as_deref(), Some("t1"));
        // A second event from the same session is not an answer to anything.
        assert_eq!(delegations.claim(session), None);
        assert_eq!(delegations.outstanding(), 0);
    }
}
