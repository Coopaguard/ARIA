//! The orchestration: a request becomes a session, and a session becomes speech.
//!
//! Everything else in [`super`] is a piece; this is what holds them in a line.
//! It owns the journal, the delegations and the waiting phrase, and it does its
//! work **over channels** rather than against a process:
//!
//! ```text
//!   delegate_task ──► launch a session ──► PENDING to the model
//!                                             │
//!                     broker: task.done ──────┘
//!                             │
//!                 ┌───────────┴───────────┐
//!                 ▼                       ▼
//!          speech channel          to-agent channel
//!          (T4 plugs the TTS)      (the main agent is told)
//! ```
//!
//! # Why channels and not calls
//!
//! There is no TTS yet — that is T4 — and the main agent is a subprocess that a
//! test should not have to start. Both are therefore ends of a channel. The
//! consequence is that **the whole of this module tests without a CLI, without
//! a model and without a sound card**, which is the difference between a
//! scenario that can be run on every commit and one that is run once.
//!
//! It is also the right shape for the product: what ARIA says and what ARIA is
//! told are two different streams, and they are allowed to move at different
//! speeds.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::Core;
use crate::agent::clock::{self, Stamp};
use crate::agent::delegation::{self, Delegations};
use crate::agent::journal::Journal;
use crate::agent::tools::DelegateTask;
use crate::events::Event;
use crate::providers::{AgentProvider, LaunchSpec};
use crate::sessions::supervisor::Supervisor;

/// How many messages may queue before a slow consumer is a problem worth
/// noticing. Speech is the narrower of the two: nobody listens to a backlog.
const DEPTH: usize = 64;

/// The ends a caller wires up.
pub struct Channels {
    /// What to say, word for word. T4 attaches the TTS here.
    pub speech: mpsc::Receiver<String>,
    /// What to tell the main agent, as a user message.
    pub to_agent: mpsc::Receiver<Told>,
}

/// A user message for the main agent, and whether its answer is ours to say.
///
/// The distinction cost a sentence spoken **twice** on 2026-10-27, 830 ms
/// apart, in the first live run with a voice:
///
/// 1. a session returns, its line is spoken verbatim — which is the whole
///    point of [`super::speak_verbatim`]: no generation, so no drift on the
///    numbers;
/// 2. the agent is told, so it knows the task closed;
/// 3. the agent **answers** — and the system prompt tells it to repeat a
///    returned payload exactly, so it repeats the sentence;
/// 4. the pilot speaks every finished turn.
///
/// Step 3 is correct and step 4 was indiscriminate. A turn the agent took
/// because a *session* came back has already been said; anything it adds is a
/// paraphrase of numbers, which is the one thing this epic exists to prevent.
///
/// # Why it carries the time
///
/// Added on 2026-11-12, for the scenario in [`super::clock`]: the agent could
/// be asked *about* a returned task and answer, and could not be asked **when**
/// and answer anything but a guess. Every message it receives now says when it
/// arrived, which is both halves of a duration — the model subtracts two stamps
/// rather than being told one and asked to remember the other.
///
/// The stamp is taken when the message is **made**, not when it is sent. The
/// channel is where a message can wait: a second utterance inside the filler's
/// grace period sits in the queue, and stamping on the way out would date it to
/// the moment the agent got free rather than the moment the user spoke.
#[derive(Clone, Debug, PartialEq)]
pub enum Told {
    /// The user spoke. ARIA answers out loud.
    Said { at: Stamp, text: String },
    /// The user spoke, and what reaches the agent is the **sound** — epic 10.
    ///
    /// Only a local model that hears is ever sent one. Its text, for the
    /// journal and for an engine that cannot hear, is a placeholder.
    Heard { at: Stamp, audio: Audio },
    /// A session returned. Context, not a cue to speak.
    Returned { at: Stamp, text: String },
    /// A saved thread, replayed into a fresh session. Context, not a cue.
    ///
    /// Its own variant and not a [`Self::Returned`], although both are silent:
    /// a returned result answers something the agent asked for, and a replay is
    /// a past it is being handed. Collapsing them would make the one place that
    /// tells them apart a comment.
    Resumed { at: Stamp, text: String },
}

