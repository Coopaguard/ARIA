//! Where what ARIA says becomes a sound.
//!
//! The other half of [`super`], and it meets the core the same way: through
//! the **event stream**, never through a call. The pilot publishes
//! `Event::Spoken`, this reads it and plays it.
//!
//! That is not indirection for its own sake. `aria-core` must keep building
//! without an ONNX runtime or an audio device — the whole reason `aria-voice`
//! is a separate crate — so the core cannot hold a loudspeaker. It publishes;
//! the shell listens. The same shape as the microphone, in the other
//! direction.
//!
//! It also pays for itself twice over: the window shows the same events, so a
//! reconnecting client sees what was said without the audio pipeline being
//! involved at all.
//!
//! # Barge-in, in two halves
//!
//! Decision 17: when the user speaks over ARIA, playback stops *inside the
//! frame*. Not a fade, not the end of the sentence.
//!
//! This file used to claim that [`VoiceState::Listening`] arrives "as soon as
//! the microphone thread has voiced audio". It does not, and never did: it is
//! published by `super::act`, which only runs on a **closed segment** — so the
//! person interrupting had to finish their phrase *and* go quiet for
//! `silence_ms` before anything happened. Somebody talking continuously for two
//! seconds was talked over for two seconds, and the comment is a fair part of
//! why nobody went looking.
//!
//! So there are two signals now, which is the shape `unmute` arrived at too:
//!
//! | Signal | Where | When |
//! |---|---|---|
//! | unexplained energy on the microphone | [`super::Barge`], on the capture thread | within [`aria_voice::echo::BARGE_FRAMES`] of the first syllable |
//! | a transcript at the next pause | [`watch_for_barge_in`], below | certain, and late |
//!
//! The fast one cuts directly, because it already holds the [`Silencer`]. The
//! slow one stays exactly as it was: it costs nothing, it catches what the
//! acoustic gate declines to judge, and `aria_voice::echo::Coupling::Unresolved`
//! means *declining to judge* is the normal state for the first two seconds of
//! every answer.
//!
//! **That takes two threads, and the second one is not decoration.** The
//! thread that synthesises is inside a blocking call for as long as a sentence
//! takes — seconds, with a local model — so a barge-in it reads afterwards is
//! no longer a barge-in. [`watch_for_barge_in`] therefore has its own
//! subscriber and does nothing else, which is what makes it able to answer
//! while the other is working.
//!
//! What is deliberately **not** here any more: the echo gate. It is wired as of
//! 2026-11-03, but on the *capture* thread ([`super::Barge`]), because that is
//! where the microphone frames are. This side provides what it needs —
//! [`Player::reference`] and the 20 ms envelope beside it, an exact record of
//! what went to the speakers rather than what we think went there — and the
//! [`Silencer`] that lets the decision be acted on.
//!
//! This header used to say that wiring it "belongs with a measurement on a
//! machine that has loudspeakers". That was right about the measurement and
//! wrong about the conclusion: `echo::Coupling` makes the unmeasured case a
//! *state* rather than a blocker, and in that state nothing is cut at all.

use std::sync::{Arc, OnceLock, mpsc};

/// How often the speech thread, with nothing to say, checks whether the last
/// sentence has finished playing.
///
/// It used to check only when an event arrived. But `say` returns when the
/// sentence is **queued**, not when it has been heard, and after the last one
/// nothing else arrives: the window kept showing « parle » over a silent ARIA
/// until something unrelated woke the thread (2026-11-23). A tenth of a second
/// is under what the eye reads as a lag, and one lock per tick costs nothing.
const SETTLE_POLL: std::time::Duration = std::time::Duration::from_millis(100);

use aria_core::Core;
use aria_core::audio::{Load, VoiceState};
use aria_core::events::Event;
use aria_voice::echo::SpokenLog;
use aria_voice::tts::kyutai as kyutai_engine;
use aria_voice::tts::phonon as phonon_engine;
use aria_voice::tts::{Player, Playing, Silencer, Voice};

use crate::config::{Engine, Voice as Setting};

