//! The event broker and the audit journal every path writes to.
//!
//! Both the short and the long path report here, so the interface shows what
//! is happening live and the audit survives the window being closed.
//!
//! # One channel, typed events, and a sequence number
//!
//! The contract is sketched in the original specification. What that sketch
//! gets right, and what matters most here: the core keeps the sequence, so a
//! client that reconnects **replays what it missed** instead of starting from
//! a blank screen. That is the whole reason every event carries a [`Seq`].
//!
//! Two deliberate departures from that sketch, both because a later decision
//! overtook it. They are named here rather than resolved silently, which is
//! what `AGENTS.md` asks for when `docs/` lags behind the decision log:
//!
//! - §4 lists `preview.ready`. Previews became **tiles** with decisions 10 and
//!   11 — an MCP Apps resource served from a second origin, living no longer
//!   than the window that shows it. The event is [`Event::TileReady`].
//! - §4 lists `voice.state` and nothing else for audio. Decision 19 adds a
//!   second, independent axis: the three listening modes. The pipeline's
//!   instantaneous state and the listening *policy* are not the same quantity,
//!   so there are two events — `voice.state` and `audio.mode`.
//!
//! And one addition §4 does not have, required by decision 7: when two
//! channels are offered a permission request, the first to answer wins and the
//! other invitation must withdraw *saying where the answer came from*. Without
//! [`Event::PermissionResolved`] the window cannot learn that the voice
//! already answered, and it would keep showing a decided request.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::audio::{AudioMode, Download, Loading, ModeTrigger, VoiceState};
use crate::permissions::{Channel, Decision, PermissionRequest, Profile, RequestId};
use crate::sessions::{ActivityStep, SessionId, SessionState};

/// Position in the single ordered stream the core publishes.
///
/// Monotonic, never reused, and it starts at 1 — so `since = 0` means "I have
/// seen nothing, give me everything you still hold".
pub type Seq = u64;

