//! What marries the agent's process to the conversation.
//!
//! Everything else under [`super`] is a piece that can be tested on its own.
//! This is the wiring, and it is deliberately thin: three tasks, no decisions.
//! Every judgement it could have made is somewhere else — [`super::filler`]
//! decides whether to cover a wait, [`super::delegation`] decides what is worth
//! saying out loud, [`crate::mcp::voice`] decides what the agent may call.
//!
//! ```text
//!    core.heard_turn ──► Conversation::heard ──► to_agent ──► Speaker::say
//!                                                                  │
//!                                              Speech ◄────────── process
//!                                                 │
//!                          ┌──────────────────────┤
//!                          ▼                      ▼
//!                    Delta / Finished         ToolUse
//!                          │                (the MCP server
//!                     speech channel         already handled it)
//!                          │
//!                   Core::spoke ──► the window
//!                          └──────► the TTS, when T4 exists
//! ```
//!
//! # Why the speech channel is drained here and not by the TTS
//!
//! Because there is no TTS yet, and the alternative would be a channel nobody
//! reads — which fills, and then wedges the conversation. So the pilot drains
//! it into the event stream, where the window shows it. When T4 arrives it
//! takes the same channel and the pilot stops publishing from it; the event
//! stays, because a window that reconnects has to see what was said and an
//! audio pipeline cannot be re-read.
//!
//! # What this does not do
//!
//! It does not restart the agent. A CLI that dies takes the post with it and
//! says so in the log; bringing it back is a decision about state — what the
//! journal holds, whether the user is told — and nothing here is equipped to
//! make it.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;

use super::conversation::{Channels, Conversation, Told};
use super::filler::{self, Filler, Waiting};
use super::local::{self, LocalAgent, LocalConfig};
use super::marks::Mark;
use super::sentences;
use super::session::{Config, MainAgent, Speaker, Speech};
use crate::Core;
use crate::audio::VoiceState;

/// The half of the agent the pilot speaks through, whichever engine it is.
///
/// An enum and not a trait object: there are two, the pilot is the only
/// caller, and a `match` says plainly what differs — which is very little.
enum Mouth {
    Cli(Speaker),
    Local(local::Input),
}

impl Mouth {
    async fn say(&mut self, told: &Told) -> Result<()> {
        match self {
            // `for_agent`, never `text`: this is where the date and time are
            // put on the message. See `super::clock`.
            Mouth::Cli(speaker) => {
                if matches!(told, Told::Heard { .. }) {
                    // Nothing in the chain sends sound to the CLI; if it ever
                    // does, the placeholder goes rather than nothing.
                    tracing::warn!("un message vocal envoyé à la CLI, qui n'entend pas");
                }
                speaker.say(&told.for_agent()).await
            }
            // The local engine stamps the message itself, because a heard
            // turn carries its stamp beside the audio rather than in a string.
            Mouth::Local(input) => input.say(told).await,
        }
    }

    /// Stop the answer being written. The CLI only: the local engine has no
    /// such request, and its turn is muted instead — the voice is what the
    /// user hears, and that part is already silent.
    async fn interrupt(&mut self) -> Result<()> {
        match self {
            Mouth::Cli(speaker) => speaker.interrupt().await,
            Mouth::Local(_) => Ok(()),
        }
    }

    async fn stderr(&self) -> String {
        match self {
            Mouth::Cli(speaker) => speaker.stderr().await,
            Mouth::Local(input) => input.stderr().await,
        }
    }

    async fn stop(&mut self) {
        match self {
            Mouth::Cli(speaker) => {
                speaker.stop().await.ok();
            }
            // Dropping the input kills `llama-server`; nothing to wait for.
            Mouth::Local(_) => {}
        }
    }
}