/// The samples of one turn, 16 kHz mono.
///
/// Its own type so a [`Told`] in a log line prints a length rather than
/// sixteen thousand floats a second.
#[derive(Clone)]
pub struct Audio(pub Arc<[f32]>);

impl std::fmt::Debug for Audio {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Audio({} échantillons)", self.0.len())
    }
}

impl PartialEq for Audio {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0[..] == other.0[..]
    }
}

impl std::ops::Deref for Audio {
    type Target = [f32];
    fn deref(&self) -> &[f32] {
        &self.0
    }
}

impl Told {
    /// The user spoke, now, and the agent hears the sound.
    pub fn heard(audio: Vec<f32>) -> Self {
        Told::Heard {
            at: clock::now(),
            audio: Audio(audio.into()),
        }
    }

    /// The user spoke, now.
    pub fn said(text: impl Into<String>) -> Self {
        Told::Said {
            at: clock::now(),
            text: text.into(),
        }
    }

    /// A session returned, at a moment its caller already read off the clock.
    pub fn returned(at: Stamp, text: impl Into<String>) -> Self {
        Told::Returned {
            at,
            text: text.into(),
        }
    }

    /// A thread being handed back to a fresh session, now.
    pub fn resumed(text: impl Into<String>) -> Self {
        Told::Resumed {
            at: clock::now(),
            text: text.into(),
        }
    }

    pub fn text(&self) -> &str {
        match self {
            Told::Said { text, .. } | Told::Returned { text, .. } | Told::Resumed { text, .. } => {
                text
            }
            Told::Heard { .. } => crate::agent::local::HEARD_PLACEHOLDER,
        }
    }

    pub fn at(&self) -> Stamp {
        match self {
            Told::Said { at, .. }
            | Told::Heard { at, .. }
            | Told::Returned { at, .. }
            | Told::Resumed { at, .. } => *at,
        }
    }

    /// The message as the main agent receives it, stamp and all.
    ///
    /// One of the two places a stamp is ever added — see [`super::clock`]. The
    /// pilot sends **this** and never [`Self::text`], and the test below is
    /// what notices if that is ever undone, because the symptom is silent: the
    /// agent keeps working and simply stops knowing what time it is.
    pub fn for_agent(&self) -> String {
        clock::stamped(self.at(), self.text())
    }

    /// Whether the agent's answer to this is ours to say out loud.
    pub fn answer_is_spoken(&self) -> bool {
        matches!(self, Told::Said { .. } | Told::Heard { .. })
    }
}

/// One running conversation.
pub struct Conversation {
    core: Arc<Core>,
    supervisor: Arc<Supervisor>,
    delegations: Arc<Delegations>,
    journal: Mutex<Journal>,
    speech: mpsc::Sender<String>,
    to_agent: mpsc::Sender<Told>,
    /// The waiting phrase just spoken, waiting to be told to the model.
    ///
    /// See [`Conversation::note_waiting`].
    said_while_waiting: Mutex<Option<String>>,
    /// Task identifiers. A counter, not a UUID: it exists to pair a result with
    /// a call inside one conversation, and a short one is legible in a log.
    next_task: AtomicU64,
    /// The user cut ARIA off. Read by the pilot, which owns the turn.
    ///
    /// A [`tokio::sync::Notify`] and not a flag: the pilot waits on it beside
    /// the next message, and a permit kept while it is busy writing one is
    /// exactly right — the cut then lands on the newest turn.
    interrupt: tokio::sync::Notify,
}

impl Conversation {
    /// Ask the pilot to stop the answer in progress. See [`Core::interrupt_agent`].
    pub fn interrupt(&self) {
        self.interrupt.notify_one();
    }

    /// Resolves when [`Self::interrupt`] has been called.
    pub async fn interrupted(&self) {
        self.interrupt.notified().await;
    }

