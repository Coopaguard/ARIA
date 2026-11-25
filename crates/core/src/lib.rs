//! ARIA core — everything that runs without a window.
//!
//! Sleep is not shutdown: the core and its sessions keep running when every
//! window is closed, and only the audio models are unloaded. Nothing in this
//! crate may depend on the shell, which is why it does not depend on Tauri.
//!
//! The rules are in `AGENTS.md`, what is left to do in `docs/ROADMAP.md`.
//!
//! # The shape of it
//!
//! [`Core`] is the only handle. It owns the state the interface renders and it
//! owns the single ordered event stream — and those two facts are the same
//! fact: **every mutation goes through a method that publishes its event**.
//! Nothing changes state quietly. That is what makes the activity feed, the
//! audit journal and a reconnecting window agree with each other, rather than
//! three readers of the same state drifting apart.
//!
//! ```text
//!   Core
//!     ├── events      the ordered stream, with replay          (the original specification)
//!     ├── sessions    who is working, under which profile      (epics 04, 05)
//!     ├── permissions pending requests, one answer each        (decisions 6, 7, 8)
//!     ├── audio       the three listening modes                (decision 19)
//!     └── transport   the WebSocket the window reads            (decision 13)
//! ```
//!
//! Modules arrive when they are needed, not before — `AGENTS.md` asks for
//! boundaries that are observed rather than bet on. `orchestrator` and
//! `actions` are the two still empty.

use std::sync::{Arc, Mutex};

use serde::Serialize;

pub mod actions;
pub mod agent;
pub mod audio;
pub mod events;
pub mod mcp;
pub mod orchestrator;
pub mod permissions;
pub mod process;
pub mod providers;
pub mod sessions;
pub mod store;
pub mod tiles;
pub mod transport;

use audio::{
    AudioGate, AudioMode, GateRefusal, Load, LoadRegistry, Loading, ModeChange, ModeTrigger,
    VoiceState,
};
use events::{Broker, Event, Seq};
use permissions::registry::PatternRegistry;
use permissions::{
    AnswerRefusal, Channel, Decision, PermissionDesk, PermissionRequest, Profile, RequestId,
};
use providers::{AgentProvider, Availability, LaunchSpec, ProfileSupport};
use sessions::supervisor::Supervisor;
use sessions::{ActivityStep, Session, SessionId, SessionRegistry, SessionState};

/// Everything the interface needs to draw itself from nothing.
///
/// The event stream covers "what changed"; this covers "where we are". Both are
/// needed, and the case that proves it is a client that reconnects after being
/// away longer than the replay ring holds: it gets an [`events::Resume::Gap`],
/// and without a snapshot its only honest option would be a blank screen —
/// which is the thing the original specification set out to avoid.
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    /// The sequence this snapshot is current as of. Events after it still
    /// apply; events up to it are already folded in.
    pub seq: Seq,
    pub mode: AudioMode,
    pub voice: VoiceState,
    /// How far the models are, so a window that connects mid-load shows the
    /// bar rather than an orb that looks idle and is deaf.
    pub loading: Loading,
    /// The default profile, as set in the settings screen.
    pub profile: Profile,
    pub sessions: Vec<Session>,
    pub pending: Vec<PermissionRequest>,
    /// Whether the tiles' second origin is actually being served.
    ///
    /// A tile whose sandbox is not up renders the browser's own « refused to
    /// connect » page inside the panel, which reads as « the tiles are
    /// broken » when the truth is « the port was taken ». The window needs to
    /// be able to say which — see `tiles::serving`.
    pub tiles_served: bool,
}

/// What the settings screen shows about the active engine.
#[derive(Clone, Debug, Serialize)]
pub struct ProviderInfo {
    pub id: String,
    pub display_name: String,
    /// A CLI, a local engine, an API. What the dropdown groups by.
    pub kind: providers::EngineKind,
    /// Checked now, not remembered: the CLI updates itself.
    pub availability: Availability,
    /// Profile by profile, with `native: None` where the provider has no
    /// equivalent. The screen says which are missing rather than offering them.
    pub mapping: Vec<ProfileSupport>,
    /// What this engine cannot do, in French, one line each.
    ///
    /// Decision 5's rule one level up: a *capability* the user cannot see
    /// missing is as much a trap as a profile silently absent. An engine that
    /// cannot be asked for permission stops showing cards, and that has to be
    /// read somewhere rather than noticed.
    pub shortcomings: Vec<String>,
}

