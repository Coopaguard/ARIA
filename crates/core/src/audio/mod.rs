//! The listening policy: three modes, and the door that only opens one way.
//!
//! This module holds no DSP. Capture, the echo gate, the ASR and the end-of-turn
//! detector live in `aria-voice`, which answers "what was heard"; this one
//! decides what may be done about it. That split is why `aria-core` still
//! builds without an ONNX runtime.
//!
//! # The three modes, and what actually differs between them
//!
//! Decision 19. The surprise in it is worth keeping in sight: **hybrid and
//! active cost exactly the same memory**. The difference is a policy, not a
//! model. So the three modes are really two axes — loaded or not, named-only or
//! not — and only [`AudioMode::Off`] saves anything.
//!
//! | Mode | Listens to | Models | Leaves by |
//! |---|---|---|---|
//! | [`Off`](AudioMode::Off) | nothing | unloaded, 0 GB | interface or shortcut **only** |
//! | [`Hybrid`](AudioMode::Hybrid) | named only | loaded, ≈ 2.4 GB | hearing the name |
//! | [`Active`](AudioMode::Active) | everything | loaded, ≈ 2.4 GB | a complete turn asking for sleep, or two silent minutes |
//!
//! # Why no voice command reaches `Off`
//!
//! From `Off` nothing is listening, so nothing can hear "wake up". It is a
//! one-way door, and a one-way door is not crossed by voice. [`AudioGate::apply`]
//! refuses a voice-originated trigger that would leave `Off`, and the refusal is
//! a value — [`GateRefusal`] — not a silent no-op, because the caller has to be
//! able to say why nothing happened.
//!
//! Waking from `Off` costs about 3.8 s of model loading, measured in T1. The
//! interface has to show that; [`AudioGate`] reports the transition so it can.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// How much of what it hears ARIA is allowed to act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioMode {
    /// Nothing is listened to, the models are unloaded, the orb is grey.
    Off,
    /// Listening, but nothing leaves the machine until the name is recognised.
    ///
    /// This is the resting mode, and that is the point of decision 19: the
    /// resting mode is *hybrid*, not off. It is what makes named-only a privacy
    /// guarantee rather than mere silence.
    Hybrid,
    /// Everything is listened to and acted on.
    Active,
}

impl AudioMode {
    /// Whether the audio models are resident in this mode.
    ///
    /// True for both loaded modes, and that is the measured fact behind
    /// decision 19: the memory is the same, the policy is not.
    pub fn models_loaded(self) -> bool {
        !matches!(self, AudioMode::Off)
    }
}

/// What moved the mode. Carried on `audio.mode` so the interface can say *why*
/// ARIA stopped listening — "you asked" and "two minutes went by" are not the
/// same message to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeTrigger {
    /// A click, a switch, a settings change.
    Interface,
    /// The configurable global shortcut.
    Shortcut,
    /// The name was recognised in the transcript.
    NameHeard,
    /// A complete turn asked for sleep — "merci Aria", "mets-toi en veille".
    ///
    /// Complete *at the end-of-turn detector's sense of the word*: "merci Aria,
    /// et peux-tu aussi…" must put nothing to sleep. That is where T2 pays for
    /// itself.
    SpokenSleep,
    /// Two minutes without an exchange.
    IdleTimeout,
    /// The user asked to be listened to without having to say the name.
    ///
    /// A **policy**, which is a different gesture from [`Self::Interface`]:
    /// that one opens and closes the ear, this one says what to do with what
    /// the ear hears. Two controls, two scopes — the same distinction the orb
    /// and STOP already carry, and confusing them loses someone either their
    /// privacy or their task.
    ListenToAll,
    /// The user asked to be heard only when ARIA is named. The resting policy.
    NamedOnly,
}

impl ModeTrigger {
    /// Whether this trigger came from something heard.
    ///
    /// The transition table in [`AudioGate::apply`] spells the spoken cases out
    /// one by one rather than calling this, so the compiler checks the table.
    /// This is for callers upstream — the turn engine deciding whether it is
    /// even allowed to offer a trigger.
    pub fn is_spoken(self) -> bool {
        matches!(self, ModeTrigger::NameHeard | ModeTrigger::SpokenSleep)
    }
}