/// A running main agent, wired to the core.
///
/// Dropping it ends the agent: the tasks stop when their channels close, and
/// the process is killed on drop of the [`Speaker`].
pub struct Pilot {
    conversation: Arc<Conversation>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Pilot {
    /// Start the agent, warm it up, and wire it in.
    ///
    /// Returns once the post is ready to be spoken to — which includes the
    /// warm-up turn, because without it the first real utterance is answered
    /// by a model whose MCP servers are still connecting. See
    /// [`MainAgent::warm_up`].
    pub async fn start(core: Arc<Core>, config: Config) -> Result<Self> {
        let mut agent = MainAgent::start(config).await?;
        agent.warm_up().await?;
        tracing::info!(
            "agent principal prêt{}",
            agent
                .native_id()
                .map(|id| format!(" ({id})"))
                .unwrap_or_default()
        );
        let (speaker, speech) = agent.split();
        Ok(Self::wire(core, Mouth::Cli(speaker), speech, None))
    }

    /// The same post on a local model that hears — epic 10.
    ///
    /// Returns once the model is loaded **and warm**: `llama-server` has
    /// answered `/health`, and a first request with the prompt, the tools and a
    /// second of audio has gone through. The user's rule for this epic: when
    /// the loading bar ends, the model answers at full speed.
    pub async fn start_local(core: Arc<Core>, config: LocalConfig) -> Result<Self> {
        let model = config.model.display_name;
        let mut agent = LocalAgent::start(Arc::clone(&core), config).await?;
        agent.warm_up().await?;
        tracing::info!("agent principal prêt (modèle local {model})");
        let (input, speech, task) = agent.split();
        Ok(Self::wire(core, Mouth::Local(input), speech, Some(task)))
    }

    fn wire(
        core: Arc<Core>,
        speaker: Mouth,
        speech: tokio::sync::mpsc::Receiver<Speech>,
        engine: Option<tokio::task::JoinHandle<()>>,
    ) -> Self {
        let (conversation, channels) = Conversation::new(Arc::clone(&core));
        // Attached before the tasks start, so a `delegate_task` arriving on the
        // very first turn finds a conversation to land in.
        core.attach_conversation(&conversation);

        let Channels {
            speech: to_say,
            to_agent,
        } = channels;

        let turn = Arc::new(Turn::new());
        // Shared, because both tasks may now cover a wait — the tool call from
        // one, the backstop timer from the other — and the rotation has to be
        // one rotation. A phrase heard twice running is exactly what makes a
        // machine sound like a machine.
        let filler = Arc::new(Mutex::new(Filler::new()));
        let mut tasks = vec![
            conversation.watch(),
            tokio::spawn(read(
                Arc::clone(&core),
                Arc::clone(&conversation),
                Arc::clone(&turn),
                Arc::clone(&filler),
                speech,
            )),
            tokio::spawn(write(
                Arc::clone(&core),
                Arc::clone(&conversation),
                turn,
                filler,
                speaker,
                to_agent,
            )),
            tokio::spawn(drain(Arc::clone(&core), to_say)),
        ];
        // The local engine's conversation task, aborted with the rest: it owns
        // the history and the HTTP client, and a pilot that is dropped must not
        // leave it answering into a closed channel.
        tasks.extend(engine);

        Self {
            conversation,
            tasks,
        }
    }

    /// The conversation, for a caller that wants the journal or the count.
    pub fn conversation(&self) -> &Arc<Conversation> {
        &self.conversation
    }
}

impl Drop for Pilot {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Which turn is in flight, and whether the model has spoken in it yet.
///
/// Two numbers rather than a lock, because both are read from a timer that
/// must not be able to block the turn it is timing. The counter is what makes
/// a late filler harmless: a timer for turn 3 firing during turn 4 sees the
/// mismatch and says nothing.
struct Turn {
    id: AtomicU64,
    /// The turn the model has already spoken in.
    spoken_in: AtomicU64,
    /// Whether the current turn's answer is ours to say out loud.
    ///
    /// False for a turn the agent took because a *session* came back: its line
    /// has already been spoken verbatim, and what the model adds is a
    /// paraphrase of numbers. See [`super::conversation::Told`].
    aloud: std::sync::atomic::AtomicBool,
    /// The turn a waiting phrase has already gone out in.
    ///
    /// Two things can cover a wait — the tool call and the backstop timer —
    /// and they must not both do it. "Je regarde. Une seconde." is two
    /// machines talking.
    covered_in: AtomicU64,
    /// The turn the user cut off. Nothing more of it is said out loud.
    cut_in: AtomicU64,
    /// The turn that reached for a tool. Such a turn is muted on a cut but
    /// never stopped at the CLI — see [`Turn::may_stop`].
    tool_in: AtomicU64,
    /// The last turn whose `result` came back.
    done_in: AtomicU64,
    /// When the current turn was handed to the agent — the zero of the one
    /// figure epic 10 compares between engines: the first text.
    started: Mutex<std::time::Instant>,
}

impl Turn {
    fn new() -> Self {
        Self {
            id: AtomicU64::new(0),
            spoken_in: AtomicU64::new(0),
            aloud: std::sync::atomic::AtomicBool::new(true),
            covered_in: AtomicU64::new(0),
            cut_in: AtomicU64::new(0),
            tool_in: AtomicU64::new(0),
            done_in: AtomicU64::new(0),
            started: Mutex::new(std::time::Instant::now()),
        }
    }