/// Start the voice, on its own thread.
///
/// Never fatal, which is the same rule as capture: a machine with no output
/// device, or a voice model that will not load, leaves ARIA answering in writing. An
/// assistant that refuses to start because a loudspeaker is missing is an
/// assistant nobody gets to configure.
pub fn spawn(
    core: Arc<Core>,
    setting: Setting,
    speaker: Arc<OnceLock<Silencer>>,
    // What goes to the loudspeaker, written here and read by the microphone
    // thread — the slow half of the echo defence. See [`SpokenLog`].
    spoken: Arc<SpokenLog>,
) -> Retuner {
    let (tx, rx) = mpsc::channel();
    forward_events(&core, tx.clone(), Arc::clone(&speaker));
    let retuner = Retuner { tx: Some(tx.clone()) };
    std::thread::Builder::new()
        .name("aria-speech".into())
        .spawn(move || {
            // Registered before the read and cleared after it, whatever the
            // outcome — including a failure, because a bar that never fills
            // is a worse symptom than a voice that did not start. Every
            // engine left reads a model, so the step is always there.
            core.begin_load(Load::Voice);
            let player = match open(&setting).and_then(Player::open) {
                Ok(player) => Some(player),
                // No fallback since the Windows voice was removed
                // (2026-11-19): ARIA answers in writing, and the log says why.
                Err(e) => {
                    tracing::error!("{:?} n'a pas démarré : {e:#}", setting.engine);
                    None
                }
            };
            // Inside the load window, and that is the point of putting it here
            // rather than letting the first turn pay it: priming is five
            // syntheses, and the first of them is also this engine's first
            // inference. Behind a bar nobody is waiting on it; on the first
            // turn it would land on the one phrase whose job is to cover a
            // wait.
            if let Some(player) = player.as_ref() {
                player.prime(aria_core::agent::filler::phrases());
            }
            core.finished_load(Load::Voice);
            let mut speech = Speech {
                core,
                player: None,
                speaker,
                spoken,
                current: setting.clone(),
                wanted: setting,
                tx,
            };
            match player {
                Some(player) => speech.adopt(player),
                // Not the end of the thread: a voice chosen in the settings
                // is another chance to open one.
                None => tracing::error!("ARIA restera muette jusqu'au choix d'une autre voix."),
            }
            speech.run(rx);
        })
        .expect("the speech thread can be named");
    retuner
}

/// What the speech thread is told, in the order it is told.
///
/// One channel for both, so a voice change lands **between two sentences** by
/// construction: the thread is either saying something or reading this.
enum Told {
    /// An event, with the queue's generation when it was published — `None`
    /// before a voice exists. See [`Player::generation`].
    Event(Event, Option<u64>),
    /// The settings changed the voice.
    Retune(Setting),
    /// A voice opened on another thread, for this setting.
    Opened(Setting, anyhow::Result<Box<dyn Voice>>),
}

/// The settings screen's way to reach the speech thread.
///
/// Decision 123: a voice chosen in the settings is heard at the next sentence,
/// not at the next start-up. Managed by Tauri, used by `set_config`.
pub struct Retuner {
    tx: Option<mpsc::Sender<Told>>,
}

impl Retuner {
    /// For a launch without a voice: changes go nowhere, and say so.
    pub fn none() -> Self {
        Self { tx: None }
    }

    pub fn retune(&self, setting: Setting) {
        match &self.tx {
            Some(tx) => {
                let _ = tx.send(Told::Retune(setting));
            }
            None => tracing::warn!("aucune voix en marche : le changement attendra le redémarrage"),
        }
    }
}