/// The audio pipeline right now, as the original specification names it.
///
/// A different quantity from [`AudioMode`], and both are needed: the mode says
/// what ARIA is allowed to hear, this says what the chain is doing this instant.
/// The orb reads this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceState {
    /// Nothing in flight.
    Idle,
    /// The audio models are being read off disk.
    ///
    /// About **3.8 s**, measured in T1, and the one stretch where ARIA is on
    /// screen and cannot hear a word. Without a state of its own the orb looks
    /// exactly as it does when idle, so the first thing a new user does is
    /// speak into a chain that is not there yet — and conclude it does not
    /// work.
    Loading,
    /// Capturing speech.
    Listening,
    /// The transcript is with the turn engine.
    Thinking,
    /// The TTS is playing.
    Speaking,
    /// The user spoke over the TTS and playback was cut.
    ///
    /// Decision 17: the answer to an interruption is to go quiet. Playback stops
    /// within the frame, and the echo is gone 20 ms later with a clean
    /// microphone for what follows.
    Interrupted,
}

/// A model being read into memory.
///
/// Two of them, and they are what [`VoiceState::Loading`] now covers **both**
/// of. Until 2026-10-28 that state meant one thing — the hearing side — and
/// the voice loaded silently beside it, which was true for as long as the only
/// voice was the one Windows already has: it needs nothing read and is ready
/// before it is asked. A chosen engine is not like that. Kyutai's holds a
/// hundred megabytes in memory, downloads them the first time, and takes
/// longer to be ready than the ASR does.
///
/// So the loading bar stopped telling the truth in the only case where it
/// mattered: it completed, the orb lit up, and ARIA could hear perfectly well
/// and had nothing to answer with. A bar that finishes before the thing it
/// measures is worse than no bar — it is an invitation to speak.
///
/// Named rather than counted, because the two are started by different threads
/// and neither knows about the other. A count would let a missing
/// [`crate::Core::finished_load`] leave the orb loading for ever with nothing
/// to say which half never arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Load {
    /// Smart Turn v3 and Parakeet: about 3.8 s together, measured in T1.
    Hearing,
    /// The speech engine, when the chosen one has something to read. The
    /// system voice registers nothing, which is correct rather than an
    /// omission: there is no wait to show.
    Voice,
    /// A local main agent (epic 10): its weights downloaded if missing,
    /// `llama-server` started, the model read into memory **and** warmed by a
    /// first request. The CLI registers nothing — it has no model to load.
    Agent,
}

/// What the ear hands the main agent: words, or the sound itself.
///
/// Set by `app/` from the engine chosen for the main agent, read by the voice
/// thread, which loads Parakeet for one and leaves it unloaded for the other.
/// The core holds it because both the shell and the voice thread need it and
/// neither owns the other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ear {
    /// Parakeet transcribes; the agent receives text. Every mode works.
    #[default]
    Transcript,
    /// No transcription; the agent receives the audio of the turn — a local
    /// model that hears. **Hybrid cannot work here**: the name is looked for
    /// in a transcript, and there is none. It returns with the wake-word
    /// detector of epic 09, decided by the user on 2026-11-18.
    Audio,
}

/// How far the models are from being in memory, as the screen is told it.
///
/// Counts rather than a percentage, because a count is what is actually known.
/// Neither model reports its own progress: one of the ASR's two files is 8 MB
/// and the other is 2.4 GB, so even an honest "step one of two" inside it
/// would jump to half in a blink and sit there. So the bar is given the
/// finished steps and the total, and eases between them — a bar that does not
/// claim to know more than it does.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loading {
    pub done: u8,
    pub total: u8,
    /// What has not arrived yet, in a stable order, so the screen can name it.
    pub pending: Vec<Load>,
}

impl Loading {
    /// Nothing is being read. The resting state, and what an empty registry
    /// reports.
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty()
    }
}