    /// The user spoke over the current turn. Returns false, and marks
    /// nothing, when that turn is already over: its last words may still be
    /// playing, but the voice drops those itself, and a mark left here would
    /// land on whatever the agent writes next.
    fn cut(&self) -> bool {
        let id = self.current();
        if id == 0 || self.done_in.load(Ordering::SeqCst) >= id {
            return false;
        }
        self.cut_in.store(id, Ordering::SeqCst);
        true
    }

    /// The last turn the user cut off.
    ///
    /// Read by the stream reader as a number it compares with the last one it
    /// acted on, **not** against [`Self::current`]: the next message bumps the
    /// current turn as soon as it is sent, while the old turn's `result` may
    /// still be on its way.
    fn last_cut(&self) -> u64 {
        self.cut_in.load(Ordering::SeqCst)
    }

    fn is_cut(&self) -> bool {
        let id = self.current();
        id != 0 && self.cut_in.load(Ordering::SeqCst) == id
    }

    fn called_tool(&self) {
        self.tool_in.store(self.current(), Ordering::SeqCst);
    }

    /// A `result` came back for turn `id` — or for the current one with `None`.
    fn finished(&self, id: Option<u64>) {
        self.done_in
            .fetch_max(id.unwrap_or_else(|| self.current()), Ordering::SeqCst);
    }

    /// Whether a cut may also stop the turn at the CLI, rather than only
    /// mute it.
    ///
    /// Not once it is over: there is nothing to stop. And **not once it has
    /// called a tool**, which is the case that matters. `delegate_task` starts
    /// the session on our side before the CLI has the answer; interrupted in
    /// between, the model never learns the work left and may send it again —
    /// two sessions, two answers. Muting is enough there: the text after a
    /// delegation is a second announcement nobody wants anyway.
    fn may_stop(&self) -> bool {
        let id = self.current();
        id != 0
            && self.done_in.load(Ordering::SeqCst) < id
            && self.tool_in.load(Ordering::SeqCst) < id
    }

    /// Milliseconds since the current turn was handed over.
    fn elapsed_ms(&self) -> u128 {
        self.started.lock().expect("turn clock poisoned").elapsed().as_millis()
    }

    /// Claim the right to cover this turn, once.
    ///
    /// Returns false if the turn is over, if the model has already spoken, if
    /// this turn is not ours to speak for, or if something has already
    /// covered it. Atomic because the tool call and the timer race, and they
    /// do so on different tasks.
    fn claim_cover(&self, id: u64) -> bool {
        if !self.is_aloud() || !self.still_waiting(id) || self.is_cut() {
            return false;
        }
        // `fetch_max` returns what was there. Below `id` means this turn had
        // not been covered and we have just claimed it; equal means something
        // else got there first.
        self.covered_in.fetch_max(id, Ordering::SeqCst) < id
    }

    fn current(&self) -> u64 {
        self.id.load(Ordering::SeqCst)
    }