    pub fn new(core: Arc<Core>) -> (Arc<Self>, Channels) {
        // The core's own, never a fresh one: a session opened by
        // `delegate_task` has to be in the registry `stop_all_sessions` walks,
        // or `STOP` leaves it running with nothing on screen to stop it with.
        let supervisor = Arc::clone(core.supervisor());
        let (speech, speech_rx) = mpsc::channel(DEPTH);
        let (to_agent, to_agent_rx) = mpsc::channel(DEPTH);
        let mut journal = Journal::new();
        // Attached here and not by the caller: the journal is mutated from four
        // places inside this module, and persistence that has to be remembered
        // at a call site is persistence that is eventually forgotten. A core
        // with no `aria.db` makes this a no-op rather than an error.
        journal.attach(Arc::new(ToHistory {
            core: Arc::clone(&core),
        }));
        let conversation = Arc::new(Self {
            core,
            supervisor,
            delegations: Arc::new(Delegations::new()),
            journal: Mutex::new(journal),
            speech,
            to_agent,
            said_while_waiting: Mutex::new(None),
            next_task: AtomicU64::new(1),
            interrupt: tokio::sync::Notify::new(),
        });
        (
            conversation,
            Channels {
                speech: speech_rx,
                to_agent: to_agent_rx,
            },
        )
    }

    /// What the user said: written down, and handed to the agent.
    ///
    /// Both, through one method, because they are one thing — and the channel
    /// is the same one a returned result travels on, which is also right: a
    /// transcript and a result are both **user messages** to the agent, and
    /// [`super::journal`] already models them that way.
    ///
    /// Synchronous and non-blocking, because the caller is the voice thread —
    /// no runtime, and nothing it may wait on. A full channel means the agent
    /// is wedged; that is worth a line in the log and not worth losing the
    /// capture thread over.
    pub fn heard(&self, text: &str) {
        // One read of the clock for both, so the journal and the wire cannot
        // disagree by a minute over a message that straddled the boundary.
        let told = Told::said(text);
        self.journal
            .lock()
            .expect("journal poisoned")
            .said(told.at(), text);
        if let Err(e) = self.to_agent.try_send(told) {
            tracing::warn!("l'agent principal n'a pas reçu « {text} » : {e}");
        }
    }

    /// The user spoke, and the agent hears the sound itself — epic 10.
    ///
    /// The journal gets the placeholder: there are no words yet, and a thread
    /// reopened later is replayed as text. Same non-blocking rule as
    /// [`Self::heard`], for the same caller.
    pub fn heard_audio(&self, audio: Vec<f32>) {
        let told = Told::heard(audio);
        self.journal
            .lock()
            .expect("journal poisoned")
            .said(told.at(), told.text());
        if let Err(e) = self.to_agent.try_send(told) {
            tracing::warn!("l'agent principal n'a pas reçu un message vocal : {e}");
        }
    }


    /// Put a saved thread back, and hand it to the agent as context.
    ///
    /// What clicking an old conversation in the left-hand menu does, **in a
    /// freshly started pilot**. That part matters and is not this function's to
    /// enforce: the CLI keeps its own history for the life of its process, so
    /// replaying a thread into the session that just held another one would
    /// leave both in its context and let it answer a question from the thread
    /// the user left. `app/` therefore drops the pilot and starts a new one,
    /// which is exactly what dropping it is built to do.
    ///
    /// The agent is told, and its answer is **not** spoken: a replay is a past
    /// being handed over, not a cue. Whatever it says back — an acknowledgement,
    /// most likely — stays inside the session.
    pub async fn resume(&self, entries: Vec<crate::agent::journal::Entry>) {
        let replay = {
            let mut journal = self.journal.lock().expect("journal poisoned");
            journal.restore(entries);
            journal.replay()
        };
        let Some(replay) = replay else {
            // An empty thread is a new conversation by another name. Sending a
            // header with nothing under it would only puzzle the model.
            return;
        };
        if self.to_agent.send(Told::resumed(replay)).await.is_err() {
            tracing::warn!("l'agent principal n'a pas reçu le fil repris");
        }
    }

    /// How many entries the live journal holds.
    ///
    /// The count `Pilot::conversation` has promised a caller since it was
    /// written. It reads the **live** journal and not the store, which is the
    /// distinction that matters when there is nowhere to write: the thread is
    /// still a thread.
    pub fn turns(&self) -> usize {
        self.journal
            .lock()
            .expect("journal poisoned")
            .entries()
            .len()
    }