/// One model file being fetched from the Hub — see [`crate::events::Event::ModelDownload`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Download {
    /// The file, as named in its repository.
    pub file: String,
    /// Its rank among the files missing, from 1.
    pub index: usize,
    pub count: usize,
    /// Bytes received, and the file's size — `0` until the Hub has said it.
    pub received: u64,
    pub size: u64,
}
/// Who is still reading, and how many have finished.
///
/// Small enough to look pointless and it is not: the whole defect this fixes
/// was two threads each believing it owned [`VoiceState::Loading`]. The ASR
/// thread set it and cleared it; the voice thread never touched it. Whoever
/// finished first turned the bar off. Here the state is derived from the set
/// instead of assigned by whoever last had an opinion.
#[derive(Default)]
pub struct LoadRegistry {
    done: u8,
    pending: std::collections::BTreeSet<Load>,
}

impl LoadRegistry {
    /// A model started loading. `None` when it was already known to be.
    pub fn begin(&mut self, what: Load) -> Option<Loading> {
        self.pending.insert(what).then(|| self.progress())
    }

    /// A model finished. `None` when it was not one we were waiting for —
    /// which means a caller is unbalanced, and inventing a step for it would
    /// hide that.
    pub fn finished(&mut self, what: Load) -> Option<Loading> {
        if !self.pending.remove(&what) {
            return None;
        }
        self.done += 1;
        let progress = self.progress();
        // The cycle is over, so the next one starts from zero. Waking from
        // `Off` reloads the hearing side, and that reload is its own wait —
        // not the continuation of this morning's.
        if self.pending.is_empty() {
            self.done = 0;
        }
        Some(progress)
    }

    pub fn progress(&self) -> Loading {
        Loading {
            done: self.done,
            total: self.done + self.pending.len() as u8,
            pending: self.pending.iter().copied().collect(),
        }
    }

    pub fn is_loading(&self) -> bool {
        !self.pending.is_empty()
    }
}

/// Why a mode change did not happen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum GateRefusal {
    /// A spoken trigger tried to leave `Off`. Structurally impossible — from
    /// `Off` nothing is listening — so reaching this means a caller fed the gate
    /// a trigger it invented.
    #[error("nothing is listening in off mode, so no spoken trigger can arrive from there")]
    SpokenWakeFromOff,
    /// A spoken sleep command was given in hybrid, which is already as quiet as
    /// a voice command may make it. Going further would mean `Off`, and
    /// decision 19 forbids reaching `Off` by voice: the door out of it is
    /// interface-only, so the door into it must be too.
    #[error("hybrid is as quiet as a voice command goes; off is interface-only")]
    SpokenSleepWouldReachOff,
    /// The trigger does not move anything from the current mode.
    #[error("{trigger:?} does not move the gate out of {from:?}")]
    NotApplicable {
        from: AudioMode,
        trigger: ModeTrigger,
    },
}

/// A mode change that happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeChange {
    pub from: AudioMode,
    pub to: AudioMode,
    pub trigger: ModeTrigger,
}

impl ModeChange {
    /// Whether this change has to load the models, which the interface must
    /// show: it costs about 3.8 s, measured in T1.
    pub fn loads_models(&self) -> bool {
        !self.from.models_loaded() && self.to.models_loaded()
    }

    /// Whether this change unloads them.
    pub fn unloads_models(&self) -> bool {
        self.from.models_loaded() && !self.to.models_loaded()
    }
}

/// How long active mode survives with no exchange, when the **user** switched
/// it on.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// How long a wake ARIA gave itself lasts after the last turn that **named**
/// it — decision 119, 2026-11-18.
///
/// Three minutes, set by the user: everything heard in that window goes to the
/// agent, which decides what was addressed to it. Only a turn naming ARIA
/// restarts it, so a room that keeps talking does not keep it awake.
pub const WAKE_WINDOW: Duration = Duration::from_secs(180);