    /// A new turn begins. Returns its number.
    fn begin(&self, aloud: bool) -> u64 {
        self.aloud.store(aloud, Ordering::SeqCst);
        *self.started.lock().expect("turn clock poisoned") = std::time::Instant::now();
        self.id.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn is_aloud(&self) -> bool {
        self.aloud.load(Ordering::SeqCst)
    }

    fn model_spoke(&self) {
        self.spoken_in
            .store(self.id.load(Ordering::SeqCst), Ordering::SeqCst);
    }

    /// Whether turn `id` is still the current one and still silent.
    fn still_waiting(&self, id: u64) -> bool {
        self.id.load(Ordering::SeqCst) == id && self.spoken_in.load(Ordering::SeqCst) < id
    }
}

/// Everything the agent says, turned into something the core knows about.
async fn read(
    core: Arc<Core>,
    conversation: Arc<Conversation>,
    turn: Arc<Turn>,
    filler: Arc<Mutex<Filler>>,
    mut speech: tokio::sync::mpsc::Receiver<Speech>,
) {
    // The answer as it is being written, so it can be spoken before it is
    // finished. One task owns it, so a plain local is the whole of the
    // synchronisation. See [`sentences`].
    let mut aloud = sentences::Pending::default();
    // Whether this turn handed work to a session. A delegation is an answer
    // even when nothing follows it aloud — the agent judged the turn was for
    // it — so it counts for decision 119 like a spoken sentence.
    let mut delegated = false;
    // The turn being read was cut off by the user — its number, so `result`
    // can be booked against the right one. See [`Turn::last_cut`] for why
    // this is followed here rather than read off the current turn.
    let mut cut: Option<u64> = None;
    let mut cuts_seen = turn.last_cut();

    while let Some(event) = speech.recv().await {
        // Checked before every event, so the delta that arrives a millisecond
        // after the cut is already silent. Muting keeps accumulating — the
        // journal gets the answer as far as it went — and drops what had not
        // reached the voice yet. Without it the next batch of the old answer
        // came out right after the cut, and the orb showed ARIA speaking over
        // the user (2026-11-23).
        let latest = turn.last_cut();
        if latest > cuts_seen {
            cuts_seen = latest;
            cut = Some(latest);
            aloud.mute();
        }
        match event {
            Speech::Started { tools, .. } => {
                // `init` comes back every turn with the same session id, so
                // this is not "the session started" — it is the CLI repeating
                // itself. Only the surprising case is worth a line, and the
                // surprise is a tool nobody asked for: `--tools` is the only
                // thing standing between a voice dispatcher and a coding
                // agent, and a mistake there is otherwise silent.
                let asked = crate::mcp::voice::allowed_tools();
                let extra: Vec<&str> = tools
                    .iter()
                    .filter(|tool| !asked.contains(tool))
                    .map(String::as_str)
                    .collect();
                if !extra.is_empty() {
                    tracing::warn!(
                        "le poste vocal porte des outils qu'on ne lui a pas donnés : {}",
                        extra.join(", ")
                    );
                }
            }

            // Spoken as they arrive, a whole sentence at a time, which is what
            // `--include-partial-messages` was turned on for. The note here
            // used to say this had to wait for a TTS that could be fed in
            // pieces; decision 56 provided it, and until it was wired the
            // window filled with the answer while ARIA stayed silent until the
            // model had finished writing.
            //
            // Only ever a **complete** sentence, never a fragment: the words
            // already streamed are append-only, so one that has reached its
            // full stop is not going to be rewritten. [`sentences`] carries
            // the rest of the reasoning, including why the pieces stop being
            // small after the first one.
            Speech::Delta(text) => {
                // The figure epic 10 compares between the two engines, logged
                // the same way for both: from the turn handed over to the
                // first word written. Grep « premier texte ».
                if turn.still_waiting(turn.current()) {
                    core.mark(Mark::FirstText {
                        aloud: turn.is_aloud(),
                    });
                    if turn.is_aloud() {
                        tracing::info!(ms = turn.elapsed_ms(), "premier texte de l'agent");
                    } else {
                        tracing::info!(ms = turn.elapsed_ms(), "premier texte de l'agent (reformulation d'un retour)");
                    }
                }
                turn.model_spoke();
                if turn.is_aloud()
                    && let Some(phrase) = aloud.push(&text)
                    // `…` — the agent staying out of a conversation that was
                    // not addressed to it (decision 119). Nothing to say.
                    && !sentences::is_silence(&phrase)
                {
                    conversation.say(phrase).await;
                }
            }

            Speech::ToolUse { name, input } => {
                // The call itself reached `crate::mcp::voice` over MCP and was
                // answered there; this is the trace of it, and the one place
                // the two halves can be seen to agree.
                tracing::info!("l'agent appelle {name} : {input}");
                core.mark(Mark::ToolCalled { name: name.clone() });
                turn.called_tool();

                // And it is the moment to cover the wait — because it is the
                // moment ARIA *knows* there will be one. Work has just gone to
                // a session that takes seconds or minutes.
                //
                // On a timer instead, the phrase fired on every turn,
                // including the ones answered from what the model already
                // knows: « Quel jour est-on ? », « Je regarde. », « On est le
                // mercredi 28 octobre 2026. » Nothing was looked up, and the
                // phrase was a lie about a wait that did not happen.
                let id = turn.current();
                if name.ends_with(crate::agent::tools::DELEGATE_TASK) {
                    delegated = true;
                    // The work has left. Whatever the model writes from here
                    // is a second announcement of a departure already
                    // announced — heard on 2026-10-30 as « Je regarde. » then
                    // « Je recherche en ligne, le résultat arrive dans
                    // quelques secondes. » The prompt asks for silence; this
                    // is what makes it one.
                    //
                    // Only on a delegation, and that is not caution for its
                    // own sake: `run_action` answers in its own result, so
                    // muting after it would throw away the answer rather than
                    // a stutter.
                    aloud.mute();

                    if turn.claim_cover(id) {
                        let phrase = {
                            let mut filler = filler.lock().expect("filler poisoned");
                            filler.grace_expired()
                        };
                        if let Waiting::Say(phrase) = phrase {
                            // Noted before it is said: `delegate_task`'s
                            // answer is being built on another task right now,
                            // and it reads this to tell the model what the
                            // user has already heard.
                            conversation.note_waiting(phrase);
                            conversation.say(phrase).await;
                        }
                    }
                }
            }

            Speech::Finished { text, usage } => {
                let text = text.trim().to_string();
                tracing::debug!(?usage, "tour fini");
                core.mark(Mark::TurnFinished {
                    aloud: turn.is_aloud(),
                    text: text.clone(),
                });
                let silent = sentences::is_silence(&text) && !delegated;
                // Decision 119: an answer to the user's turn — spoken, or a
                // delegation — restarts the wake window and counts against a
                // wake ARIA gave itself. A silence does neither.
                if turn.is_aloud() && !silent {
                    core.answer_spoken();
                }
                if silent && !text.is_empty() {
                    // Not written down either: a « … » in the thread is noise
                    // to whoever reopens it, and the agent loses nothing — the
                    // turn it stayed out of is in the journal as heard.
                    tracing::debug!("l'agent se tait : le tour ne lui était pas adressé");
                } else if !text.is_empty() {
                    // Always written down — the journal is the conversation's
                    // memory whether or not anyone heard it — and said only
                    // when the turn was the user's.
                    conversation.answered(&text);
                    if cut.is_some() {
                        tracing::debug!("tour coupé par l'utilisateur : la fin n'est pas prononcée");
                    } else if turn.is_aloud() {
                        // Only the tail: the sentences already spoken are not
                        // said again, and a final text that is not a
                        // continuation of them is not spoken at all. The
                        // reasoning for that silence is in [`sentences`].
                        match aloud.rest_of(&text) {
                            Some(rest) if !rest.is_empty() => conversation.say(rest).await,
                            Some(_) => {}
                            None => tracing::warn!(
                                "la réponse finale ne prolonge pas ce qui a déjà été dit — \
                                 la fin n'est pas prononcée pour ne pas tout répéter : « {text} »"
                            ),
                        }
                    } else {
                        tracing::debug!(
                            "réponse à un résultat de session, non prononcée : « {text} »"
                        );
                    }
                }
                // Every turn ends here or in `Failed`, including one the user
                // spoke over, so this is the one place the accumulator has to
                // be cleared. Resetting it when a turn *starts* would be worse:
                // a stray `init` mid-turn would set the spoken length back to
                // zero and the whole answer would be read out a second time.
                aloud.reset();
                delegated = false;
                turn.finished(cut);
                // A cut turn leaves the state to whoever cut it: the user is
                // speaking, and `Idle` here would put the orb to rest under
                // them.
                if cut.take().is_none() {
                    core.set_voice_state(VoiceState::Idle);
                }
            }

            Speech::Interrupted(text) => {
                tracing::info!("tour interrompu à la CLI");
                core.mark(Mark::TurnFinished {
                    aloud: turn.is_aloud(),
                    text: format!("interrompu : {text}"),
                });
                // Written down as far as it got, and marked: the CLI keeps the
                // same partial answer in its own context, and a thread reopened
                // later must not read it as something ARIA finished saying.
                let text = text.trim();
                if !text.is_empty() && !sentences::is_silence(text) {
                    conversation.answered(&format!("{text} [interrompue]"));
                }
                aloud.reset();
                delegated = false;
                turn.finished(cut.take());
            }

            Speech::Failed(message) => {
                tracing::error!("l'agent principal a échoué : {message}");
                core.mark(Mark::TurnFinished {
                    aloud: turn.is_aloud(),
                    text: format!("échec : {message}"),
                });
                // Said out loud, because the alternative is a user waiting for
                // an answer that will never come. Not the reason — that is a
                // CLI error string, and reading one aloud is worse than
                // useless.
                if cut.is_none() {
                    conversation.say("Je n'ai pas pu répondre.").await;
                }
                aloud.reset();
                delegated = false;
                turn.finished(cut.take());
                core.set_voice_state(VoiceState::Idle);
            }

            Speech::Quota {
                utilization,
                window,
                warning,
            } => {
                let percent = utilization * 100.0;
                if warning {
                    tracing::warn!("quota {window} à {percent:.0} %");
                } else {
                    tracing::debug!("quota {window} à {percent:.0} %");
                }
            }

            Speech::Unknown(line) => tracing::debug!("ligne non modélisée : {line}"),
        }
    }
    tracing::warn!("le flux de l'agent principal s'est fermé");
}

/// Everything the agent is told, plus the waiting phrase it cannot be fast
/// enough to cover itself.
async fn write(
    core: Arc<Core>,
    conversation: Arc<Conversation>,
    turn: Arc<Turn>,
    filler: Arc<Mutex<Filler>>,
    mut speaker: Mouth,
    mut to_agent: tokio::sync::mpsc::Receiver<Told>,
) {
    loop {
        let told = tokio::select! {
            told = to_agent.recv() => match told {
                Some(told) => told,
                None => break,
            },
            () = conversation.interrupted() => {
                stop_turn(&turn, &mut speaker).await;
                continue;
            }
        };
        let aloud = told.answer_is_spoken();
        let id = turn.begin(aloud);
        core.mark(Mark::TurnBegan { aloud });
        if aloud {
            core.set_voice_state(VoiceState::Thinking);
        }
        // Stamped on the way out — `Told::for_agent` for the CLI, the engine
        // itself for a local model. See `super::clock`.
        if let Err(e) = speaker.say(&told).await {
            tracing::error!(
                "impossible de parler à l'agent principal : {e} — {}",
                speaker.stderr().await
            );
            return;
        }

        // The waiting phrase, on a timer rather than in reaction to the model:
        // T0 measured the model's first text at 1113 ms median, so by the time
        // it arrives the moment the phrase was for is already gone. A timer is
        // the only thing that can be early enough.
        //
        // Awaited rather than spawned, which looks wrong and is not: the loop
        // only turns again when the user says something else, and holding the
        // `Filler` here is what keeps its rotation honest — a phrase heard
        // twice running is exactly what makes a machine sound like a machine.
        // A second utterance inside the grace period waits in the channel.
        //
        // Not for a turn the agent took because a session came back: nobody is
        // waiting on that one, and "Je regarde." after the answer has already
        // been given is the machine talking to itself.
        if !aloud {
            continue;
        }
        tokio::select! {
            () = tokio::time::sleep(filler::PATIENCE) => {}
            // The patience is the one stretch this task is not listening, and
            // it is exactly when a user who changed their mind speaks up.
            () = conversation.interrupted() => {
                stop_turn(&turn, &mut speaker).await;
                continue;
            }
        }
        if turn.claim_cover(id) {
            let phrase = {
                let mut filler = filler.lock().expect("filler poisoned");
                filler.grace_expired()
            };
            if let Waiting::Say(phrase) = phrase {
                conversation.say(phrase).await;
            }
        }
    }
    tracing::info!("plus rien à dire à l'agent principal, arrêt");
    speaker.stop().await;
}

/// The user cut ARIA off: mute the rest of the turn, and stop it at the CLI
/// when that cannot lose work.
///
/// Stopping is what makes the next sentence fast. Without it, a message sent
/// while the old answer is still being written waits in the CLI's queue
/// behind it; measured on 2026-11-23, with the interrupt the next turn starts
/// a few milliseconds later and the model still sees what it had begun to say.
async fn stop_turn(turn: &Turn, speaker: &mut Mouth) {
    if !turn.cut() {
        tracing::debug!("coupure après la fin du tour : rien à arrêter côté agent");
        return;
    }
    if !turn.may_stop() {
        tracing::info!("coupure pendant un appel d'outil : le tour est tu, pas arrêté");
        return;
    }
    match speaker.interrupt().await {
        Ok(()) => tracing::info!("coupure : le tour en cours est arrêté"),
        Err(e) => tracing::warn!("l'arrêt du tour n'a pas pu être envoyé : {e}"),
    }
}

/// What ARIA says, on its way to the window — and to the TTS, when there is
/// one.
async fn drain(core: Arc<Core>, mut to_say: tokio::sync::mpsc::Receiver<String>) {
    while let Some(text) = to_say.recv().await {
        tracing::info!("ARIA : {text}");
        core.spoke(text);
    }
}

/// How to launch the post, with its own tools attached.
///
/// Here rather than at the call site because the three parts have to agree and
/// the disagreement would be silent: a config pointing at one endpoint, a
/// `--tools` list naming another server, and a relay that is never asked.
pub fn config(core: &Core) -> Config {
    let home = core.home();
    Config {
        mcp_config: Some(crate::transport::agent_mcp_config(core.agent_token())),
        allowed_tools: crate::mcp::voice::allowed_tools(),
        permission_tool: Some(crate::mcp::voice::permission_tool_name()),
        // Decision 106. `None` until `app/` has resolved it, which is the
        // state in every test here: no file, no flag.
        instructions_file: home.main,
        // Decision 108: `~/.aria`, the folder the user's own ARIA files are
        // already in. Falling back to the throwaway directory rather than
        // refusing to start, because a home directory that cannot be resolved
        // must cost a path and not the assistant.
        cwd: home.dir.unwrap_or_else(Config::fallback_cwd),
        ..Config::default()
    }
}

/// The same, with what the user set over it.
///
/// Separate from [`config`] rather than a parameter, so the three parts that
/// have to agree — endpoint, tool list, relay — stay in one place and a caller
/// overriding the binary cannot accidentally drop them.
///
/// `persona` is **added** to the shipped prompt, never substituted for it. The
/// shipped lines are what make the mechanism work; a setting that replaced
/// them would be a setting that breaks the product silently.
pub fn config_with(core: &Core, command: &str, model: &str, persona: &str) -> Config {
    let mut config = config(core);
    if !command.trim().is_empty() {
        config.command = command.trim().to_string();
    }
    if !model.trim().is_empty() {
        config.model = model.trim().to_string();
    }
    config.persona = persona.trim().to_string();
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three parts of the launch that have to agree, and would fail
    /// silently if they did not: the model would simply have no tools.
    #[test]
    fn the_launch_points_every_part_at_the_same_server() {
        let core = Core::new();
        let config = config(&core);
        let mcp = config.mcp_config.expect("the endpoint is configured");
        assert!(mcp.contains(core.agent_token()), "{mcp}");
        assert!(mcp.contains(crate::transport::AGENT_PATH), "{mcp}");
        assert_eq!(
            config.allowed_tools,
            vec![
                "mcp__aria__delegate_task",
                "mcp__aria__recall",
                "mcp__aria__deep_recall",
                "mcp__aria__remember",
                "mcp__aria__str_replace",
                "mcp__aria__rename_page",
            ],
            "the memory tools joined the catalogue on 2026-11-11 (decision 91), \
             and this list is derived from it"
        );
        assert_eq!(
            config.permission_tool.as_deref(),
            Some("mcp__aria__approve")
        );
    }

    /// A wait is covered **once**, by whichever of the two gets there first.
    ///
    /// The tool call and the backstop timer race, on different tasks. Both
    /// speaking gives "Je regarde. Une seconde." — two machines talking.
    #[test]
    fn a_wait_is_covered_once_and_only_once() {
        let turn = Turn::new();
        let id = turn.begin(true);
        assert!(turn.claim_cover(id), "the first claimant covers it");
        assert!(!turn.claim_cover(id), "the second finds it already covered");

        // A new turn is a new chance.
        let next = turn.begin(true);
        assert!(turn.claim_cover(next));
    }

    /// Nothing covers a turn the model has already answered, nor one that was
    /// never ours to speak for.
    #[test]
    fn a_turn_that_needs_no_cover_gets_none() {
        let turn = Turn::new();
        let id = turn.begin(true);
        turn.model_spoke();
        assert!(
            !turn.claim_cover(id),
            "the model got there first — this is the date question, and \
             « Je regarde. » before an answer it already knew is a lie about a \
             wait that did not happen"
        );

        // And a turn taken because a session came back is not spoken for at
        // all: its line has already been said verbatim.
        let returned = turn.begin(false);
        assert!(!turn.claim_cover(returned));
    }

    /// A cut stops the turn at the CLI only while that cannot lose work.
    ///
    /// The case this guards: « la météo à Paris », the model calls
    /// `delegate_task`, the user says « et le trajet aussi » over it. Stopped
    /// between the call and its answer, the model never learns the weather
    /// left and may send it again. Muted, nothing is lost.
    #[test]
    fn a_turn_that_called_a_tool_is_muted_and_never_stopped() {
        let turn = Turn::new();
        turn.begin(true);
        assert!(turn.cut(), "a running turn can be cut");
        assert!(turn.may_stop(), "and, having called nothing, stopped");

        turn.begin(true);
        turn.called_tool();
        assert!(turn.cut(), "still cut — the voice goes quiet");
        assert!(!turn.may_stop(), "but the CLI is left to finish");
    }

    /// A cut that lands after the `result` marks nothing, or it would mute
    /// whatever the agent writes next.
    #[test]
    fn a_cut_after_the_turn_ended_marks_nothing() {
        let turn = Turn::new();
        let id = turn.begin(true);
        turn.finished(None);
        assert!(!turn.cut());
        assert!(turn.last_cut() < id);
        assert!(!turn.may_stop());
    }

    /// The result of a cut turn is booked against that turn even when the
    /// next one has already been sent — the case where a tool turn is muted
    /// and left to finish while the user's new sentence goes out.
    #[test]
    fn a_cut_turn_is_booked_against_itself() {
        let turn = Turn::new();
        let first = turn.begin(true);
        assert!(turn.cut());
        let second = turn.begin(true);
        turn.finished(Some(first));
        assert!(turn.may_stop(), "the second turn is still running: {second}");
        assert!(!turn.is_cut(), "and nobody cut it");
    }

    /// No waiting phrase over a turn the user cut off.
    #[test]
    fn a_cut_turn_gets_no_cover() {
        let turn = Turn::new();
        let id = turn.begin(true);
        assert!(turn.cut());
        assert!(!turn.claim_cover(id));
    }

    /// A late timer must not speak into the turn after the one it was timing.
    #[test]
    fn a_waiting_phrase_for_a_turn_that_is_over_is_dropped() {
        let turn = Turn::new();
        let first = turn.begin(true);
        assert!(turn.still_waiting(first));

        // The model answers, so the cover is no longer wanted.
        turn.model_spoke();
        assert!(!turn.still_waiting(first));

        // And a timer from the previous turn stays quiet in the next one.
        let second = turn.begin(true);
        assert!(turn.still_waiting(second));
        assert!(!turn.still_waiting(first));
    }
}