    /// What ARIA said on its own, for the record.
    pub fn answered(&self, text: &str) {
        self.journal
            .lock()
            .expect("journal poisoned")
            .answered(clock::now(), text);
    }

    /// The entry point the MCP tool handler calls.
    ///
    /// Answers **immediately** with [`crate::agent::journal::PENDING`], whatever
    /// the task turns out to cost: a tool call is a question that wants an
    /// answer now, and holding it open would stall the turn of speech behind a
    /// session that may run for minutes.
    pub async fn delegate(
        &self,
        provider: &dyn AgentProvider,
        call: DelegateTask,
    ) -> anyhow::Result<String> {
        let id = format!("t{}", self.next_task.fetch_add(1, Ordering::Relaxed));
        let prompt = delegation::delegated_prompt(&call);

        // A follow-up goes to the session that already holds the subject —
        // live, or resumed from its id. Only when that cannot be done does it
        // fall through to a new session, and the log says why: an unknown id,
        // or a session that cannot be reached any more.
        if let Some(previous) = call.follows.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            match self.delegations.session_of(previous) {
                Some(session) => match self
                    .core
                    .deliver_to_session(session, &with_its_tiles(prompt.clone(), &self.core.tiles().shown_by(session)))
                    .await
                {
                    Ok(reached) => {
                        tracing::info!(tache = %call.task, suite_de = previous, "suite envoyée à la même session");
                        self.delegations.record(&id, reached);
                        let pending = self
                            .journal
                            .lock()
                            .expect("journal poisoned")
                            .delegated(clock::now(), id.as_str(), call.task);
                        return Ok(format!("Tâche {id}, suite de {previous}. {pending}"));
                    }
                    Err(e) => tracing::warn!(
                        suite_de = previous,
                        "la session de la tâche ne répond plus, une nouvelle est ouverte : {e:#}"
                    ),
                },
                None => tracing::warn!(
                    suite_de = previous,
                    "tâche inconnue pour une suite : une nouvelle session est ouverte"
                ),
            }
        }

        // The tier becomes a model **here**, because this is the one place that
        // holds both: the voice agent asked for `rapide` or `expert`, and only
        // the provider knows which of its models that is. A tier the engine has
        // no model for falls back rather than refusing — see
        // `providers::model_for`; an engine with no list at all names nothing,
        // and the session runs on that engine own default, which is what every
        // delegated session did until 2026-11-12.
        let tier = call.tier();
        let model = crate::providers::model_for(&provider.models(), tier).map(|m| m.id.clone());
        tracing::info!(
            tache = %call.task,
            tier = tier.word(),
            modele = ?model,
            "tâche déléguée"
        );

        let session = self
            .supervisor
            .launch(
                &self.core,
                provider,
                call.task.clone(),
                LaunchSpec {
                    prompt,
                    profile: self.core.default_profile(),
                    // Decision 108: `~/.aria/subagents` unless something says
                    // otherwise. `None` was not "nowhere" — it was "wherever
                    // the application was launched from", which in development
                    // is this repository, and which no task asked for.
                    //
                    // A task about a project still belongs in that project's
                    // folder; this is the floor, not the rule. Naming one per
                    // task is the open half — see
                    // `docs/todos/task-working-directory.md`.
                    cwd: self.core.home().subagents_dir,
                    resume: None,
                    max_turns: None,
                    max_budget_usd: None,
                    model,
                    relay: None,
                    mcp_servers: Vec::new(),
                    policy: None,
                    // Both filled by the supervisor, which is the one place
                    // that knows the session's token and the user's paths.
                    instructions_file: None,
                },
            )
            .await?;