/// How many turns ARIA may **answer** in a row without being named, when it
/// woke itself, before it decides the conversation is not with it.
///
/// Measured against the real thing on 2026-10-27: woken by its name, ARIA
/// answered **nine** consecutive utterances of a conversation about moving
/// furniture, including "n'hésitez pas à appeler quelqu'un près de vous". A
/// net independent of the clock, because the clock alone cannot catch a room
/// that never falls quiet.
///
/// Three, because two is a legitimate follow-up ("et après-demain ?") and four
/// is already a conversation ARIA is not part of.
///
/// **Answers, not turns heard**, since decision 119: the agent now gets every
/// utterance in the window and may stay silent on what was not addressed to
/// it. A silence costs the room nothing, so it does not count; only a sentence
/// actually spoken does — [`AudioGate::answer_spoken`].
pub const UNNAMED_TURNS_ALLOWED: u8 = 3;

/// The mode, and the two things that can move it on their own.
///
/// The core owns this, not the agent. The note in `docs/todos/agent-held-state.md`
/// says why in one line: the agent may *propose* a status, the core *applies*
/// it, or a restarted session loses the state and two sources of truth start
/// disagreeing about the most visible question in the product — is ARIA
/// listening or not.
#[derive(Debug)]
pub struct AudioGate {
    mode: AudioMode,
    /// End of the last turn actually processed — not the last noise heard.
    ///
    /// Decision 19 is explicit, and the failure it avoids is concrete: a
    /// television in the room would otherwise keep ARIA awake for ever.
    last_turn_ended: Option<Instant>,
    /// Whether ARIA put *itself* into active by hearing its name.
    ///
    /// The distinction the first real test made necessary, and the gate
    /// already knew it — it just threw it away. Two ways into [`AudioMode::Active`]
    /// that look identical afterwards and are not:
    ///
    /// - **the user switched it on.** They asked to be listened to; every turn
    ///   restarts the timer, and nothing counts turns. That is the setting
    ///   doing what it says;
    /// - **the name woke it.** That is a *temporary* wake for one exchange. An
    ///   overheard turn does not extend it, because extending an exchange with
    ///   speech that was never addressed to ARIA is how a room full of talk
    ///   keeps it awake for ever — the very failure decision 19 was written
    ///   against, arriving by the path meant to prevent it.
    woke_itself: bool,
    /// Consecutive answered turns that did not name ARIA, since it woke itself.
    unnamed_turns: u8,
    /// Whether the last turn heard named ARIA — what an answer to it is
    /// counted against. See [`Self::answer_spoken`].
    last_named: bool,
}

impl AudioGate {
    /// Starts in hybrid, because that is the resting mode.
    pub fn new() -> Self {
        Self {
            mode: AudioMode::Hybrid,
            last_turn_ended: None,
            woke_itself: false,
            unnamed_turns: 0,
            last_named: false,
        }
    }

    /// Starts in a given mode. For restoring what the user last chose.
    pub fn starting_at(mode: AudioMode) -> Self {
        Self {
            mode,
            last_turn_ended: None,
            woke_itself: false,
            unnamed_turns: 0,
            last_named: false,
        }
    }

    pub fn mode(&self) -> AudioMode {
        self.mode
    }

    /// Record that a turn finished being processed, and whether it named ARIA.
    ///
    /// `named` is what the first real test proved the gate needs. When the user
    /// switched active on, it is ignored: they asked to be listened to, so
    /// every turn restarts the timer. When ARIA woke *itself*, only a turn that
    /// names it restarts the timer — an overheard one does not extend a wake
    /// that was never meant to outlast one exchange.
    ///
    /// Hearing a turn no longer counts against a wake (decision 119): the agent
    /// may answer it with silence. What counts is [`Self::answer_spoken`].
    pub fn turn_processed(&mut self, named: bool) {
        if !self.woke_itself || named {
            self.last_turn_ended = Some(Instant::now());
        }
        if named {
            self.unnamed_turns = 0;
        }
        self.last_named = named;
    }