/// The core, as the shell holds it.
///
/// Cheap to clone behind an `Arc`, and every method takes `&self`: the mutexes
/// are inside, and none is ever held across an await.
pub struct Core {
    broker: Broker,
    sessions: Mutex<SessionRegistry>,
    permissions: Mutex<PermissionDesk>,
    audio: Mutex<AudioGate>,
    voice: Mutex<VoiceState>,
    /// Which models are still being read into memory.
    ///
    /// A registry and not a flag, because two threads load in parallel and
    /// neither knows about the other — see [`LoadRegistry`] for the defect
    /// that taught us.
    loading: Mutex<LoadRegistry>,
    /// Whether the ear transcribes or hands over the sound — epic 10.
    ear: Mutex<audio::Ear>,
    /// What the main agent does, timestamped, for a bench. See
    /// [`agent::marks`].
    marks: tokio::sync::broadcast::Sender<agent::marks::Marked>,
    /// How loud the microphone is, right now, 0 to 1.
    ///
    /// **Not an event, and deliberately not on the broker.** A level meter is
    /// a signal, not a change of state: it arrives fifty times a second, it is
    /// worthless one frame later, and putting it on the ordered stream would
    /// push every real event out of a 4096-slot replay ring in eighty seconds.
    ///
    /// A `watch` channel instead — it keeps only the latest value, a slow
    /// reader misses frames rather than building a backlog, and that is the
    /// correct behaviour for a meter. Nothing is audited and nothing replays.
    level: tokio::sync::watch::Sender<f32>,
    profile: Mutex<Profile>,
    /// The engine sessions are delegated to.
    ///
    /// Behind a trait object, and epic 03 says why that is worth the
    /// indirection with one implementation: the settings screen asks the
    /// *active* provider for its profile table, so a second engine is a
    /// mapping to declare rather than interface code to change.
    ///
    /// `None` until one is attached. The core is useful without it — the audio
    /// states, the permission desk and the stream all work — so a missing
    /// provider disables launching, not the application.
    provider: Mutex<Option<Arc<dyn AgentProvider>>>,
    /// The long-term memory, once `app/` has handed over its root.
    ///
    /// `Option` for the same reason as the provider above: the core receives
    /// its root rather than discovering it (decision 94), because the path API
    /// that knows where `~` is belongs to Tauri. Between start-up and that
    /// call there is no memory, and every operation on it answers so — a tool
    /// that says "aucun dossier de mémoire n'est attaché" is readable; a panic
    /// in a speaking turn is not.
    ///
    /// Behind a plain `Mutex` and never held across an await: every operation
    /// in [`store::Memory`] is synchronous — files and SQLite — which is what
    /// makes that safe to state rather than hope.
    memory: Mutex<Option<store::Memory>>,
    /// The conversations, their project sessions, and the audit — `aria.db`.
    ///
    /// Optional for the same reason the memory is: `app/` resolves the path,
    /// and a run with nowhere to write still holds a conversation. What is lost
    /// is the saving, never the talking.
    history: Mutex<Option<store::history::History>>,
    /// Which thread the live conversation writes into.
    ///
    /// Separate from the conversation itself, and deliberately: the journal is
    /// held by `agent::conversation`, which has no business knowing about
    /// SQLite, while the thread a turn lands in changes the moment the user
    /// clicks another line in the menu.
    thread: Mutex<Option<store::history::ConversationId>>,
    /// The running processes.
    ///
    /// Behind an `Arc` so the main agent's conversation delegates through the
    /// **same** supervisor. It used to take one of its own, and that was a
    /// defect waiting to be found: sessions opened by `delegate_task` would not
    /// have been in the registry `stop_all_sessions` walks, so `STOP` would
    /// have left them running with nothing on screen to stop them with.
    supervisor: Arc<Supervisor>,
    /// The two-column registry: auto-approved or not, and the risk level.
    registry: Mutex<PatternRegistry>,
    /// Which session is behind which relay endpoint.
    routes: Arc<crate::mcp::SessionRoutes>,
    /// Every tile this run of the application has produced.
    ///
    /// Decision 11: tiles die with the application, so this is the only place
    /// one exists — no table, no file. Shared rather than borrowed because the
    /// sandbox origin serves from it on its own task.
    tiles: Arc<crate::tiles::Registry>,
    /// The conversation the main agent's MCP endpoint hands work to.
    ///
    /// **Weak, and that is load-bearing**: the conversation holds an `Arc` of
    /// this core, so a strong handle here would be a cycle and neither would
    /// ever be dropped. The core is the longer-lived of the two — the
    /// conversation is restarted when the agent is — so the weak side belongs
    /// here.
    ///
    /// `None` until one is attached, exactly like [`Self::provider`]: the
    /// endpoint is mounted from start-up and answers honestly before there is
    /// anything behind it.
    conversation: Mutex<Option<std::sync::Weak<agent::conversation::Conversation>>>,
    /// What `~/.aria` holds for the agents: the two instruction files the user
    /// may edit (decision 106), and the two directories the agents run in
    /// (decision 108).
    ///
    /// Same rule as the memory root above: **the core receives the paths, it
    /// does not discover them** — `~` belongs to Tauri's path API, which lives
    /// in `app/`. What the core owns is the *text* a missing file is created
    /// from ([`agent::instructions`]), because that is product knowledge.
    ///
    /// Held here rather than passed at each call site because
    /// [`sessions::Supervisor::launch`] is what fills a
    /// [`providers::LaunchSpec`] in, and it is reached from `delegate_task` as
    /// well as from the window. Empty is the honest state before `app/` has
    /// resolved them, and costs the paths, not the sessions.
    home: Mutex<agent::instructions::Home>,
    /// The token on the main agent's own MCP endpoint, minted once per run.
    ///
    /// Not the window's [`transport::SessionToken`] and not a session's relay
    /// token: one token, one audience. This one authorises *starting work on
    /// the machine*, so the window — a web page — has no business holding it.
    agent_token: String,
}