/// What the core tells its clients.
///
/// Tagged with the dotted names of the original specification rather than
/// Rust-shaped ones: this is a wire contract, read from TypeScript on the
/// other side.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    /// A conversation became the live one — a fresh thread at startup, or an
    /// old one the user clicked in the left-hand menu.
    ///
    /// Published rather than returned only to the caller: the menu has to move
    /// its highlight, and a second window would otherwise show a thread that is
    /// no longer the one being written into.
    #[serde(rename = "conversation.opened")]
    ConversationOpened {
        id: crate::store::history::ConversationId,
    },

    /// A session was opened, with the profile it runs under.
    ///
    /// Epic 04 insists the profile is shown *per session*, and says why: once
    /// it is overridable at launch, a list without that column leaves nobody
    /// knowing what runs under which guard rail.
    #[serde(rename = "session.opened")]
    SessionOpened {
        session: SessionId,
        title: String,
        profile: Profile,
    },

    /// The engine assigned its own identifier to a session.
    ///
    /// Published rather than stored quietly, and the reason is the rule this
    /// whole design rests on: a snapshot carries `native_id`, so a client that
    /// folded only events would end up disagreeing with one that re-read the
    /// snapshot. Nothing changes state without the stream saying so.
    #[serde(rename = "session.engaged")]
    SessionEngaged {
        session: SessionId,
        native_id: String,
    },

    /// Where a session is in its work.
    #[serde(rename = "session.state")]
    SessionState {
        session: SessionId,
        state: SessionState,
    },

    /// What the USER is saying, as the ASR hears it.
    ///
    /// Distinct from [`Event::MessageDelta`], which is what ARIA *answers*, and
    /// the two were conflated in neither direction by accident: the main
    /// window's transcript shows this one, and without it that area has nothing
    /// to display. Missing from the original specification as well, which
    /// only models the answer — noted there as a gap rather than invented here.
    ///
    /// `final_` marks the end of an utterance. A partial is shown dimmed and
    /// italic and WILL be replaced; a final one will not. Carrying the
    /// distinction is what lets the interface avoid flickering a corrected
    /// transcript as though the user had said two different things.
    #[serde(rename = "transcript.partial")]
    Transcript {
        text: String,
        #[serde(rename = "final")]
        final_: bool,
    },

    /// A fragment of a streamed textual answer.
    #[serde(rename = "message.delta")]
    MessageDelta { session: SessionId, text: String },

    /// What ARIA says out loud.
    ///
    /// No session, and that is the point: this is the main agent's own voice —
    /// its answer, a waiting phrase, or a result read back from a session —
    /// and it belongs to the conversation, not to a task. [`Event::MessageDelta`]
    /// is a session reporting to the screen; this is ARIA talking to the room.
    ///
    /// Published even though the text also goes to the TTS, for the reason the
    /// whole stream exists: a window that reconnects has to be able to see what
    /// was said, and reading it off an audio pipeline is not an option.
    #[serde(rename = "spoken")]
    Spoken { text: String },

    /// One step of execution — a tool, a command, a file — with its status.
    #[serde(rename = "activity.step")]
    ActivityStep {
        session: SessionId,
        step: ActivityStep,
    },

    /// A session wants permission to act. It does not expire; see
    /// [`crate::permissions`].
    #[serde(rename = "permission.request")]
    PermissionRequest { request: PermissionRequest },

    /// The request was answered — and by whom. Decision 7: the losing channel
    /// withdraws its invitation saying where the answer came from, because the
    /// action may already be under way and cannot be overwritten after the
    /// fact.
    #[serde(rename = "permission.resolved")]
    PermissionResolved {
        id: RequestId,
        decision: Decision,
        by: Channel,
    },

    /// A tile already on screen has new contents, or its files changed.
    ///
    /// Carries only the URI: the window reloads the frame it already has,
    /// keeping where the user put it and how they sized it. A second
    /// `tile.ready` would have made a second panel appear next to the first.
    #[serde(rename = "tile.updated")]
    TileUpdated { uri: String },

    /// A tile is available to render. Replaces §4's `preview.ready`.
    #[serde(rename = "tile.ready")]
    TileReady {
        session: SessionId,
        /// The `ui://` resource, as MCP Apps names it.
        uri: String,
        title: String,
        /// Where to frame it, on the sandbox origin.
        ///
        /// Carried rather than derived by the window: a `ui://` URI is not a
        /// path, and the rule that stops a model-chosen string reaching one
        /// holds only while it lives in [`crate::tiles`] alone.
        source: String,
        /// The fields to draw under it, declared by the session.
        ///
        /// Missing until 2026-11-16: the window read `form` off this event
        /// and the core never put it there, so no declared form could ever
        /// appear in the application.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        form: Option<crate::tiles::Form>,
        /// PDFs the window frames beside the tile, outside the sandbox —
        /// see [`crate::tiles::Tile::documents`].
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        documents: Vec<String>,
    },

    /// The audio pipeline, right now: speaking, listening, interrupted.
    #[serde(rename = "voice.state")]
    VoiceState { state: VoiceState },

    /// How far the models are from being in memory.
    ///
    /// Published alongside [`Event::VoiceState`] rather than folded into it,
    /// and the split is deliberate: the state is what the orb paints, this is
    /// what the loading bar measures, and one of them is a small enumeration
    /// while the other changes shape the day a third model appears.
    ///
    /// What the numbers mean, and why they are counts rather than a
    /// percentage, is on [`Loading`] itself.
    #[serde(rename = "voice.loading")]
    VoiceLoading { loading: Loading },

    /// A model file coming down from the Hub, or `None` once nothing is.
    ///
    /// Beside [`Event::VoiceLoading`], not inside it: a load is a count of
    /// models, this is bytes of one file — 2.4 GB for Parakeet's encoder on a
    /// first start, which is minutes the first-run screen has to show (T12).
    #[serde(rename = "models.download")]
    ModelDownload { download: Option<Download> },

    /// The listening policy, and what moved it. Decision 19.
    #[serde(rename = "audio.mode")]
    AudioMode {
        mode: AudioMode,
        trigger: ModeTrigger,
    },

    /// A session finished its work.
    #[serde(rename = "task.done")]
    TaskDone { session: SessionId, summary: String },

    /// A session failed. Distinct from `task.done` so the interface can show
    /// the difference without parsing a summary.
    #[serde(rename = "task.error")]
    TaskError { session: SessionId, message: String },

    /// Part of ARIA stopped working, in words for the user — or, with `None`,
    /// works again.
    ///
    /// Added on 2026-11-27, after a day where ARIA was deaf and its agent
    /// logged out, and the window said neither: both were only in the log.
    #[serde(rename = "fault")]
    Fault {
        part: Part,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

/// What can stop working on its own, for [`Event::Fault`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Part {
    /// The ear: the microphone or the model that transcribes it.
    Hearing,
    /// The main agent, which answers.
    Agent,
}

/// An event with its position in the stream, and when it happened.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope {
    pub seq: Seq,
    /// Milliseconds since the Unix epoch. A number rather than a formatted
    /// date: the window renders it in the user's locale, the core does not
    /// pick one for it.
    pub at: u64,
    #[serde(flatten)]
    pub event: Event,
}

/// What a client is handed when it asks to resume.
///
/// The gap case is said out loud rather than papered over. Silently returning
/// the oldest retained events would show a plausible, wrong history — a hole
/// in the middle of the activity feed that nobody can see. The client is told
/// to resynchronise from current state instead.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Resume {
    /// Everything the client missed, in order.
    #[serde(rename = "resume.replay")]
    Replay { from: Seq, events: Vec<Envelope> },
    /// The requested position fell off the ring. `oldest` is the earliest the
    /// broker still holds.
    #[serde(rename = "resume.gap")]
    Gap { asked: Seq, oldest: Seq },
}

/// How many events the broker keeps for replay.
///
/// This covers a window that was closed for a while, not a full audit trail:
/// the durable record is SQLite's job ([`crate::store`]). Sized so a long
/// session's activity feed survives a reconnect, not so the ring becomes the
/// archive.
const RING_CAPACITY: usize = 4096;