        // Recorded only once the session exists. A pairing made before the
        // launch would survive a failed launch and wait for a result from a
        // process that never started.
        self.delegations.record(&id, session);
        // Epic 05, and the decision is the agent's: a session named a project
        // is kept so the conversation can come back to it; one that named none
        // is throwaway and its identifier is never stored. Written here and not
        // in the supervisor because this is where the agent's answer is.
        if let Some(project) = call.project.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
            self.core.remember_project_session(session, project);
        }
        let pending = self
            .journal
            .lock()
            .expect("journal poisoned")
            .delegated(clock::now(), id.as_str(), call.task);
        // The id comes first: it is what `follows` takes, and the model reads
        // it here, at the moment it may want to add to the task.
        Ok(format!("Tâche {id}. {pending}"))
    }

    /// Turn one broker event into speech, if it is ours to speak.
    ///
    /// Public and synchronous on purpose: the scenario test drives it directly,
    /// which is what lets the whole return path be exercised without a runtime
    /// full of tasks.
    pub fn on_event(&self, event: &Event) -> Option<delegation::Returned> {
        // Read here rather than in `emit`, because this is the moment the
        // result exists. `emit` awaits two channels, and a stamp taken after
        // that would date the result to whenever the queue drained.
        let at = clock::now();
        let mut journal = self.journal.lock().expect("journal poisoned");
        match event {
            Event::TaskDone { session, summary } => {
                let mut returned =
                    delegation::finished(&self.delegations, &mut journal, at, *session, summary)?;
                // Said so the agent ties « montre-moi la carte » to this task
                // and sends it with `follows`, instead of opening a session
                // that would draw a second tile.
                if !self.core.tiles().shown_by(*session).is_empty() {
                    returned.inform_agent.push_str(" (Sa tuile est à l'écran.)");
                }
                let proposals = self.core.take_memory_proposals(*session);
                returned
                    .inform_agent
                    .push_str(&delegation::proposals_note(&proposals));
                Some(returned)
            }
            Event::TaskError { session, message } => {
                // A failed task's proposals are not worth more than its result.
                self.core.take_memory_proposals(*session);
                delegation::failed(&self.delegations, &mut journal, at, *session, message)
            }
            _ => None,
        }
    }

    /// Send a returned result down both channels.
    pub async fn emit(&self, returned: delegation::Returned) {
        // Speech first. The agent being told is bookkeeping; the user waiting
        // is not.
        let _ = self.speech.send(returned.speak).await;
        let _ = self
            .to_agent
            .send(Told::returned(returned.at, returned.inform_agent))
            .await;
    }

    /// Say something that did not come from a session — a waiting phrase, or
    /// the main agent's own answer.
    pub async fn say(&self, text: impl Into<String>) {
        let _ = self.speech.send(text.into()).await;
    }

    /// Remember that ARIA has just covered a wait out loud.
    ///
    /// # Why the model is told, and not merely silenced
    ///
    /// Muting its words after a delegation stops the stutter but leaves the
    /// model writing into a void: it believes it is announcing something, and
    /// spends tokens and a turn's worth of latency doing it. Telling it what
    /// was already said lets it **adapt** instead of being overruled — which
    /// is what the user asked for on 2026-10-30, and the better shape: a
    /// mechanism that informs beats one that only forbids.
    ///
    /// The channel is the tool's own result. `delegate_task` already answers
    /// immediately with [`super::journal::PENDING`]; naming the phrase there
    /// costs no extra round trip and arrives exactly when it is useful.
    pub fn note_waiting(&self, phrase: &str) {
        *self.said_while_waiting.lock().expect("filler poisoned") = Some(phrase.to_string());
    }

    /// Take that phrase, once.
    ///
    /// Taken rather than read, so it cannot leak into a later turn and tell
    /// the model about a sentence spoken a minute ago.
    pub fn waiting_phrase(&self) -> Option<String> {
        self.said_while_waiting
            .lock()
            .expect("filler poisoned")
            .take()
    }

    pub fn outstanding(&self) -> usize {
        self.delegations.outstanding()
    }

    /// Watch the broker and speak what comes back.
    ///
    /// Spawned once per conversation. It survives a lagged subscription rather
    /// than ending: dropping out here would mean a delegation that silently
    /// never answers, which is the failure this whole module exists to avoid.
    pub fn watch(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let conversation = Arc::clone(self);
        let mut events = conversation.core.broker().subscribe();
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(envelope) => {
                        if let Some(returned) = conversation.on_event(&envelope.event) {
                            conversation.emit(returned).await;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        // A missed event may have been a result. Say so loudly:
                        // the symptom is a task that answers nothing, and
                        // nothing else would explain it.
                        tracing::warn!(
                            missed,
                            "la conversation a pris du retard sur le flux — un résultat a pu être \
                             manqué"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        })
    }
}

