//! One policy, two paths into it.
//!
//! The short path is stopped by the guard before a native action runs; the
//! long path arrives through the relay the CLI calls. A pattern registry
//! answers alone for recognised side-effect-free commands and assigns the
//! risk level; anything else reaches the user.
//!
//! `bypassPermissions` is reachable from no profile. A pending request never
//! expires, and closing the orb is never a refusal. See
//! The original specification.
//!
//! # What is here, and what is not yet
//!
//! Here: the four profiles, the risk scale, a request that cannot expire, and
//! the rule that the first channel to answer wins. That is what the window has
//! to render, so it is what the core has to hold.
//!
//! Not here: the **pattern registry** and the **relay**. They arrive with the
//! bridge in T5, together, never after — a session that is not guarded is not
//! an MVP, it is a debt nobody can see. Building their types now would be
//! guessing at a boundary instead of observing one, which `AGENTS.md` asks us
//! not to do.

pub mod registry;

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::sessions::SessionId;

/// The stable setting the user chooses, in our vocabulary rather than a
/// provider's.
///
/// Decision 5, and the reason it is a profile and not a mode: a mode is one
/// provider's word and does not transpose. Each provider declares what each
/// profile means on its side, so wiring a second engine comes down to
/// declaring one more mapping — and the mapping is **displayed** in settings,
/// because a profile whose translation you cannot see is a trap.
///
/// `bypassPermissions` is reachable from none of these, by construction: there
/// is no variant for it. `dontAsk` is not offered either, though the core sets
/// it for unattended cadence sessions — at 7:30 in the morning, blocking on a
/// request nobody will read is worse than a refusal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// Plans, touches nothing.
    Plan,
    /// Asks for everything.
    Strict,
    /// The default.
    #[default]
    Normal,
    /// Acts alone wherever the registry allows it.
    Autonomous,
}

impl Profile {
    /// What the settings screen lists, in order.
    pub const ALL: [Profile; 4] = [
        Profile::Plan,
        Profile::Strict,
        Profile::Normal,
        Profile::Autonomous,
    ];

    /// The French label, because this is what the user reads.
    pub fn label(self) -> &'static str {
        match self {
            Profile::Plan => "Plan",
            Profile::Strict => "Strict",
            Profile::Normal => "Normal",
            Profile::Autonomous => "Autonome",
        }
    }
}

/// How much a request could cost if it is wrong.
///
/// Decision 6 put this on the pattern registry, which carries two columns:
/// auto-approved or not, and the risk level. The rule it exists to serve: an
/// irreversible action is never approved by voice. An unknown pattern is
/// neither auto-approved nor classed low — it is asked, at [`Risk::Medium`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Reads, and changes nothing.
    ///
    /// The `Read` *tool* never arrives here — reads of working-directory files
    /// run without asking in every mode, so a path perimeter for them is
    /// expressed in `permissions.deny` or it does not exist
    /// (`docs/cli/claude-code-cli.md` §4). But a read-only **shell**
    /// command does arrive: `Bash(cat file)` goes through the relay like any
    /// other, and this is what the registry classes it as.
    Read,
    Low,
    /// What an unrecognised pattern gets.
    Medium,
    High,
}

impl Risk {
    /// Whether this level may be approved by voice.
    ///
    /// Decision 7's guard rail, and it sits upstream on purpose: at high risk
    /// the voice channel is **not offered at all**, rather than offered and
    /// then refused. Afterwards would be too late — the first channel to answer
    /// wins, and the action may already be under way.
    pub fn voice_may_approve(self) -> bool {
        self != Risk::High
    }
}

/// A request's identity, assigned by the core.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(uuid::Uuid);

impl RequestId {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Which way an answer came in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Spoken. Never offered at [`Risk::High`].
    Voice,
    /// A gesture in a window.
    Window,
    /// The pattern registry answered without asking anybody.
    ///
    /// A channel like the others as far as the audit is concerned, and that is
    /// the point of naming it: decision 6 removes the confirmation, not the
    /// trace, so an auto-approved action has to be legible in the journal as
    /// *auto-approved* rather than indistinguishable from a human "yes".
    Registry,
}

/// What the user decided.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// This once.
    Allow,
    /// This and anything matching, within a stated scope.
    ///
    /// The scope is carried as the user was shown it, so the audit records what
    /// they actually agreed to rather than what was inferred from it later.
    AllowAlways {
        scope: String,
    },
    Deny,
}