/// The single ordered stream, live and replayable.
///
/// Live delivery is a [`tokio::sync::broadcast`]; history is a ring under a
/// plain [`Mutex`], which is never held across an await.
pub struct Broker {
    live: tokio::sync::broadcast::Sender<Envelope>,
    history: Mutex<History>,
}

struct History {
    next_seq: Seq,
    ring: VecDeque<Envelope>,
}

impl Broker {
    pub fn new() -> Self {
        // Capacity of the live channel, not of the history. A slow client that
        // overflows it gets a `RecvError::Lagged`, which is precisely the
        // signal to fall back to a replay — so this is a recoverable failure,
        // not a lost event.
        let (live, _) = tokio::sync::broadcast::channel(1024);
        Self {
            live,
            history: Mutex::new(History {
                next_seq: 1,
                ring: VecDeque::with_capacity(RING_CAPACITY),
            }),
        }
    }

    /// Publish an event: assign it a sequence number, keep it, hand it out.
    ///
    /// Publishing with no client connected is normal, not an error: sleep is
    /// not shutdown, and the core keeps working with every window closed.
    pub fn publish(&self, event: Event) -> Seq {
        let envelope = {
            let mut h = self.history.lock().expect("broker history mutex poisoned");
            let seq = h.next_seq;
            h.next_seq += 1;
            let envelope = Envelope {
                seq,
                at: now_millis(),
                event,
            };
            if h.ring.len() == RING_CAPACITY {
                h.ring.pop_front();
            }
            h.ring.push_back(envelope.clone());
            envelope
        };
        let seq = envelope.seq;
        // The error case here is "nobody is listening", which is not a failure.
        let _ = self.live.send(envelope);
        seq
    }

    /// Everything published after `since`, or a [`Resume::Gap`] if that
    /// position has fallen out of the ring.
    pub fn resume_from(&self, since: Seq) -> Resume {
        let h = self.history.lock().expect("broker history mutex poisoned");
        let oldest = h.ring.front().map(|e| e.seq).unwrap_or(h.next_seq);
        // `since == 0` is "I have seen nothing": whatever is left is a
        // legitimate answer, never a gap. Otherwise the client holds `since`,
        // so it needs `since + 1` to still be there for the stream to join up.
        if since > 0 && since + 1 < oldest {
            return Resume::Gap {
                asked: since,
                oldest,
            };
        }
        Resume::Replay {
            from: since,
            events: h.ring.iter().filter(|e| e.seq > since).cloned().collect(),
        }
    }

    /// Subscribe to what comes next.
    ///
    /// Subscribe *before* calling [`Self::resume_from`], and drop any replayed
    /// sequence that also arrives live: the other order loses every event
    /// published between the two calls.
    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Envelope> {
        self.live.subscribe()
    }

    /// The sequence number the next published event will get.
    pub fn next_seq(&self) -> Seq {
        self.history
            .lock()
            .expect("broker history mutex poisoned")
            .next_seq
    }
}

impl Default for Broker {
    fn default() -> Self {
        Self::new()
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tick(n: u32) -> Event {
        Event::MessageDelta {
            session: SessionId::new(),
            text: n.to_string(),
        }
    }

    #[test]
    fn sequence_starts_at_one_and_never_repeats() {
        let b = Broker::new();
        assert_eq!(b.publish(tick(1)), 1);
        assert_eq!(b.publish(tick(2)), 2);
        assert_eq!(b.next_seq(), 3);
    }

    #[test]
    fn a_fresh_client_gets_everything_still_held() {
        let b = Broker::new();
        b.publish(tick(1));
        b.publish(tick(2));
        match b.resume_from(0) {
            Resume::Replay { events, .. } => assert_eq!(events.len(), 2),
            Resume::Gap { .. } => panic!("an empty client is never a gap"),
        }
    }

    #[test]
    fn resuming_mid_stream_yields_only_what_was_missed() {
        let b = Broker::new();
        for i in 0..5 {
            b.publish(tick(i));
        }
        match b.resume_from(3) {
            Resume::Replay { events, .. } => {
                assert_eq!(events.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![4, 5]);
            }
            Resume::Gap { .. } => panic!("3 is still in the ring"),
        }
    }

    #[test]
    fn a_position_off_the_ring_is_a_gap_not_a_partial_history() {
        let b = Broker::new();
        for i in 0..(RING_CAPACITY as u32 + 10) {
            b.publish(tick(i));
        }
        // Position 1 has been evicted; the client must resynchronise rather
        // than be shown a history with a hole in it.
        match b.resume_from(1) {
            Resume::Gap { asked, oldest } => {
                assert_eq!(asked, 1);
                assert!(oldest > 1);
            }
            Resume::Replay { .. } => panic!("an evicted position must be reported"),
        }
    }

    #[test]
    fn the_wire_names_are_the_dotted_ones() {
        let json = serde_json::to_string(&Event::VoiceState {
            state: VoiceState::Listening,
        })
        .unwrap();
        assert!(json.contains("\"type\":\"voice.state\""), "{json}");
    }
}