/// A follow-up, told which of its tiles are still on screen.
///
/// Asked for on 2026-11-23: « la durée d'un trajet », then
/// « affiche-moi la carte », gave two tiles where the user wanted the first one
/// completed. The session has the URI somewhere in its history; this puts it
/// next to the request, where it is read.
fn with_its_tiles(mut prompt: String, tiles: &[(String, String)]) -> String {
    if tiles.is_empty() {
        return prompt;
    }
    let list = tiles
        .iter()
        .map(|(uri, title)| {
            if title.is_empty() {
                uri.clone()
            } else {
                format!("{uri} « {title} »")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    prompt.push_str(&format!(
        "\n\nTes tuiles encore à l'écran : {list}. Si ceci en complète ou en change \
         une, actualise-la avec refresh_tile au lieu d'en afficher une nouvelle."
    ));
    prompt
}

/// Writes every journal entry into `aria.db`, under whichever thread is live.
///
/// It asks the core which thread that is on **each** entry rather than holding
/// one: the user can click another line in the left-hand menu at any moment,
/// and a sink that captured an identifier would keep filing new turns under the
/// conversation they left.
struct ToHistory {
    core: Arc<Core>,
}

impl crate::agent::journal::Sink for ToHistory {
    fn append(&self, entry: &crate::agent::journal::Entry) {
        let Some(thread) = self.core.current_conversation() else {
            // No thread open — a unit test, or a run with nowhere to write.
            // The conversation goes on; only the saving is lost.
            return;
        };
        self.core
            .with_history("l'écriture d'un tour", |history| {
                history.append(thread, entry)
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::clock::tests::at;

    /// The invariant the pilot depends on, and the reason it is asserted here
    /// rather than trusted: the symptom of losing it is silent. The agent keeps
    /// answering and simply stops knowing what time it is, which shows up weeks
    /// later as a confidently wrong duration said out loud.
    #[test]
    fn what_leaves_for_the_agent_carries_the_date_and_the_minute() {
        let told = Told::returned(at(14, 47), "Résultat de la tâche « l'heure » : ...");
        assert_eq!(
            told.for_agent(),
            "[2026-11-12 14:47] Résultat de la tâche « l'heure » : ..."
        );
        // And the text alone is still the text alone, because the journal and
        // the log want it unstamped.
        assert!(!told.text().contains('['));
    }

    /// A message is dated when it is **made**, not when the channel drains.
    ///
    /// The channel is where a message waits — a second utterance inside the
    /// filler's grace period sits in the queue — so stamping on the way out
    /// would date it to the moment the agent got free rather than the moment
    /// the user spoke. Here: a stamp taken before the two sends still survives
    /// them.
    #[test]
    fn the_stamp_is_the_moment_of_the_message_and_not_of_the_send() {
        let told = Told::returned(at(9, 5), "revenu");
        let travelled = told.clone();
        assert_eq!(travelled.at(), at(9, 5));
        assert_eq!(travelled.for_agent(), "[2026-11-12 09:05] revenu");
    }

    /// The follow-up names the tile to refresh, and says nothing when there is
    /// none.
    #[test]
    fn a_follow_up_is_told_which_tile_to_refresh() {
        assert_eq!(with_its_tiles("affiche la carte".into(), &[]), "affiche la carte");
        let told = with_its_tiles(
            "affiche la carte".into(),
            &[("ui://aria/tile/3".into(), "Trajet".into())],
        );
        assert!(told.starts_with("affiche la carte"));
        assert!(told.contains("ui://aria/tile/3 « Trajet »"));
        assert!(told.contains("refresh_tile"));
    }

    /// Only a turn the **user** opened is answered out loud. A session's result
    /// has already been spoken verbatim; the agent is told for context.
    #[test]
    fn only_the_users_own_turn_is_answered_aloud() {
        assert!(Told::said("bonjour").answer_is_spoken());
        assert!(!Told::returned(at(9, 5), "revenu").answer_is_spoken());
    }
}