/// Copy the stream onto the speech thread's channel.
///
/// The speech thread used to read the broker itself. It now has two sources —
/// the stream and the settings — and a thread blocked on one cannot hear the
/// other, so this reads the stream and posts it.
///
/// Each event is stamped with the queue's generation **as it is read off the
/// stream**, which is the closest point to when it was published. The speech
/// thread is blocked inside every sentence, so what waits in its channel was
/// often published before a cut and is said after it; the stamp is how it
/// tells (2026-11-23).
fn forward_events(core: &Arc<Core>, tx: mpsc::Sender<Told>, speaker: Arc<OnceLock<Silencer>>) {
    let mut events = core.broker().subscribe();
    std::thread::Builder::new()
        .name("aria-speech-events".into())
        .spawn(move || {
            loop {
                match events.blocking_recv() {
                    Ok(envelope) => {
                        let generation = speaker.get().map(Silencer::generation);
                        if tx.send(Told::Event(envelope.event, generation)).is_err() {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        // A missed event may have been something to say. Loud,
                        // because the symptom is ARIA skipping an answer and
                        // nothing else explaining it.
                        tracing::warn!(missed, "la voix a pris du retard sur le flux");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        })
        .expect("the speech events thread can be named");
}

/// The chosen engine. Kyutai is suspended, so it resolves to Phonon here too —
/// `Config::load` already does it, this covers a setting saved since.
fn open(setting: &Setting) -> anyhow::Result<Box<dyn Voice>> {
    match setting.engine.in_force() {
        Engine::Kyutai => kyutai(setting),
        Engine::Phonon => phonon(setting),
    }
}

/// Phonon, in this process.
///
/// The wait registered around this call is a model load of about a quarter of
/// a second — or a download, the first time. It used to be the only engine
/// whose `Voice::stream` did something; since decision 56 Kyutai streams too.
fn phonon(setting: &Setting) -> anyhow::Result<Box<dyn Voice>> {
    let setup = phonon_setup(setting);
    Ok(Box::new(phonon_engine::PhononVoice::open(&setup)?))
}

fn phonon_setup(setting: &Setting) -> phonon_engine::Setup {
    phonon_engine::Setup {
        model: setting.phonon.model.clone(),
        voice: setting.phonon.voice.clone(),
        language: setting.language.clone(),
    }
}

/// Kyutai Pocket TTS, loaded into this process.
///
/// This is the call that takes seconds — minutes on a first run, while the
/// weights come down from Hugging Face. [`Load::Voice`] is registered around
/// it by the caller, which is what puts the wait on screen.
///
/// Cheap when the settings screen has already asked for the same model: the
/// weights are held by `kyutai`'s own registry and only the voice prompt is
/// read here. That is what replaced the warm server of decision 50 — one copy
/// in memory, shared, without a child to supervise.
fn kyutai(setting: &Setting) -> anyhow::Result<Box<dyn Voice>> {
    let setup = kyutai_engine::Setup {
        language: kyutai_engine::model_for(&setting.language).to_string(),
        voice: setting.kyutai.voice.clone(),
        quantize: setting.kyutai.quantize,
    };
    Ok(Box::new(kyutai_engine::KyutaiVoice::open(&setup)?))
}

/// Watch for the user speaking over ARIA, on a thread that is never busy.
///
/// # Why this is not an arm of the loop below, where it used to be
///
/// It was, and it did not work. [`speak`] synthesises **on the thread that
/// reads the events**: `player.say` is a blocking call that lasts as long as
/// the sentence takes, and with a local model that is seconds. A
/// [`VoiceState::Listening`] published in the middle of it sat in the channel
/// until the sentence was finished and the cut arrived too late to be one —
/// decision 17 asks for the frame, not the paragraph.
///
/// So the barge-in gets its own subscriber and its own thread, which does
/// nothing else and therefore always answers. It holds a [`Silencer`] and not
/// the player: the authority to stop, without the ability to queue.
///
/// Nothing is raced by this. The sentence being generated discovers at its
/// next chunk that the queue is no longer its own and abandons the rest, which
/// is exactly what the generation counter in `aria-voice` exists for.
fn watch_for_barge_in(core: Arc<Core>, silencer: Silencer) {
    std::thread::Builder::new()
        .name("aria-barge-in".into())
        .spawn(move || {
            let mut events = core.broker().subscribe();
            loop {
                let envelope = match events.blocking_recv() {
                    Ok(envelope) => envelope,
                    // Loud, and then nothing else: the missed event may have
                    // been the barge-in, but a cut on a guess would stop ARIA
                    // mid-sentence for no reason the user can see.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                        tracing::warn!(missed, "la veille d'interruption a pris du retard");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                };

                // The slow half, and the honest description of it: this
                // arrives from `super::act`, which runs on a closed segment
                // — so the cut lands after the interrupting phrase, not
                // during it. `super::Barge` is what gets there first; this
                // is what catches the cases it declined to judge.
                match envelope.event {
                    Event::VoiceState {
                        state: VoiceState::Listening,
                    } if silencer.state() == Playing::Speaking => {
                        tracing::info!("on me parle dessus — je me tais");
                        silencer.silence();
                        core.interrupt_agent();
                    }
                    // Someone else decided — STOP, or the fast half that
                    // already silenced and is telling the rest. Silencing
                    // twice costs a counter; not silencing is ARIA talking
                    // through the button meant to stop her.
                    Event::VoiceState {
                        state: VoiceState::Interrupted,
                    } => silencer.silence(),
                    _ => {}
                }
            }
        })
        .expect("the barge-in thread can be named");
}

/// The speech thread's state: the player, and which voice it should have.
struct Speech {
    core: Arc<Core>,
    /// `None` when no voice opened; a later choice gets another try.
    player: Option<Player>,
    speaker: Arc<OnceLock<Silencer>>,
    spoken: Arc<SpokenLog>,
    /// The voice the player speaks in.
    current: Setting,
    /// The last voice asked for. Differs from `current` while one loads, and
    /// a load that finishes for anything else is dropped: the user picked
    /// twice, and only the second one counts.
    wanted: Setting,
    /// For the loader threads to post back on.
    tx: mpsc::Sender<Told>,
}

impl Speech {
    fn run(&mut self, rx: mpsc::Receiver<Told>) {
        loop {
            match rx.recv_timeout(SETTLE_POLL) {
                Ok(Told::Event(event, generation)) => self.event(event, generation),
                Ok(Told::Retune(setting)) => self.retune(setting),
                Ok(Told::Opened(setting, voice)) => self.opened(setting, voice),
                // Nothing to say: the moment to notice the last sentence has
                // finished playing. See [`SETTLE_POLL`].
                Err(mpsc::RecvTimeoutError::Timeout) => self.settle(),
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Back to idle once the queue is spent.
    fn settle(&self) {
        if let Some(player) = self.player.as_ref()
            && player.state() == Playing::Silent
            && self.core.voice_state() == VoiceState::Speaking
        {
            self.core.set_voice_state(VoiceState::Idle);
        }
    }

    fn event(&mut self, event: Event, generation: Option<u64>) {
        let Some(player) = self.player.as_ref() else {
            return;
        };
        let core = &self.core;
        // One event, since the barge-in moved to its own thread: this one only
        // says things. The call blocks for the whole sentence, which is the
        // reason the other thread exists.
        if let Event::Spoken { text } = &event
            && generation.is_some_and(|g| g != player.generation())
        {
            // Published before the user cut ARIA off, read after. Said, it was
            // ARIA resuming the old answer — or, with the user still talking,
            // being cut again at every sentence while the window showed her
            // speaking (2026-11-23).
            tracing::debug!("phrase d'une réponse interrompue, abandonnée : « {text} »");
        } else if let Event::Spoken { text } = event {
            core.set_voice_state(VoiceState::Speaking);
            // Recorded **before** the synthesis, which blocks for the whole
            // sentence: an echo of the first syllable can be transcribed while
            // we are still inside `say`, and a record written on the far side
            // of that call would arrive after the transcript it has to veto.
            self.spoken.push(&text);
            match player.say(&text) {
                Ok(took) => tracing::debug!("synthèse en {} ms", took.as_millis()),
                Err(e) => tracing::error!("la synthèse a échoué : {e:#}"),
            }
        }

        self.settle();
    }

    /// Decision 123. Same model: the voice is swapped on it, now. Another
    /// model: it loads on its own thread while the old one keeps talking.
    fn retune(&mut self, setting: Setting) {
        // The same setting is a no-op only while a voice is speaking. With
        // none — the French model refused for want of a Hugging Face token,
        // the first-run case (T12) — asking again is how it gets another try.
        if setting == self.wanted && self.player.is_some() {
            return;
        }
        self.wanted = setting.clone();
        let same_model = setting.engine.in_force() == self.current.engine.in_force()
            && setting.language == self.current.language
            && setting.phonon.model == self.current.phonon.model
            && setting.kyutai == self.current.kyutai;
        if same_model && let Some(player) = self.player.as_mut() {
            match player.revoice(&setting.phonon.voice) {
                Ok(true) => {
                    player.prime(aria_core::agent::filler::phrases());
                    tracing::info!("voix : {}", player.voice_name());
                    self.current = setting;
                    return;
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::error!("la voix choisie ne charge pas, l'ancienne reste : {e:#}");
                    return;
                }
            }
        }
        tracing::info!("modèle de voix remplacé : chargement en arrière-plan");
        let tx = self.tx.clone();
        let _ = std::thread::Builder::new()
            .name("aria-voice-load".into())
            .spawn(move || {
                let voice = open(&setting);
                let _ = tx.send(Told::Opened(setting, voice));
            });
    }

    fn opened(&mut self, setting: Setting, voice: anyhow::Result<Box<dyn Voice>>) {
        if setting != self.wanted {
            tracing::debug!("une voix chargée pour un choix déjà remplacé est ignorée");
            return;
        }
        let voice = match voice {
            Ok(voice) => voice,
            Err(e) => {
                tracing::error!("le nouveau modèle de voix ne charge pas, l'ancien reste : {e:#}");
                return;
            }
        };
        match self.player.as_mut() {
            Some(player) => {
                player.replace_voice(voice);
                player.prime(aria_core::agent::filler::phrases());
            }
            None => match Player::open(voice) {
                Ok(player) => {
                    player.prime(aria_core::agent::filler::phrases());
                    self.adopt(player);
                }
                Err(e) => {
                    tracing::error!("pas de sortie audio : {e:#}");
                    return;
                }
            },
        }
        self.current = setting;
    }

    /// Hand a freshly opened player its two neighbours.
    fn adopt(&mut self, player: Player) {
        tracing::info!("voix : {}", player.voice_name());
        // This thread is blocked inside every synthesis, so the barge-in
        // needs one of its own that always answers.
        watch_for_barge_in(Arc::clone(&self.core), player.silencer());
        // The same authority, handed to the microphone thread, which is where
        // the *fast* half of barge-in lives: it has the frames and the
        // calibration, and now it has the right to act on them. See
        // `super::Barge`.
        let _ = self.speaker.set(player.silencer());
        self.player = Some(player);
    }
}