impl Core {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            broker: Broker::new(),
            sessions: Mutex::new(SessionRegistry::new()),
            permissions: Mutex::new(PermissionDesk::new()),
            audio: Mutex::new(AudioGate::new()),
            voice: Mutex::new(VoiceState::Idle),
            loading: Mutex::new(LoadRegistry::default()),
            ear: Mutex::new(audio::Ear::default()),
            marks: tokio::sync::broadcast::Sender::new(256),
            level: tokio::sync::watch::Sender::new(0.0),
            profile: Mutex::new(Profile::default()),
            provider: Mutex::new(None),
            memory: Mutex::new(None),
            history: Mutex::new(None),
            thread: Mutex::new(None),
            supervisor: Arc::new(Supervisor::new()),
            registry: Mutex::new(PatternRegistry::with_defaults()),
            routes: Arc::new(crate::mcp::SessionRoutes::new()),
            tiles: Arc::new(crate::tiles::Registry::default()),
            conversation: Mutex::new(None),
            home: Mutex::new(agent::instructions::Home::default()),
            agent_token: uuid::Uuid::new_v4().to_string(),
        })
    }

    pub fn broker(&self) -> &Broker {
        &self.broker
    }

    /// Where we are, right now.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            // Read first: a snapshot that claims a sequence it does not yet
            // include would have the client discard events it still needs.
            seq: self.broker.next_seq().saturating_sub(1),
            mode: self.audio.lock().unwrap().mode(),
            voice: *self.voice.lock().unwrap(),
            loading: self.loading.lock().unwrap().progress(),
            profile: *self.profile.lock().unwrap(),
            tiles_served: self.tiles.serving(),
            sessions: self.sessions.lock().unwrap().all(),
            pending: self.permissions.lock().unwrap().pending(),
        }
    }

    // ----------------------------------------------------------------- audio

    pub fn audio_mode(&self) -> AudioMode {
        self.audio.lock().unwrap().mode()
    }

    /// Move the listening mode, or say why it did not move.
    ///
    /// The refusal is returned rather than swallowed. On this state machine a
    /// silent no-op means the microphone ends up somewhere nobody asked for,
    /// and the user is owed the reason — "no voice command leads to off" is a
    /// sentence the interface should be able to show.
    pub fn apply_audio(&self, trigger: ModeTrigger) -> Result<ModeChange, GateRefusal> {
        let change = self.audio.lock().unwrap().apply(trigger)?;
        self.broker.publish(Event::AudioMode {
            mode: change.to,
            trigger: change.trigger,
        });
        // Reopening the ear lands in hybrid, which an ear that hears cannot
        // use — see `set_ear`. So the same hand goes on to active listening.
        // Only for reopening: a user who asks for named-only gets it.
        if change.to == AudioMode::Hybrid
            && matches!(trigger, ModeTrigger::Interface | ModeTrigger::Shortcut)
            && self.ear() == audio::Ear::Audio
        {
            return self.apply_audio(ModeTrigger::ListenToAll);
        }
        Ok(change)
    }

    /// Note that a turn finished being processed, restarting the idle timer.
    ///
    /// The end of a *processed turn*, not the last noise heard — decision 19,
    /// and the failure it avoids is a television in the room keeping ARIA awake
    /// for ever.
    /// `named` says whether ARIA's own name was in it, which is what decides
    /// whether an overheard turn may extend a wake ARIA gave itself. See
    /// [`AudioGate::turn_processed`].
    pub fn turn_processed(&self, named: bool) {
        self.audio.lock().unwrap().turn_processed(named);
    }

    /// The main agent spoke an answer — not a silence. Decision 119: it
    /// restarts the wake window, and counts against a wake ARIA gave itself
    /// when the turn did not name it. See [`AudioGate::answer_spoken`].
    pub fn answer_spoken(&self) {
        self.audio.lock().unwrap().answer_spoken();
    }

    /// Publish what the user is saying, as the ASR hears it.
    ///
    /// For `aria-voice` to call through the shell. The core deliberately does
    /// NOT depend on that crate — it drags an ONNX runtime, and the boundary
    /// that pays here is "the core builds without a window **or** a model". So
    /// the chain pushes in; the core never reaches out.
    pub fn push_transcript(&self, text: impl Into<String>, final_: bool) {
        self.broker.publish(Event::Transcript {
            text: text.into(),
            final_,
        });
    }

    /// Publish what ARIA just said.
    ///
    /// Called by the pilot, which also hands the same text to the TTS. Both,
    /// not one or the other: the window needs to be able to show a reconnected
    /// history, and an audio pipeline cannot be re-read.
    pub fn spoke(&self, text: impl Into<String>) {
        self.broker.publish(Event::Spoken { text: text.into() });
    }

    /// Hand a finished turn to the main agent.
    ///
    /// The one door into the conversation from outside, and the voice chain
    /// and the keyboard both come through it — which is what keeps
    /// `app/src/voice` from knowing that an agent exists at all.
    ///
    /// Silently does nothing when no agent is running. That is the honest
    /// behaviour rather than an error: ARIA listens before it answers, and the
    /// transcript has already been published either way.
    pub fn heard_turn(&self, text: &str) {
        match self.conversation() {
            Some(conversation) => conversation.heard(text),
            None => tracing::debug!("un tour complet sans agent pour l'entendre : {text}"),
        }
    }

    /// Hand a finished turn to the main agent **as sound** — epic 10.
    ///
    /// The same door as [`Self::heard_turn`], for an ear that does not
    /// transcribe. Plain samples, 16 kHz mono: the core still knows nothing
    /// of `aria-voice`.
    pub fn heard_audio(&self, samples: Vec<f32>) {
        match self.conversation() {
            Some(conversation) => conversation.heard_audio(samples),
            None => tracing::debug!("un tour complet sans agent pour l'entendre"),
        }
    }

    /// Subscribe to the main agent's timestamped marks.
    pub fn marks(&self) -> tokio::sync::broadcast::Receiver<agent::marks::Marked> {
        self.marks.subscribe()
    }

    /// Record one. Free when nobody listens.
    pub fn mark(&self, mark: agent::marks::Mark) {
        let _ = self.marks.send(agent::marks::Marked {
            at: std::time::Instant::now(),
            mark,
        });
    }

    /// What the ear should hand over. Read by the voice thread on every pass.
    pub fn ear(&self) -> audio::Ear {
        *self.ear.lock().unwrap()
    }

    /// Change it. The voice thread notices and reloads what the new ear needs.
    ///
    /// An ear that hears also moves hybrid to **active listening**. Hybrid
    /// waits for the name, the name is looked for in a transcript, and there is
    /// none: left in hybrid, ARIA hears every turn and drops it. That was the
    /// first evening with Gemma on 2026-11-18 — the model loaded and warm, and
    /// not one turn reaching it. Hybrid comes back with the wake-word detector
    /// of epic 09. Off stays off: that is the user's hand on the ear.
    pub fn set_ear(&self, ear: audio::Ear) {
        let previous = std::mem::replace(&mut *self.ear.lock().unwrap(), ear);
        if previous != ear {
            tracing::info!(?ear, "l'oreille change");
        }
        if ear == audio::Ear::Audio && self.audio_mode() == AudioMode::Hybrid {
            match self.apply_audio(ModeTrigger::ListenToAll) {
                Ok(_) => tracing::info!(
                    "oreille audio : écoute active, puisque l'hybride ne peut pas entendre mon nom"
                ),
                Err(e) => tracing::warn!("écoute active refusée : {e}"),
            }
        }
    }

    /// How loud the microphone is, right now.
    ///
    /// Pushed by the voice chain on every buffer — fifty times a second — and
    /// read by whoever is drawing. See [`Self::level`] for why this is not an
    /// event.
    pub fn push_level(&self, level: f32) {
        let _ = self.level.send(level.clamp(0.0, 1.0));
    }

    /// Watch the microphone level. Misses frames rather than queueing them,
    /// which is the right failure for a meter.
    pub fn levels(&self) -> tokio::sync::watch::Receiver<f32> {
        self.level.subscribe()
    }

    pub fn voice_state(&self) -> VoiceState {
        *self.voice.lock().unwrap()
    }

    pub fn set_voice_state(&self, state: VoiceState) {
        *self.voice.lock().unwrap() = state;
        self.broker.publish(Event::VoiceState { state });
    }

    // --------------------------------------------------------------- loading

    /// A model file is coming down, or — with `None` — nothing is any more.
    ///
    /// Not kept in the snapshot: progress is published several times a second
    /// while it lasts, so a window opened mid-download catches up within one.
    pub fn report_download(&self, download: Option<audio::Download>) {
        self.broker.publish(Event::ModelDownload { download });
    }

    /// A model started being read into memory.
    ///
    /// Call it **before** the read, not after: the stretch this covers is the
    /// one where ARIA is on screen and cannot do its job, and a mark that
    /// appears once the wait is over marks nothing.
    ///
    /// Idempotent, and the two threads that call it never coordinate: the
    /// hearing side is in the capture thread, the voice in the speech thread,
    /// and both may start within a millisecond of each other at launch.
    /// [`VoiceState::Loading`] is published on the first one and holds until
    /// the last [`Self::finished_load`] — which is the whole point. It used to
    /// be set and cleared by whoever had an opinion last, so the bar finished
    /// when the *first* model did, the orb lit up, and ARIA was listening with
    /// nothing to answer with.
    pub fn begin_load(&self, what: Load) {
        let progress = self.loading.lock().unwrap().begin(what);
        if let Some(progress) = progress {
            tracing::info!(
                ?what,
                done = progress.done,
                total = progress.total,
                "chargement"
            );
            self.broker
                .publish(Event::VoiceLoading { loading: progress });
            self.set_voice_state(VoiceState::Loading);
        }
    }

    /// A model is in memory.
    ///
    /// Returns to [`VoiceState::Idle`] only when nothing is left to wait for,
    /// and only if the state is still `Loading` — something that moved the
    /// pipeline on in the meantime (a barge-in, a first turn) is not undone by
    /// a model that happened to finish after it.
    pub fn finished_load(&self, what: Load) {
        let progress = self.loading.lock().unwrap().finished(what);
        let Some(progress) = progress else {
            // Unbalanced rather than harmless: it means a caller reported a
            // load it never began, and the next thing to go wrong will be a
            // bar that never fills.
            tracing::warn!(?what, "fin de chargement sans début correspondant");
            return;
        };
        tracing::info!(
            ?what,
            done = progress.done,
            total = progress.total,
            "chargé"
        );
        let finished = progress.is_idle();
        self.broker
            .publish(Event::VoiceLoading { loading: progress });
        if finished && self.voice_state() == VoiceState::Loading {
            self.set_voice_state(VoiceState::Idle);
        }
    }

    /// How far the models are, for a window that just connected.
    pub fn loading(&self) -> Loading {
        self.loading.lock().unwrap().progress()
    }

    // -------------------------------------------------------------- settings

    pub fn default_profile(&self) -> Profile {
        *self.profile.lock().unwrap()
    }

    /// Set the default profile. A default, not a decree: a session may be
    /// launched under another one, which is why each session carries its own.
    pub fn set_default_profile(&self, profile: Profile) {
        *self.profile.lock().unwrap() = profile;
    }

    // -------------------------------------------------------------- sessions

    /// Open a session. Under the given profile, or the current default.
    pub fn open_session(&self, title: impl Into<String>, profile: Option<Profile>) -> SessionId {
        let profile = profile.unwrap_or_else(|| self.default_profile());
        let title = title.into();
        let id = self.sessions.lock().unwrap().open(title.clone(), profile);
        self.broker.publish(Event::SessionOpened {
            session: id,
            title,
            profile,
        });
        id
    }

    pub fn sessions(&self) -> Vec<Session> {
        self.sessions.lock().unwrap().all()
    }

    pub fn set_session_state(&self, id: SessionId, state: SessionState) {
        let moved = self.sessions.lock().unwrap().set_state(id, state);
        // Only announce a session that exists. Publishing for an unknown id
        // would put a phantom in the right-hand menu.
        if moved.is_some() {
            self.broker
                .publish(Event::SessionState { session: id, state });
        }
    }

    /// Record the engine's own identifier for a session.
    ///
    /// What a resume needs later, and it has to outlive the process: the live
    /// handle is dropped when the session ends, which is precisely when this
    /// becomes useful.
    pub fn attach_native_id(&self, id: SessionId, native_id: impl Into<String>) {
        let native_id = native_id.into();
        if self
            .sessions
            .lock()
            .unwrap()
            .set_native_id(id, native_id.clone())
        {
            // And into `aria.db`, for a project session. An update, so a
            // throwaway session — which has no row — leaves no trace here
            // either, with nothing having to remember which is which.
            if let Some(conversation) = self.current_conversation() {
                let at = agent::clock::now();
                self.with_history("l'identifiant natif d'une session", |history| {
                    history.attach_native(conversation, id, &native_id, at)
                });
            }
            self.broker.publish(Event::SessionEngaged {
                session: id,
                native_id,
            });
        }
    }

    /// The engine's own identifier, for a resume.
    pub fn session_native_id(&self, id: SessionId) -> Option<String> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .and_then(|s| s.native_id.clone())
    }

    pub fn push_step(&self, id: SessionId, step: ActivityStep) {
        self.sessions.lock().unwrap().touch(id);
        self.broker
            .publish(Event::ActivityStep { session: id, step });
    }

    pub fn push_message_delta(&self, id: SessionId, text: impl Into<String>) {
        self.sessions.lock().unwrap().touch(id);
        self.broker.publish(Event::MessageDelta {
            session: id,
            text: text.into(),
        });
    }

    pub fn finish_session(&self, id: SessionId, summary: impl Into<String>) {
        self.set_session_state(id, SessionState::Done);
        self.broker.publish(Event::TaskDone {
            session: id,
            summary: summary.into(),
        });
    }

    pub fn fail_session(&self, id: SessionId, message: impl Into<String>) {
        self.set_session_state(id, SessionState::Failed);
        self.broker.publish(Event::TaskError {
            session: id,
            message: message.into(),
        });
    }

    /// Mark every session that has gone quiet for three minutes.
    ///
    /// Marks. Never kills — decision 8, and the measured reasons behind it:
    /// `--max-turns` and `--max-budget-usd` are already hard ceilings, and a
    /// long compile legitimately produces nothing. Returns what it marked, so
    /// the caller can log it.
    pub fn sweep_quiet_sessions(&self) -> Vec<SessionId> {
        let quiet = self.sessions.lock().unwrap().gone_quiet();
        for id in &quiet {
            // Through the public method, so the event goes out with it.
            self.set_session_state(*id, SessionState::Unresponsive);
        }
        quiet
    }

    // ----------------------------------------------------------- permissions

    /// File a permission request and announce it.
    ///
    /// It will sit there until answered. There is no deadline to set.
    pub fn ask_permission(&self, request: PermissionRequest) -> RequestId {
        let session = request.session;
        let id = self.permissions.lock().unwrap().file(request.clone());
        // The session is blocked, not silent: say so before the request, so a
        // client folding events in order never sees the request against a
        // session still claiming to execute.
        self.set_session_state(session, SessionState::AwaitingPermission);
        self.broker.publish(Event::PermissionRequest { request });
        id
    }

    /// The supervisor the sessions run under.
    pub fn supervisor(&self) -> &Arc<Supervisor> {
        &self.supervisor
    }

    /// The relay's endpoint routing table.
    pub fn session_routes(&self) -> &Arc<crate::mcp::SessionRoutes> {
        &self.routes
    }

    /// The tiles this run of the application is holding.
    ///
    /// Shared rather than borrowed: the sandbox origin serves from it on a task
    /// of its own, and the window reads it over IPC.
    pub fn tiles(&self) -> &Arc<crate::tiles::Registry> {
        &self.tiles
    }

    /// Register a tile and tell the window it exists.
    ///
    /// Decision 23: the mutation and its event are one call, so an interface
    /// that reconnects cannot disagree with what the core holds. Returns the
    /// `ui://` URI, which is what MCP Apps puts in `_meta.ui.resourceUri`.
    pub fn show_tile(
        &self,
        session: SessionId,
        title: impl Into<String>,
        body: impl Into<String>,
        files: Vec<crate::tiles::TileFile>,
        form: Option<crate::tiles::Form>,
    ) -> String {
        let tile = self.tiles.add(session, title, body, files, form);
        self.broker.publish(Event::TileReady {
            session,
            uri: tile.uri.clone(),
            title: tile.title.clone(),
            source: tile.source(),
            form: tile.form.clone(),
            documents: tile.documents(),
        });
        tile.uri
    }

    // ------------------------------------------------- the main agent's tools

    /// Attach the conversation the main agent's MCP endpoint delegates to.
    ///
    /// Held weakly, so attaching costs nothing and dropping the conversation
    /// takes the capability with it: a `delegate_task` arriving after the agent
    /// is gone is refused rather than launched into the void.
    pub fn attach_conversation(&self, conversation: &Arc<agent::conversation::Conversation>) {
        *self.conversation.lock().unwrap() = Some(Arc::downgrade(conversation));
    }

    pub fn conversation(&self) -> Option<Arc<agent::conversation::Conversation>> {
        self.conversation
            .lock()
            .unwrap()
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
    }

    /// The user cut ARIA off: stop talking, and stop the answer being written.
    ///
    /// One call for both doors — the voice heard someone speak over it, or
    /// STOP was pressed — so the two cannot drift. It publishes
    /// [`VoiceState::Interrupted`], which the speech side reads to silence the
    /// speakers and drop the sentences still queued, and it tells the pilot,
    /// which mutes the rest of the turn and stops the CLI when that is safe.
    ///
    /// **Delegated sessions are not touched.** A barge-in is "stop talking",
    /// not "stop working": the weather asked for a minute ago keeps coming.
    /// STOP kills them separately, through [`Self::stop_all_sessions`].
    pub fn interrupt_agent(&self) {
        self.set_voice_state(VoiceState::Interrupted);
        if let Some(conversation) = self.conversation() {
            conversation.interrupt();
        }
    }

    /// The token guarding the main agent's MCP endpoint.
    pub fn agent_token(&self) -> &str {
        &self.agent_token
    }

    /// How the engine holding delegated sessions spells an MCP tool.
    ///
    /// Asked of the provider rather than held, because the provider can be
    /// swapped in the settings screen and a conversation that outlives the swap
    /// has to follow it. The default is only reached before one is attached —
    /// which is every unit test here, and costs nothing: it is also the only
    /// convention anyone has verified.
    pub fn delegation_naming(&self) -> providers::McpToolNaming {
        self.provider()
            .map(|provider| provider.capabilities().naming)
            .unwrap_or_default()
    }

    /// The pattern registry, for inspection.
    pub fn registry(&self) -> PatternRegistry {
        self.registry.lock().unwrap().clone()
    }

    /// Decide whether a session may run something. **Blocks until decided.**
    ///
    /// This is what the relay's tool call awaits, and the order of the two
    /// branches is the whole of decision 6:
    ///
    /// 1. the **registry** answers alone for a recognised side-effect-free
    ///    command — and the action is still written to the audit, because what
    ///    is removed is the confirmation, not the trace;
    /// 2. anything else reaches the user, and waits. There is no timeout: a
    ///    pending request does not expire, so neither does this await. The
    ///    session is blocked, the interface says so, and nothing is lost.
    pub async fn decide_permission(
        &self,
        session: SessionId,
        tool: &str,
        command: Option<String>,
    ) -> Decision {
        // ARIA's own MCP Apps tools are answered without asking — see
        // `tiles::is_own_app_tool` for why, and for how narrowly that is meant.
        // Before the registry rather than inside it, because the registry
        // matches command lines and this tool has none: it would be judged
        // `unknown` and every tile would cost a confirmation.
        // The spelling belongs to whichever engine holds this session — see
        // `providers::McpToolNaming`, and the leak it seals: until 2026-11-12
        // `mcp__server__tool` was written into both matchers below, so a second
        // engine would have failed to recognise ARIA's own tools and turned
        // every tile and every `recall` into a permission card. Silently.
        let naming = self.delegation_naming();
        let verdict = if crate::tiles::is_own_app_tool(naming, tool) {
            permissions::registry::Verdict::by_tool(permissions::Risk::Low, "outil MCP Apps d'ARIA")
        } else if crate::mcp::subagent::is_read_tool(naming, tool) {
            // Decision 106. Reading ARIA's memory is answered without asking,
            // and for the same two reasons the tile tool is: the registry
            // matches **command lines** and this tool has none, so every
            // `recall` inside a delegated task would be a card on screen; and
            // there is nothing for a confirmation to protect — the pages are
            // the user's own notes, in their own folder, and the session was
            // opened by the user's own sentence.
            //
            // Bounded to the **read** half on purpose: the write tools are not
            // on this post at all (see `mcp::subagent`), so this is not a
            // door left open for them.
            permissions::registry::Verdict::by_tool(
                permissions::Risk::Low,
                "lecture de la mémoire d'ARIA",
            )
        } else {
            match &command {
                Some(command) => self.registry.lock().unwrap().verdict(command),
                // A tool with no command to match on cannot be judged.
                None => permissions::registry::Verdict::unknown(),
            }
        };

        let request = PermissionRequest::new(
            session,
            tool,
            command.clone(),
            crate::mcp::describe(tool, command.as_deref(), verdict.risk),
            verdict.risk,
        );

        if verdict.auto_approve {
            // Audited exactly like any other, so the activity feed and the
            // journal show what ran — one event to say it was asked, one to say
            // the registry answered.
            let id = request.id;
            tracing::debug!(
                "registry allowed {tool}: {}",
                verdict.matched.as_deref().unwrap_or("?")
            );
            self.record_decision(
                Some(session),
                tool,
                command,
                verdict.risk,
                Decision::Allow,
                Channel::Registry,
            );
            self.broker.publish(Event::PermissionRequest { request });
            self.broker.publish(Event::PermissionResolved {
                id,
                decision: Decision::Allow,
                by: Channel::Registry,
            });
            return Decision::Allow;
        }

        let (_, wait) = {
            let mut desk = self.permissions.lock().unwrap();
            desk.file_awaiting(request.clone())
        };
        // Said before the request, so a client folding events in order never
        // sees a request against a session still claiming to execute.
        self.set_session_state(session, SessionState::AwaitingPermission);
        self.broker.publish(Event::PermissionRequest { request });

        match wait.await {
            Ok(decision) => decision,
            // The desk was dropped, or the session was abandoned while its
            // request was still open. Refusing is the only honest answer.
            Err(_) => Decision::Deny,
        }
    }

    /// Refuse every request still open against a session, and wake its callers.
    ///
    /// For a session that ended with requests on screen: the thing that would
    /// have acted on an approval is gone, so leaving them would be an invitation
    /// to approve nothing.
    pub fn abandon_permissions(&self, session: SessionId) {
        let ids = self.permissions.lock().unwrap().abandon_session(session);
        if !ids.is_empty() {
            // One line, not one per request: what happened is that a session
            // ended with questions open, and the journal should read that way.
            self.record_decision(
                Some(session),
                "session abandonnée",
                Some(format!("{} demande(s) encore ouverte(s)", ids.len())),
                permissions::Risk::Low,
                Decision::Deny,
                Channel::Registry,
            );
        }
        for id in ids {
            self.broker.publish(Event::PermissionResolved {
                id,
                decision: Decision::Deny,
                by: Channel::Registry,
            });
        }
    }

    pub fn pending_permissions(&self) -> Vec<PermissionRequest> {
        self.permissions.lock().unwrap().pending()
    }

    /// Answer a request. The first channel to answer wins.
    pub fn answer_permission(
        &self,
        id: RequestId,
        decision: Decision,
        by: Channel,
    ) -> Result<PermissionRequest, AnswerRefusal> {
        let (request, decision) = self.permissions.lock().unwrap().answer(id, decision, by)?;
        // Written down before the stream is told, so a decision cannot be seen
        // on screen and be missing from the journal.
        self.record_decision(
            Some(request.session),
            &request.tool,
            request.command.clone(),
            request.risk,
            decision.clone(),
            by,
        );
        self.broker
            .publish(Event::PermissionResolved { id, decision, by });
        Ok(request)
    }

    // ------------------------------------------------------------ the engine

    /// Attach the engine sessions are delegated to.
    pub fn attach_provider(&self, provider: Arc<dyn AgentProvider>) {
        tracing::info!("provider attached: {}", provider.display_name());
        *self.provider.lock().unwrap() = Some(provider);
    }

    pub fn provider(&self) -> Option<Arc<dyn AgentProvider>> {
        self.provider.lock().unwrap().clone()
    }

    /// Hand the core its memory folder.
    ///
    /// Called from `app/`, which is the only place that knows where
    /// `~/.aria/memories` is (decision 94).
    pub fn attach_memory(&self, memory: store::Memory) {
        tracing::info!(pages = memory.len(), "mémoire attachée");
        *self.memory.lock().unwrap() = Some(memory);
    }

    /// Whether a memory folder is attached at all.
    pub fn memory_attached(&self) -> bool {
        self.memory.lock().unwrap().is_some()
    }

    /// Run something against the memory, under the lock.
    ///
    /// Everything the memory does is synchronous, so the closure form is what
    /// keeps the lock out of an await rather than a rule to remember.
    pub fn with_memory<T>(
        &self,
        f: impl FnOnce(&mut store::Memory) -> Result<T, store::MemoryError>,
    ) -> Result<T, store::MemoryError> {
        let mut guard = self.memory.lock().unwrap();
        match guard.as_mut() {
            Some(memory) => f(memory),
            None => Err(store::MemoryError::Detached),
        }
    }

    /// The fast path, for the voice agent: two sentences and the page's id.
    pub fn memory_recall(&self, query: &str) -> Result<Option<store::Recalled>, store::MemoryError> {
        self.with_memory(|memory| memory.recall(query))
    }

    /// The deep path, summarised: the fused ranking, resumes only.
    ///
    /// Served to the voice post *and* to a delegated session (decision 105).
    /// Whole pages are a separate call — [`Core::memory_read`] — so the cost
    /// decision 89 objected to is paid per page the model actually wants.
    pub fn memory_deep(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<store::Recalled>, store::MemoryError> {
        self.with_memory(|memory| memory.deep_recall(query, limit))
    }

    /// One page in full, by id or alias. For a reader, never for the voice.
    pub fn memory_read(&self, page: &str) -> Result<String, store::MemoryError> {
        self.with_memory(|memory| memory.read(page))
    }

    // --------------------------------------------------- the conversations

    /// Hand the core `aria.db`, and sweep what has gone thirty days unused.
    ///
    /// Called from `app/`, the only place that knows where the file is —
    /// `~/.aria/db/aria.db`, with the memory's pages and the two `AGENTS.md`
    /// rather than under `%LOCALAPPDATA%/aria`. Decision 111, and the argument
    /// is ownership: a thread of conversation and the audit of what ARIA was
    /// allowed to do belong to the user, where `%LOCALAPPDATA%/aria` is what
    /// gets emptied to reclaim 3.7 GB. The cost — a home directory a
    /// synchroniser may be watching — is written out in `app/src/history.rs`,
    /// along with the three things that make it bearable.
    ///
    /// The sweep runs here rather than on a timer because this is also when the
    /// CLI runs its own `cleanupPeriodDays` pass, and the two staying in step
    /// costs nothing.
    pub fn attach_history(&self, history: store::history::History) {
        match history.sweep(agent::clock::now()) {
            Ok(swept) if !swept.is_empty() => tracing::info!(
                conversations = swept.conversations,
                sessions = swept.sessions,
                "rétention appliquée au démarrage"
            ),
            Ok(_) => {}
            // A sweep that fails is not a reason to start without a history:
            // the worst case is a file that keeps rows it could have dropped.
            Err(e) => tracing::warn!("la rétention n'a pas pu s'appliquer : {e}"),
        }
        *self.history.lock().unwrap() = Some(history);
    }

    /// Whether anything is being saved at all.
    pub fn history_attached(&self) -> bool {
        self.history.lock().unwrap().is_some()
    }

    /// Run something against the store, or `None` when there is none.
    ///
    /// `None` and not an error: every caller here is doing bookkeeping beside
    /// its real work, and a run with nowhere to write still holds a
    /// conversation. What is lost is the saving, never the talking.
    pub fn with_history<T>(
        &self,
        what: &'static str,
        f: impl FnOnce(&store::history::History) -> rusqlite::Result<T>,
    ) -> Option<T> {
        let guard = self.history.lock().unwrap();
        let history = guard.as_ref()?;
        match f(history) {
            Ok(value) => Some(value),
            Err(e) => {
                tracing::warn!("{what} : {e}");
                None
            }
        }
    }

    /// Every conversation, most recently used first — the left-hand menu.
    pub fn conversations(&self) -> Vec<store::history::Conversation> {
        self.with_history("la liste des conversations", |history| {
            history.conversations()
        })
        .unwrap_or_default()
    }

    /// Which thread the live conversation is writing into.
    pub fn current_conversation(&self) -> Option<store::history::ConversationId> {
        *self.thread.lock().unwrap()
    }

    /// Open a fresh thread and make it the current one.
    ///
    /// What starting the application does. The user's answer on 2026-11-13: a
    /// new exchange by default, the old ones waiting in the menu.
    pub fn start_conversation(&self) -> Option<store::history::ConversationId> {
        let id = self.with_history("l'ouverture d'une conversation", |history| {
            // The thread being left, and any other that was never spoken in,
            // goes before the new one opens — there is nothing in it to resume.
            history.sweep_empty(None)?;
            history.start(agent::clock::now())
        })?;
        *self.thread.lock().unwrap() = Some(id);
        tracing::info!(%id, "nouvelle conversation");
        self.broker.publish(Event::ConversationOpened { id });
        Some(id)
    }

    /// Make an existing thread the current one, and hand back what it holds.
    ///
    /// The entries are returned rather than applied here: what to do with them
    /// belongs to [`agent::conversation::Conversation`], which owns the journal
    /// and the wire to the agent. The core's job is to say which thread is
    /// live.
    pub fn open_conversation(
        &self,
        id: store::history::ConversationId,
    ) -> Option<Vec<agent::journal::Entry>> {
        let thread = self.with_history("la reprise d'une conversation", |history| {
            history.sweep_empty(Some(id))?;
            history.touch(id, agent::clock::now())?;
            history.thread(id)
        })?;
        *self.thread.lock().unwrap() = Some(id);
        tracing::info!(%id, turns = thread.len(), "conversation reprise");
        self.broker.publish(Event::ConversationOpened { id });
        Some(thread)
    }

    /// Drop a thread and everything hanging off it. The audit is untouched.
    pub fn forget_conversation(&self, id: store::history::ConversationId) {
        self.with_history("l'oubli d'une conversation", |history| history.forget(id));
        if self.current_conversation() == Some(id) {
            *self.thread.lock().unwrap() = None;
        }
    }

    /// Note that a session belongs to the live thread, so a later turn can
    /// resume it.
    ///
    /// Epic 05: only a **project** session gets here. A throwaway one — the
    /// weather, the news — is never written down, which is why there is nothing
    /// to clean up behind it.
    pub fn remember_project_session(&self, session: SessionId, project: &str) {
        let Some(conversation) = self.current_conversation() else {
            return;
        };
        let entry = {
            let sessions = self.sessions.lock().unwrap();
            let Some(found) = sessions.get(session) else {
                return;
            };
            store::history::ProjectSession {
                id: session,
                // Usually `None` here: the row is written the moment the
                // session is launched, and the engine names it a beat later.
                // `attach_native_id` fills it in.
                native: found.native_id.clone(),
                project: project.to_string(),
                title: found.title.clone(),
                last_used_at: agent::clock::now(),
                expired: false,
            }
        };
        self.with_history("l'enregistrement d'une session de projet", |history| {
            history.remember_session(conversation, &entry)
        });
    }

    /// The project sessions of one thread, most recently used first.
    pub fn project_sessions(
        &self,
        conversation: store::history::ConversationId,
    ) -> Vec<store::history::ProjectSession> {
        self.with_history("la liste des sessions d'une conversation", |history| {
            history.sessions(conversation)
        })
        .unwrap_or_default()
    }

    /// Mark a session no longer resumable — **after** a resume failed.
    ///
    /// Epic 05 forbids deducing this from a date: `cleanupPeriodDays` lives in
    /// the user's own settings and can be one day. The row stays, marked, so
    /// the user is told why the context is gone instead of watching the agent
    /// start over without explanation.
    pub fn mark_session_expired(&self, session: SessionId) {
        let Some(conversation) = self.current_conversation() else {
            return;
        };
        self.with_history("le marquage d'une session expirée", |history| {
            history.mark_expired(conversation, session)
        });
    }

    /// Write one permission decision to the journal nothing deletes.
    ///
    /// Decision 6 removes the confirmation for a recognised pattern, not the
    /// trace: an auto-approval arrives here like any other, with
    /// [`Channel::Registry`] as what tells it apart.
    pub fn record_decision(
        &self,
        session: Option<SessionId>,
        tool: &str,
        command: Option<String>,
        risk: permissions::Risk,
        decision: Decision,
        by: Channel,
    ) {
        let entry = store::history::audit::Decided {
            at: agent::clock::now(),
            session,
            tool: tool.to_string(),
            command,
            risk,
            decision,
            channel: by,
        };
        self.with_history("l'écriture d'une décision à l'audit", |history| {
            history.record(&entry)
        });
    }

    /// The audit, most recent first.
    pub fn audit(&self, limit: usize) -> Vec<store::history::audit::Decided> {
        self.with_history("la lecture de l'audit", |history| history.audit(limit))
            .unwrap_or_default()
    }

    /// `~/.aria` as `app/` resolved it: the files and the directories.
    pub fn home(&self) -> agent::instructions::Home {
        self.home.lock().unwrap().clone()
    }

    /// Called once at start-up, from `app/`, which is the only part that knows
    /// where `~` is.
    pub fn set_home(&self, found: agent::instructions::Home) {
        *self.home.lock().unwrap() = found;
    }

    /// Catch up on vectors — called **at silence only** (decision 92).
    ///
    /// Returns how many pages were embedded, and `0` when there is nothing to
    /// do or no vectoriser attached, so a caller can poll it cheaply.
    pub fn memory_vectorise(&self, budget: usize) -> Result<usize, store::MemoryError> {
        self.with_memory(|memory| memory.vectorise_pending(budget))
    }

    /// What the settings screen shows about the active engine.
    ///
    /// The mapping is **asked of the provider**, never written in the
    /// interface. That is the whole of decision 5: a profile the provider
    /// cannot honour comes back with `native: None`, and the screen says so
    /// instead of offering a setting with no effect.
    pub async fn provider_info(&self) -> Option<ProviderInfo> {
        let provider = self.provider()?;
        Some(ProviderInfo {
            id: provider.id().to_string(),
            display_name: provider.display_name().to_string(),
            kind: provider.kind(),
            availability: provider.availability().await,
            mapping: provider.profile_mapping(),
            shortcomings: provider
                .capabilities()
                .shortcomings()
                .into_iter()
                .map(str::to_string)
                .collect(),
        })
    }

    /// Delegate a task. Returns the session it opened.
    pub async fn launch_session(
        self: &Arc<Self>,
        title: impl Into<String>,
        spec: LaunchSpec,
    ) -> anyhow::Result<SessionId> {
        let provider = self
            .provider()
            .ok_or_else(|| anyhow::anyhow!("aucun fournisseur attaché"))?;
        self.supervisor
            .launch(self, provider.as_ref(), title, spec)
            .await
    }

    /// Say something more to a session that is already running.
    pub async fn say_to_session(&self, id: SessionId, text: &str) -> anyhow::Result<()> {
        self.supervisor.send(id, text).await
    }

    /// Tell the window a tile it already shows has changed.
    ///
    /// Decision 23 again: the mutation and its event are one call. Returns
    /// false when the tile is gone — a session that reworked a document the
    /// user has since closed, which is a normal outcome and not an error.
    pub fn refresh_tile(&self, uri: &str, body: Option<String>) -> bool {
        if !self.tiles.refresh(uri, body) {
            return false;
        }
        self.broker.publish(Event::TileUpdated {
            uri: uri.to_string(),
        });
        true
    }

    /// Hand a tile's filled-in form back to the session that asked for it.
    ///
    /// # Why it goes to that session and not to the main agent
    ///
    /// Decided with the user on 2026-11-01. A form is the continuation of a
    /// piece of work, not a new request: the session that drew the tile is the
    /// one holding the context the answer belongs to. Routing it to the voice
    /// post would mean re-explaining, in a sentence, what a whole session
    /// already knows.
    ///
    /// # Why it may relaunch, and what that costs
    ///
    /// Decision 11: a tile outlives its session. So a form submitted an hour
    /// later has nobody listening, and the only honest answer is to **try the
    /// resume** — epic 05 forbids deducing resumability from a date, because
    /// the user's own `cleanupPeriodDays` can be one day and we do not control
    /// it. Try it, and handle the failure.
    ///
    /// A resumed session is a **new** [`SessionId`]: the process is new, the
    /// activity feed is new, and the native id is what carries the
    /// conversation across. The tile is re-pointed at it, so a second
    /// submission goes to the session that answered the first rather than
    /// starting a third.
    ///
    /// Returns the session the text actually reached.
    pub async fn deliver_to_session(
        self: &Arc<Self>,
        id: SessionId,
        text: &str,
    ) -> anyhow::Result<SessionId> {
        if self.supervisor.is_live(id) {
            self.supervisor.send(id, text).await?;
            return Ok(id);
        }

        let Some(native) = self.session_native_id(id) else {
            anyhow::bail!(
                "cette session est terminée et n'a pas d'identifiant à reprendre : \
                 sa réponse ne peut pas lui être rendue"
            );
        };
        let Some(provider) = self.provider() else {
            anyhow::bail!("aucun moteur n'est attaché : la session ne peut pas être reprise");
        };

        let title = self
            .sessions
            .lock()
            .unwrap()
            .get(id)
            .map(|s| s.title.clone())
            .unwrap_or_else(|| "Reprise".to_string());

        tracing::info!("reprise de la session {id} pour lui rendre un formulaire");
        let resumed = self
            .supervisor
            .launch(
                self,
                provider.as_ref(),
                title,
                providers::LaunchSpec {
                    prompt: text.to_string(),
                    profile: self.default_profile(),
                    resume: Some(native),
                    ..Default::default()
                },
            )
            .await;
        match resumed {
            Ok(resumed) => {
                // The tiles follow the conversation, so the resumed session can
                // refresh the panel it drew rather than draw another.
                self.tiles.follow(id, resumed);
                Ok(resumed)
            }
            Err(e) => {
                // Epic 05's rule, and the only place it can be applied: never
                // deduce resumability from a date — try it and handle the
                // failure. `cleanupPeriodDays` lives in the user's own
                // settings, defaults to 30 and can be 1, so a session that
                // looks fresh can already be gone.
                //
                // The entry **stays** in the thread, marked. What is lost is
                // the ability to continue, not the memory of what was done —
                // without the trace the user simply asks again and watches the
                // agent start over with nobody able to say why.
                tracing::warn!("la session {id} n'est plus reprenable : {e:#}");
                self.mark_session_expired(id);
                Err(e)
            }
        }
    }

    /// Stop one session — the **Arrêter** button decision 8 puts on a quiet one.
    pub async fn stop_session(self: &Arc<Self>, id: SessionId) -> anyhow::Result<()> {
        self.supervisor.stop(self, id).await
    }

    /// Stop every running session. `STOP` cuts the work as well as the ear.
    pub async fn stop_all_sessions(self: &Arc<Self>) {
        self.supervisor.stop_all(self).await;
    }

    pub fn live_session_count(&self) -> usize {
        self.supervisor.live_count()
    }

    // ------------------------------------------------------------- the clock

    /// Run the two timers that nobody else will run.
    ///
    /// Both exist because they are the only thing standing between a decision
    /// and a declaration: the three-minute silence mark (decision 8) and the
    /// two-minute idle fall-back out of active listening (decision 19). Ticks
    /// every five seconds, which is fine granularity for timers measured in
    /// minutes and costs nothing while the machine is idle.
    pub fn spawn_watchdog(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let core = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                tick.tick().await;
                for id in core.sweep_quiet_sessions() {
                    tracing::info!("session {id} has not reported in three minutes");
                }
                // Two ways out of a wake ARIA gave itself, and they catch
                // different rooms: the clock catches one that falls quiet, the
                // count catches one that never does. Both land in the resting
                // mode, never in off.
                let (idle, talked_past) = {
                    let audio = core.audio.lock().unwrap();
                    (audio.idle_expired(), audio.talked_past_its_welcome())
                };
                if talked_past {
                    tracing::info!(
                        "trois tours d'affilée sans qu'on me nomme — ce n'est pas à moi qu'on \
                         parle, je me remets en veille"
                    );
                }
                // Not with an ear that hears (epic 10): the resting mode is
                // hybrid, and hybrid cannot hear the name without a transcript.
                // Falling back there would make ARIA deaf, silently, two
                // minutes after the last exchange — measured on 2026-11-18,
                // the first evening with Gemma.
                if (idle || talked_past) && core.ear() == audio::Ear::Transcript {
                    let _ = core.apply_audio(ModeTrigger::IdleTimeout);
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use permissions::Risk;

    #[test]
    fn every_mutation_leaves_a_trace_in_the_stream() {
        let core = Core::new();
        let before = core.broker().next_seq();
        let id = core.open_session("essai", Some(Profile::Strict));
        core.set_session_state(id, SessionState::Executing);
        core.finish_session(id, "fait");
        // session.opened, session.state, session.state (done), task.done.
        assert_eq!(core.broker().next_seq(), before + 4);
    }

    #[test]
    fn a_snapshot_never_claims_a_sequence_it_does_not_include() {
        let core = Core::new();
        core.open_session("un", None);
        let snapshot = core.snapshot();
        assert_eq!(snapshot.seq, 1);
        assert_eq!(snapshot.sessions.len(), 1);
    }

    #[test]
    fn asking_permission_marks_the_session_blocked_before_the_request() {
        let core = Core::new();
        let session = core.open_session("compiler", None);
        core.ask_permission(PermissionRequest::new(
            session,
            "Bash",
            Some("cargo build".into()),
            "compiler le projet",
            Risk::Low,
        ));
        assert_eq!(
            core.sessions()[0].state,
            SessionState::AwaitingPermission,
            "a client folding events in order must never see the request \
             against a session still claiming to execute"
        );
    }

    #[test]
    fn an_unknown_session_does_not_become_a_phantom_in_the_menu() {
        let core = Core::new();
        let before = core.broker().next_seq();
        core.set_session_state(SessionId::new(), SessionState::Executing);
        assert_eq!(core.broker().next_seq(), before, "nothing was announced");
        assert!(core.sessions().is_empty());
    }

    #[test]
    fn a_refused_mode_change_publishes_nothing() {
        let core = Core::new();
        let before = core.broker().next_seq();
        // Hybrid is as quiet as a voice command goes.
        assert!(core.apply_audio(ModeTrigger::SpokenSleep).is_err());
        assert_eq!(core.broker().next_seq(), before);
        assert_eq!(core.audio_mode(), AudioMode::Hybrid);
    }
}