    /// The agent **said** something in answer to the last turn heard.
    ///
    /// Decision 119, two effects, both asked for by the user on 2026-11-18:
    ///
    /// - **it restarts the window.** The agent judged the turn was for it, and
    ///   an exchange that is going on is not one to cut at three minutes;
    /// - **it counts, if the turn did not name ARIA.** That is the thing
    ///   decision 47 was catching — ARIA speaking into a conversation that may
    ///   not be its own — and three in a row sends it back to named-only.
    ///
    /// A **silence** does neither: the agent decided the turn was not for it,
    /// and the room heard nothing. So a television cannot keep ARIA awake — it
    /// gets silences, which restart nothing.
    pub fn answer_spoken(&mut self) {
        if self.mode != AudioMode::Active {
            return;
        }
        self.last_turn_ended = Some(Instant::now());
        if self.woke_itself && !self.last_named {
            self.unnamed_turns = self.unnamed_turns.saturating_add(1);
        }
    }

    /// Whether active mode has run out of idle time, or of patience.
    ///
    /// Only meaningful in [`AudioMode::Active`]; hybrid has no timer, since
    /// there is nothing cheaper to fall back to.
    pub fn idle_expired(&self) -> bool {
        // A wake by the name gets the window the user set (decision 119); a
        // mode the user switched on keeps the original two minutes.
        let limit = if self.woke_itself {
            WAKE_WINDOW
        } else {
            IDLE_TIMEOUT
        };
        self.mode == AudioMode::Active
            && self
                .last_turn_ended
                .is_some_and(|t| t.elapsed() >= limit)
    }

    /// Whether ARIA has answered its way through a conversation it is not in.
    ///
    /// Only applies to a wake it gave itself: a user who switched active on is
    /// not second-guessed. Independent of [`Self::idle_expired`] on purpose —
    /// the clock cannot catch a room that never falls quiet, and the count
    /// cannot catch a room that falls quiet and stays that way.
    pub fn talked_past_its_welcome(&self) -> bool {
        self.mode == AudioMode::Active
            && self.woke_itself
            && self.unnamed_turns >= UNNAMED_TURNS_ALLOWED
    }

    /// Apply a trigger, or say why it does not apply.
    pub fn apply(&mut self, trigger: ModeTrigger) -> Result<ModeChange, GateRefusal> {
        use AudioMode::{Active, Hybrid, Off};
        use ModeTrigger::{
            IdleTimeout, Interface, ListenToAll, NameHeard, NamedOnly, Shortcut, SpokenSleep,
        };

        // Written out in full — every mode against every trigger, no guard —
        // so the compiler is the one checking that the table is complete. A
        // guarded catch-all would let a new trigger slip through as a silent
        // no-op, which on this particular state machine means the microphone
        // ends up somewhere nobody asked for.
        let to = match (self.mode, trigger) {
            // Off: nothing is listening, so no spoken trigger can have come
            // from here. The only way out is a hand on the interface.
            (Off, NameHeard | SpokenSleep) => return Err(GateRefusal::SpokenWakeFromOff),
            (Off, Interface | Shortcut) => Hybrid,
            // A policy is not a way in. The ear is shut, so there is nothing
            // to apply a policy to — and the interface says so by disabling
            // the switch rather than by moving the microphone somewhere
            // nobody asked for.
            (Off, ListenToAll | NamedOnly) => {
                return Err(GateRefusal::NotApplicable { from: Off, trigger });
            }
            (Off, IdleTimeout) => {
                return Err(GateRefusal::NotApplicable { from: Off, trigger });
            }

            // Hybrid: the resting mode. It waits for the name, and it has no
            // timer, because there is nothing cheaper to fall back to.
            (Hybrid, NameHeard) => Active,
            // The deliberate way in, and the only one besides the name. It
            // matters that it is distinguishable: decision 47 lets an
            // overheard turn extend a wake ARIA gave **itself** and not one
            // the user asked for, and the gate tells them apart by the trigger
            // that opened the mode.
            (Hybrid, ListenToAll) => Active,
            (Hybrid, NamedOnly) => {
                return Err(GateRefusal::NotApplicable {
                    from: Hybrid,
                    trigger,
                });
            }
            (Hybrid, Interface | Shortcut) => Off,
            (Hybrid, SpokenSleep) => return Err(GateRefusal::SpokenSleepWouldReachOff),
            (Hybrid, IdleTimeout) => {
                return Err(GateRefusal::NotApplicable {
                    from: Hybrid,
                    trigger,
                });
            }

            // Active: falls back to the resting mode, never to off.
            (Active, SpokenSleep | IdleTimeout | NamedOnly) => Hybrid,
            (Active, ListenToAll) => {
                return Err(GateRefusal::NotApplicable {
                    from: Active,
                    trigger,
                });
            }
            (Active, Interface | Shortcut) => Off,
            (Active, NameHeard) => {
                // Already listening to everything; the name is just a word.
                return Err(GateRefusal::NotApplicable {
                    from: Active,
                    trigger,
                });
            }
        };

        let from = std::mem::replace(&mut self.mode, to);
        // A fresh timer on the way in, and no stale one left behind on the way
        // out: entering active with `last_turn_ended` still set from an earlier
        // visit would expire the mode before a word is spoken.
        self.last_turn_ended = (to == Active).then(Instant::now);
        // How we got in is remembered, because it decides what keeps us in.
        self.woke_itself = to == Active && trigger == NameHeard;
        self.unnamed_turns = 0;
        // The turn that carried the name is what woke us: an answer to it is
        // an answer to being named, and does not count against the wake.
        self.last_named = trigger == NameHeard;
        Ok(ModeChange { from, to, trigger })
    }
}

