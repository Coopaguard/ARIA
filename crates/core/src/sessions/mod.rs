//! Session lifecycle: start, resume, retention.
//!
//! A session that has been silent for more than three minutes offers the user
//! a kill button rather than being killed; sessions survive as long as the
//! application runs. See epic 05.
//!
//! # Never a silent kill, and decision 8 says why
//!
//! Three minutes without an event marks a session [`SessionState::Unresponsive`]
//! and puts a **Stop** button on it. It does not end it. The reasons are
//! measured, not cautious: `--max-turns` and `--max-budget-usd` already give
//! hard ceilings, and a long compile legitimately produces nothing. A session
//! waiting on a permission request is not silent at all — it is blocked, which
//! is a different thing, and [`SessionState::AwaitingPermission`] keeps it out
//! of the silence count.

pub mod supervisor;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::permissions::Profile;

/// How long a session may go without an event before it is marked
/// unresponsive. Decision 8.
pub const SILENCE_BEFORE_UNRESPONSIVE: Duration = Duration::from_secs(180);

/// A session, as the interface addresses it.
///
/// A UUID rather than the provider's own handle: Claude Code's session id is
/// one provider's vocabulary, and the right-hand menu has to list sessions from
/// a provider that will not share its format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(uuid::Uuid);

impl SessionId {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

/// Read one back — what the history table stores and the interface sends.
///
/// Added with `store::history`: a session identifier survives a restart in a
/// column, and the row has to become a `SessionId` again without going through
/// serde, which would make the text representation a detail of a derive.
impl std::str::FromStr for SessionId {
    type Err = uuid::Error;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self)
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Where a session is in its work.
///
/// The first five come from the original specification; the last three from
/// decision 8, which needed names for "silent", "finished" and "no longer
/// resumable".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Alive, nothing in flight.
    Idle,
    /// Reasoning, no tool running.
    Thinking,
    /// A tool or a command is running.
    Executing,
    /// Blocked on a permission request.
    ///
    /// Not a form of silence: this session is waiting on a human, and the
    /// request it waits on never expires.
    AwaitingPermission,
    /// Three minutes with no event. Carries a **Stop** button, never an
    /// automatic kill.
    Unresponsive,
    /// Finished, with a result.
    Done,
    /// Finished, with an error.
    Failed,
    /// Past the provider's resumption window.
    ///
    /// Decision 8: the entry **stays in the thread**, marked no longer
    /// resumable. What is lost is the resumption, not the memory.
    Expired,
}

impl SessionState {
    /// Whether the session is still doing something.
    pub fn is_live(self) -> bool {
        matches!(
            self,
            SessionState::Idle
                | SessionState::Thinking
                | SessionState::Executing
                | SessionState::AwaitingPermission
                | SessionState::Unresponsive
        )
    }

    /// Whether silence here is worth reporting.
    ///
    /// A session blocked on a permission request is not silent, and a finished
    /// one has nothing left to say.
    fn silence_counts(self) -> bool {
        matches!(
            self,
            SessionState::Idle | SessionState::Thinking | SessionState::Executing
        )
    }
}

/// One step of execution, as the right-hand menu lists it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActivityStep {
    /// What the agent reached for — a tool name, a command, a path.
    pub label: String,
    /// The command itself when there is one.
    ///
    /// Carried from the start, and epic 02 says why it cannot wait: a pattern
    /// registry that only ever sees tool names can express no pattern. Without
    /// this field the V0 forecloses the V1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    pub status: StepStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Running,
    Ok,
    Failed,
    /// Refused — by the user, or by the registry.
    Denied,
}

/// A session as the interface shows it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub title: String,
    /// The profile this session actually runs under.
    ///
    /// Per session, not global, and epic 04 insists it is displayed: once the
    /// profile is overridable at launch, a list without this column leaves
    /// nobody knowing what runs under which guard rail.
    pub profile: Profile,
    pub state: SessionState,
    /// Milliseconds since the Unix epoch.
    pub opened_at: u64,
    /// The provider's own identifier for this session.
    ///
    /// `None` until the provider assigns one. It lives here rather than on the
    /// live process handle because **a resume has to outlive the process**: the
    /// handle is dropped when the session ends, and that is exactly when the id
    /// starts being useful.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_id: Option<String>,
}

/// The live sessions, and the silence clock over them.
#[derive(Debug, Default)]
pub struct SessionRegistry {
    sessions: HashMap<SessionId, Entry>,
    /// Insertion order, so the right-hand menu is stable between frames. A
    /// `HashMap` iteration order would shuffle the list on every render.
    order: Vec<SessionId>,
}