/// A session asking to act.
///
/// # It has no expiry field, and that is the design
///
/// Decision 8, and it corrects the original specification: a pending request does not expire
/// for as long as the application runs. There is deliberately nowhere here to
/// put a deadline, so no later change can quietly introduce one. Closing the
/// orb is never a refusal either — the request keeps sitting under its session
/// in the right-hand menu.
///
/// The consequence is carried elsewhere, in the shell: a request that never
/// expires would block for ever, invisible, with no window to show it in, so it
/// **wakes the overview by itself**.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: RequestId,
    pub session: SessionId,
    /// The tool the agent reached for.
    pub tool: String,
    /// The command itself, when the tool has one.
    ///
    /// Not optional out of politeness: epic 02 requires the relay to receive
    /// *the command*, not just the tool name, from the V0 onwards. A registry
    /// that only ever sees tool names can express no pattern, so leaving this
    /// out of the V0 would foreclose the V1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// What the user is shown, in French.
    pub detail: String,
    pub risk: Risk,
    /// Milliseconds since the Unix epoch. When it was asked — not when it
    /// lapses, because it does not.
    pub asked_at: u64,
}

impl PermissionRequest {
    pub fn new(
        session: SessionId,
        tool: impl Into<String>,
        command: Option<String>,
        detail: impl Into<String>,
        risk: Risk,
    ) -> Self {
        Self {
            id: RequestId::new(),
            session,
            tool: tool.into(),
            command,
            detail: detail.into(),
            risk,
            asked_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0),
        }
    }

    /// Whether this request may be put to the voice channel at all.
    pub fn offers_voice(&self) -> bool {
        self.risk.voice_may_approve()
    }
}

/// Why an answer was not accepted.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AnswerRefusal {
    /// Unknown id — or one already answered and cleared.
    #[error("no pending request with id {0}")]
    Unknown(RequestId),
    /// Someone else got there first. Decision 7: no overwriting after the
    /// fact, because the action may already have gone out. The losing channel
    /// withdraws saying where the answer came from, which is what `by` is for.
    #[error("already answered by {by:?}")]
    AlreadyAnswered { by: Channel },
    /// The voice tried to approve something it is not offered.
    #[error("a high-risk request is not offered to the voice channel")]
    VoiceMayNotApprove,
}

/// The pending requests, and the rule that only one answer counts.
///
/// # Why a request can be *awaited*
///
/// The relay's tool call **blocks**: Claude Code calls it and waits for the
/// verdict before running anything. So filing a request has to hand back
/// something the caller can wait on, and answering it has to wake that caller.
/// Hence the one-shot channels below — one per pending request, dropped with
/// it.
///
/// A dropped sender means the waiter is gone, which happens when the session
/// died while its request was still on screen. Answering it then is not an
/// error; there is simply nobody left to tell.
#[derive(Debug, Default)]
pub struct PermissionDesk {
    pending: HashMap<RequestId, PermissionRequest>,
    /// Who is blocked on each pending request.
    waiters: HashMap<RequestId, tokio::sync::oneshot::Sender<Decision>>,
    /// Requests answered since start-up, and by whom.
    ///
    /// Kept rather than dropped, so a second channel arriving late is told
    /// *where the answer came from* instead of being told the request never
    /// existed. That difference is the whole of decision 7 as the user
    /// experiences it.
    answered: HashMap<RequestId, Channel>,
}

impl PermissionDesk {
    pub fn new() -> Self {
        Self::default()
    }

    /// File a request. It stays until answered.
    pub fn file(&mut self, request: PermissionRequest) -> RequestId {
        let id = request.id;
        self.pending.insert(id, request);
        id
    }

    /// File a request and hand back something to wait on.
    ///
    /// For the relay, whose tool call blocks until the verdict comes back.
    pub fn file_awaiting(
        &mut self,
        request: PermissionRequest,
    ) -> (RequestId, tokio::sync::oneshot::Receiver<Decision>) {
        let id = request.id;
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending.insert(id, request);
        self.waiters.insert(id, tx);
        (id, rx)
    }