impl Default for AudioGate {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resting_mode_is_hybrid_not_off() {
        assert_eq!(AudioGate::new().mode(), AudioMode::Hybrid);
    }

    #[test]
    fn the_name_is_what_opens_active() {
        let mut gate = AudioGate::new();
        let change = gate.apply(ModeTrigger::NameHeard).unwrap();
        assert_eq!(change.to, AudioMode::Active);
        assert!(!change.loads_models(), "hybrid already had them loaded");
    }

    #[test]
    fn no_voice_command_leads_to_off() {
        let mut gate = AudioGate::new();
        gate.apply(ModeTrigger::NameHeard).unwrap();
        // The deepest a spoken command may go is the resting mode.
        assert_eq!(
            gate.apply(ModeTrigger::SpokenSleep).unwrap().to,
            AudioMode::Hybrid
        );
        assert_eq!(gate.mode(), AudioMode::Hybrid);
    }

    #[test]
    fn nothing_spoken_can_wake_from_off() {
        let mut gate = AudioGate::starting_at(AudioMode::Off);
        assert_eq!(
            gate.apply(ModeTrigger::NameHeard),
            Err(GateRefusal::SpokenWakeFromOff)
        );
        assert_eq!(gate.mode(), AudioMode::Off);
    }

    #[test]
    fn waking_from_off_is_the_only_transition_that_loads_models() {
        let mut gate = AudioGate::starting_at(AudioMode::Off);
        let change = gate.apply(ModeTrigger::Interface).unwrap();
        assert_eq!(change.to, AudioMode::Hybrid);
        assert!(
            change.loads_models(),
            "this is the 3.8 s the interface shows"
        );
    }

    #[test]
    fn the_interface_can_always_cut_the_ear() {
        for mode in [AudioMode::Hybrid, AudioMode::Active] {
            let mut gate = AudioGate::starting_at(mode);
            let change = gate.apply(ModeTrigger::Interface).unwrap();
            assert_eq!(change.to, AudioMode::Off);
            assert!(change.unloads_models());
        }
    }

    #[test]
    fn the_idle_timer_only_runs_in_active_mode() {
        let mut gate = AudioGate::new();
        assert!(!gate.idle_expired(), "hybrid has nothing to fall back to");
        gate.apply(ModeTrigger::NameHeard).unwrap();
        gate.turn_processed(true);
        assert!(!gate.idle_expired(), "two minutes have not gone by");
    }