#[derive(Debug)]
struct Entry {
    session: Session,
    last_event: Instant,
}

impl SessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a session under a profile. Returns its id.
    pub fn open(&mut self, title: impl Into<String>, profile: Profile) -> SessionId {
        let id = SessionId::new();
        let session = Session {
            id,
            title: title.into(),
            profile,
            state: SessionState::Idle,
            native_id: None,
            opened_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        };
        self.sessions.insert(
            id,
            Entry {
                session,
                last_event: Instant::now(),
            },
        );
        self.order.push(id);
        id
    }

    pub fn get(&self, id: SessionId) -> Option<&Session> {
        self.sessions.get(&id).map(|e| &e.session)
    }

    /// Every session, oldest first.
    pub fn all(&self) -> Vec<Session> {
        self.order
            .iter()
            .filter_map(|id| self.sessions.get(id))
            .map(|e| e.session.clone())
            .collect()
    }

    /// Move a session to a new state, and restart its silence clock.
    ///
    /// Returns the previous state, or `None` if the id is unknown.
    pub fn set_state(&mut self, id: SessionId, state: SessionState) -> Option<SessionState> {
        let entry = self.sessions.get_mut(&id)?;
        let previous = entry.session.state;
        entry.session.state = state;
        entry.last_event = Instant::now();
        Some(previous)
    }

    /// Record the provider's own identifier for a session.
    pub fn set_native_id(&mut self, id: SessionId, native: impl Into<String>) -> bool {
        match self.sessions.get_mut(&id) {
            Some(entry) => {
                entry.session.native_id = Some(native.into());
                true
            }
            None => false,
        }
    }

    /// Note that a session produced an event, without changing its state.
    pub fn touch(&mut self, id: SessionId) {
        if let Some(entry) = self.sessions.get_mut(&id) {
            entry.last_event = Instant::now();
        }
    }

    /// Sessions that have gone quiet for longer than
    /// [`SILENCE_BEFORE_UNRESPONSIVE`] and are not already marked.
    ///
    /// Returns them for the caller to mark and announce. Nothing in here kills
    /// anything: decision 8 leaves that to the user and to the CLI's own hard
    /// ceilings.
    pub fn gone_quiet(&self) -> Vec<SessionId> {
        self.order
            .iter()
            .filter(|id| {
                self.sessions.get(*id).is_some_and(|e| {
                    e.session.state.silence_counts()
                        && e.last_event.elapsed() >= SILENCE_BEFORE_UNRESPONSIVE
                })
            })
            .copied()
            .collect()
    }

    /// Drop a session from the live list.
    ///
    /// Only for a session the user closed. A finished one keeps its entry: the
    /// thread is the memory, and decision 8 keeps even an expired session
    /// visible in it.
    pub fn forget(&mut self, id: SessionId) -> Option<Session> {
        self.order.retain(|other| *other != id);
        self.sessions.remove(&id).map(|e| e.session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_carries_the_profile_it_runs_under() {
        let mut reg = SessionRegistry::new();
        let id = reg.open("renommer les captures", Profile::Strict);
        assert_eq!(reg.get(id).unwrap().profile, Profile::Strict);
    }

    #[test]
    fn the_list_keeps_the_order_sessions_were_opened_in() {
        let mut reg = SessionRegistry::new();
        let first = reg.open("un", Profile::Normal);
        let second = reg.open("deux", Profile::Plan);
        let third = reg.open("trois", Profile::Autonomous);
        assert_eq!(
            reg.all().iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![first, second, third]
        );
    }

    #[test]
    fn waiting_on_a_permission_is_not_silence() {
        let mut reg = SessionRegistry::new();
        let id = reg.open("compiler", Profile::Normal);
        reg.set_state(id, SessionState::AwaitingPermission);
        // The clock is not what makes this pass — the state is. A session
        // blocked on a request that never expires must not also be reported
        // as having stopped answering.
        assert!(!reg.get(id).unwrap().state.silence_counts());
        assert!(reg.gone_quiet().is_empty());
    }

    #[test]
    fn a_finished_session_keeps_its_entry() {
        let mut reg = SessionRegistry::new();
        let id = reg.open("fini", Profile::Normal);
        reg.set_state(id, SessionState::Expired);
        // Decision 8: what is lost is the resumption, not the memory.
        assert_eq!(reg.all().len(), 1);
        assert!(!reg.get(id).unwrap().state.is_live());
    }
}