    /// Everything still waiting, in no particular order.
    ///
    /// These are also what gets **put to the user again out loud** when the orb
    /// reopens. The single catch-up sentence covers what happened during the
    /// silence; pending requests are the stated exception, because they are not
    /// a stale report — they are blocking a session right now.
    pub fn pending(&self) -> Vec<PermissionRequest> {
        self.pending.values().cloned().collect()
    }

    pub fn get(&self, id: RequestId) -> Option<&PermissionRequest> {
        self.pending.get(&id)
    }

    /// Drop every request filed against a session, waking its waiters with a
    /// refusal.
    ///
    /// Called when a session ends with requests still open. Refusing rather
    /// than leaving them is the only honest answer: the thing that would have
    /// acted on an approval is gone.
    pub fn abandon_session(&mut self, session: SessionId) -> Vec<RequestId> {
        let ids: Vec<RequestId> = self
            .pending
            .values()
            .filter(|r| r.session == session)
            .map(|r| r.id)
            .collect();
        for id in &ids {
            self.pending.remove(id);
            if let Some(waiter) = self.waiters.remove(id) {
                let _ = waiter.send(Decision::Deny);
            }
        }
        ids
    }

    /// Answer a request. The first channel to answer wins.
    pub fn answer(
        &mut self,
        id: RequestId,
        decision: Decision,
        by: Channel,
    ) -> Result<(PermissionRequest, Decision), AnswerRefusal> {
        if let Some(by) = self.answered.get(&id) {
            return Err(AnswerRefusal::AlreadyAnswered { by: *by });
        }
        let request = self
            .pending
            .get(&id)
            .ok_or(AnswerRefusal::Unknown(id))?
            .clone();
        if by == Channel::Voice && !request.offers_voice() {
            return Err(AnswerRefusal::VoiceMayNotApprove);
        }
        self.pending.remove(&id);
        self.answered.insert(id, by);
        // Wake whoever is blocked on it. A closed receiver means the session
        // died while the request was still on screen — not an error, just
        // nobody left to tell.
        if let Some(waiter) = self.waiters.remove(&id) {
            let _ = waiter.send(decision.clone());
        }
        Ok((request, decision))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(risk: Risk) -> PermissionRequest {
        PermissionRequest::new(
            SessionId::new(),
            "Bash",
            Some("rm -rf build/".into()),
            "supprimer le dossier de compilation",
            risk,
        )
    }

    #[test]
    fn the_first_channel_to_answer_wins() {
        let mut desk = PermissionDesk::new();
        let id = desk.file(request(Risk::Medium));
        assert!(desk.answer(id, Decision::Allow, Channel::Voice).is_ok());
        // The second answer does not overwrite: the action may already be out.
        assert_eq!(
            desk.answer(id, Decision::Deny, Channel::Window).err(),
            Some(AnswerRefusal::AlreadyAnswered { by: Channel::Voice })
        );
    }

    #[test]
    fn a_late_channel_is_told_where_the_answer_came_from() {
        let mut desk = PermissionDesk::new();
        let id = desk.file(request(Risk::Low));
        desk.answer(id, Decision::Allow, Channel::Window).unwrap();
        // Not "no such request" — that would be a different message to show.
        match desk.answer(id, Decision::Allow, Channel::Voice) {
            Err(AnswerRefusal::AlreadyAnswered { by }) => assert_eq!(by, Channel::Window),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn high_risk_is_not_offered_to_the_voice_channel() {
        let mut desk = PermissionDesk::new();
        let high = request(Risk::High);
        assert!(!high.offers_voice());
        let id = desk.file(high);
        assert_eq!(
            desk.answer(id, Decision::Allow, Channel::Voice).err(),
            Some(AnswerRefusal::VoiceMayNotApprove)
        );
        // And it is still waiting for a gesture in the window.
        assert_eq!(desk.pending().len(), 1);
    }

    #[test]
    fn a_pending_request_outlives_being_ignored() {
        let mut desk = PermissionDesk::new();
        desk.file(request(Risk::Medium));
        // There is nowhere to put a deadline, so there is nothing to age out.
        // Closing the orb is never a refusal.
        assert_eq!(desk.pending().len(), 1);
    }

    #[test]
    fn the_registry_default_for_an_unknown_pattern_is_asked_at_medium() {
        // Decision 6: neither auto-approved nor classed low.
        assert!(Risk::Medium.voice_may_approve());
        assert!(Risk::Medium > Risk::Low);
    }
}