    /// The failure the first real test produced, in miniature.
    ///
    /// On 2026-10-27 ARIA was woken by its name, then answered nine utterances
    /// of a conversation about moving furniture — because every one of them
    /// restarted the two-minute timer. A room that never falls quiet kept it
    /// awake for ever, which is the exact failure decision 19 was written
    /// against, arriving by the path meant to prevent it.
    #[test]
    fn an_overheard_turn_does_not_extend_a_wake_aria_gave_itself() {
        let mut gate = AudioGate::new();
        gate.apply(ModeTrigger::NameHeard).unwrap();

        // Decision 119: heard and left unanswered, any number of turns is
        // nothing — the agent judged they were not for it.
        for _ in 0..10 {
            gate.turn_processed(false);
        }
        assert!(
            !gate.talked_past_its_welcome(),
            "un silence de l'agent ne compte pas"
        );

        // Three turns of a conversation ARIA is not in — and answered.
        for _ in 0..UNNAMED_TURNS_ALLOWED {
            gate.turn_processed(false);
            gate.answer_spoken();
        }
        assert!(
            gate.talked_past_its_welcome(),
            "three answers nobody addressed to it is a conversation it is not in"
        );

        // And being named clears the count: a genuine exchange is not punished
        // for having had a pause in it.
        gate.turn_processed(true);
        assert!(!gate.talked_past_its_welcome());
    }

    /// The switch in the top bar, both ways — and the thing that makes it
    /// different from the name.
    #[test]
    fn choosing_a_policy_is_not_the_same_as_being_woken_by_name() {
        let mut gate = AudioGate::new();
        let change = gate.apply(ModeTrigger::ListenToAll).unwrap();
        assert_eq!(change.to, AudioMode::Active);

        // Deliberate, so decision 47's leash does not apply: the user asked to
        // be listened to and is not second-guessed.
        for _ in 0..10 {
            gate.turn_processed(false);
        }
        assert!(!gate.talked_past_its_welcome());

        // And back, by the same switch.
        assert_eq!(
            gate.apply(ModeTrigger::NamedOnly).unwrap().to,
            AudioMode::Hybrid
        );
    }

    /// A policy is not a way to open the ear. From off there is nothing to
    /// apply one to, and the refusal is a value the interface can show.
    #[test]
    fn a_policy_cannot_open_a_shut_ear() {
        let mut gate = AudioGate::starting_at(AudioMode::Off);
        assert!(gate.apply(ModeTrigger::ListenToAll).is_err());
        assert!(gate.apply(ModeTrigger::NamedOnly).is_err());
        assert_eq!(gate.mode(), AudioMode::Off);
    }

    /// A user who switched active on is not second-guessed.
    ///
    /// The other half of the rule, and the half that keeps the setting
    /// honest: "répond à tout ce qu'il entend" has to mean that, or the switch
    /// is a suggestion.
    #[test]
    fn a_wake_the_user_asked_for_is_never_cut_short() {
        let mut gate = AudioGate::starting_at(AudioMode::Active);
        for _ in 0..20 {
            gate.turn_processed(false);
        }
        assert!(
            !gate.talked_past_its_welcome(),
            "they asked to be listened to; the count does not apply"
        );
        assert!(
            !gate.idle_expired(),
            "and every turn restarts the timer, which is the setting doing what it says"
        );
    }

    #[test]
    fn entering_active_does_not_inherit_an_old_timer() {
        let mut gate = AudioGate::new();
        gate.apply(ModeTrigger::NameHeard).unwrap();
        gate.turn_processed(true);
        gate.apply(ModeTrigger::SpokenSleep).unwrap();
        gate.apply(ModeTrigger::NameHeard).unwrap();
        // Were the timer inherited, a mode entered after a long hybrid spell
        // would expire before anybody got a word in.
        assert!(!gate.idle_expired());
    }

    /// The registry exists because of one defect, and this is it.
    mod loading {
        use super::*;

        #[test]
        fn nothing_is_loading_by_default() {
            let registry = LoadRegistry::default();
            assert!(!registry.is_loading());
            assert!(registry.progress().is_idle());
            assert_eq!(registry.progress().total, 0);
        }

        /// The defect, written as a test: the hearing side finishes first, and
        /// the bar must **not** be over. This is what used to light the orb up
        /// while ARIA still had nothing to speak with.
        #[test]
        fn the_first_model_to_finish_does_not_end_the_wait() {
            let mut registry = LoadRegistry::default();
            registry.begin(Load::Voice);
            registry.begin(Load::Hearing);

            let progress = registry.finished(Load::Hearing).expect("it was pending");
            assert_eq!((progress.done, progress.total), (1, 2));
            assert_eq!(progress.pending, vec![Load::Voice]);
            assert!(!progress.is_idle(), "the voice has not arrived yet");
            assert!(registry.is_loading());

            let progress = registry.finished(Load::Voice).expect("it was pending");
            assert_eq!((progress.done, progress.total), (2, 2));
            assert!(progress.is_idle());
            assert!(!registry.is_loading());
        }

        /// One model is the ordinary case — the system voice reads nothing —
        /// and it must still report a complete bar rather than a half-finished
        /// one.
        #[test]
        fn a_single_model_fills_the_whole_bar() {
            let mut registry = LoadRegistry::default();
            registry.begin(Load::Hearing);
            assert_eq!(registry.progress().total, 1);
            let progress = registry.finished(Load::Hearing).expect("it was pending");
            assert_eq!((progress.done, progress.total), (1, 1));
        }

        #[test]
        fn beginning_twice_is_one_load() {
            let mut registry = LoadRegistry::default();
            assert!(registry.begin(Load::Hearing).is_some());
            assert!(
                registry.begin(Load::Hearing).is_none(),
                "already known, so nothing changed and nothing is published"
            );
            assert_eq!(registry.progress().total, 1);
        }

        /// An unbalanced caller is reported, not absorbed. Absorbing it would
        /// mean a bar that never fills and no line saying why.
        #[test]
        fn finishing_something_that_never_began_is_refused() {
            let mut registry = LoadRegistry::default();
            assert!(registry.finished(Load::Voice).is_none());
            assert_eq!(registry.progress().total, 0);
        }

        /// Waking from `Off` reloads the hearing side, and that wait is its
        /// own: the counter must not carry this morning's two steps into it.
        #[test]
        fn a_second_cycle_starts_from_zero() {
            let mut registry = LoadRegistry::default();
            registry.begin(Load::Hearing);
            registry.finished(Load::Hearing);

            registry.begin(Load::Hearing);
            let progress = registry.progress();
            assert_eq!((progress.done, progress.total), (0, 1));
        }
    }
}

#[cfg(test)]
mod wake_window {
    use super::*;

    /// Decision 119: an answer the agent speaks restarts the window, the way a
    /// turn naming ARIA does; a silence restarts nothing.
    #[test]
    fn a_spoken_answer_restarts_the_window_and_a_silence_does_not() {
        let mut gate = AudioGate::new();
        gate.apply(ModeTrigger::NameHeard).unwrap();
        let opened = gate.last_turn_ended.expect("the wake opens the window");

        std::thread::sleep(Duration::from_millis(5));
        gate.turn_processed(false);
        assert_eq!(gate.last_turn_ended, Some(opened), "un tour entendu ne relance rien");

        gate.answer_spoken();
        assert!(
            gate.last_turn_ended.expect("still open") > opened,
            "une réponse prononcée relance la fenêtre"
        );
    }

    /// The answer to the very turn that carried the name is an answer to being
    /// named, and does not count against the wake.
    #[test]
    fn answering_the_waking_turn_costs_nothing() {
        let mut gate = AudioGate::new();
        gate.apply(ModeTrigger::NameHeard).unwrap();
        for _ in 0..5 {
            gate.answer_spoken();
        }
        assert!(!gate.talked_past_its_welcome());
    }

    /// The window of a wake by name is the user's three minutes; a mode the
    /// user switched on keeps two.
    #[test]
    fn a_wake_by_name_lasts_three_minutes() {
        assert_eq!(WAKE_WINDOW, Duration::from_secs(180));
        assert_eq!(IDLE_TIMEOUT, Duration::from_secs(120));
    }
}
