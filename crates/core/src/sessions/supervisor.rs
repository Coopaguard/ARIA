//! The pump between a provider and the core.
//!
//! A provider reports [`ProviderEvent`]s on a channel; the core holds the
//! registry and the stream. This is the one place the two meet, and keeping it
//! to one place is what lets `providers` know nothing about the broker and the
//! broker know nothing about Claude Code.
//!
//! It also holds the live session handles — the registry holds what a session
//! *is*, this holds what can be *said* to it. They are separate on purpose: the
//! registry is cloneable data the interface renders, and a handle is a process
//! nobody should be able to clone.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::Core;
use crate::providers::{AgentProvider, AgentSession, LaunchSpec, ProviderEvent};
use crate::sessions::{ActivityStep, SessionId, SessionState, StepStatus};

/// The running processes, by the core's own session id.
#[derive(Default)]
pub struct Supervisor {
    live: Mutex<HashMap<SessionId, Box<dyn AgentSession>>>,
}

impl Supervisor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a session against a provider and start pumping it.
    ///
    /// Returns as soon as the process is up — the work happens on the stream,
    /// which is the point: epic 01 asks for progress to reach the rest of the
    /// application **as it happens**, not in one lump at the end.
    pub async fn launch(
        &self,
        core: &Arc<Core>,
        provider: &dyn AgentProvider,
        title: impl Into<String>,
        spec: LaunchSpec,
    ) -> anyhow::Result<SessionId> {
        // No check before the call since 2026-11-19: the launch is attempted,
        // and a CLI that is missing or refuses fails `provider.launch` below,
        // whose error goes back to the caller.
        let profile = spec.profile;

        // The session is opened BEFORE the process, because the relay endpoint
        // is keyed on its id: the first thing the CLI may do is ask for
        // permission, and an endpoint minted after the launch would be minted
        // after the first call could arrive.
        let id = core.open_session(title, Some(profile));
        let token = core.session_routes().open(id);

        // What a launched session gets whatever asked for it, here rather than
        // at each call site because these have to agree: the relay tool must be
        // the one the MCP config publishes, and the memory post rides on the
        // same token.
        let capabilities = provider.capabilities();

        // Decision 109: an engine that cannot be asked is handed its decisions
        // in advance instead. Exactly one of these two is ever `Some` — a
        // relay *and* a frozen list would be a session judged twice, and the
        // stricter of the two silently wins.
        let (relay, policy) = if capabilities.relays_permissions {
            (Some(crate::mcp::relay_tool()), None)
        } else {
            tracing::info!(
                engine = provider.id(),
                ?profile,
                "ce moteur ne sait pas demander : politique figée au lancement"
            );
            (
                None,
                Some(crate::providers::StaticPolicy::compile(
                    &core.registry(),
                    profile,
                )),
            )
        };

        let spec = LaunchSpec {
            relay,
            policy,
            // An engine that cannot take MCP servers for one session gets
            // none: writing ARIA's own servers into the user's global
            // configuration is not something this product does behind their
            // back. The settings screen says what such a session gives up.
            mcp_servers: if capabilities.mcp_per_session {
                crate::transport::session_servers(&token)
            } else {
                Vec::new()
            },
            // Decision 106. `None` when `app/` has not resolved the file,
            // which is the state in every test and costs nothing. Passed even
            // to an engine with no flag for it: decision 108 put the session in
            // `~/.aria/subagents`, so one that discovers its instructions in
            // the working directory finds them there without being told.
            instructions_file: core.home().subagents,
            ..spec
        };

        let (session, mut events) = match provider.launch(spec).await {
            Ok(launched) => launched,
            Err(e) => {
                // Do not leave a session in the list with no process behind it,
                // nor an endpoint nobody will ever call.
                core.session_routes().close(id);
                core.fail_session(id, format!("le lancement a échoué : {e}"));
                return Err(e);
            }
        };
        self.live.lock().unwrap().insert(id, session);

        let core = Arc::clone(core);
        tokio::spawn(async move {
            // The step still running, so a tool result can close it.
            //
            // The CLI reports a tool result on a *user* turn and does not name
            // the tool it belongs to, so there is nothing to match on — the
            // pump has to remember. Local to this task, which is also the only
            // place that could know.
            let mut running: Option<ActivityStep> = None;

            while let Some(event) = events.recv().await {
                apply(&core, id, event, &mut running);
            }

            // The channel closed: the process ended without a `result`.
            // Leaving the session "executing" would be the worst of both — it
            // would never finish, and decision 8's silence clock would never
            // mark it either, because that clock is about a session that is
            // still running.
            if core
                .sessions()
                .iter()
                .any(|s| s.id == id && s.state.is_live())
            {
                core.fail_session(id, "le processus s'est terminé sans rendre de résultat");
            }
            // The endpoint goes with the process, and so do any requests still
            // open against it: whatever would have acted on an approval is
            // gone, so leaving them on screen would invite approving nothing.
            core.session_routes().close(id);
            core.abandon_permissions(id);
        });

        Ok(id)
    }

    /// Say something to a running session. One process holds several turns.
    pub async fn send(&self, id: SessionId, text: &str) -> anyhow::Result<()> {
        // The handle is taken out and put back rather than held across the
        // await: a `std::sync::Mutex` guard is not `Send`, and holding any lock
        // across an await is how a deadlock gets written by accident.
        let mut session = self
            .live
            .lock()
            .unwrap()
            .remove(&id)
            .ok_or_else(|| anyhow::anyhow!("aucune session vivante pour {id}"))?;
        let result = session.send(text).await;
        self.live.lock().unwrap().insert(id, session);
        result
    }

    /// Stop a session — the **Arrêter** button, and `STOP`.
    ///
    /// Never called on a timer: decision 8 *marks* a quiet session, it does not
    /// kill one. This is the user having asked.
    pub async fn stop(&self, core: &Arc<Core>, id: SessionId) -> anyhow::Result<()> {
        let Some(mut session) = self.live.lock().unwrap().remove(&id) else {
            return Ok(());
        };
        let result = session.stop().await;
        core.session_routes().close(id);
        core.abandon_permissions(id);
        core.set_session_state(id, SessionState::Done);
        result
    }

    /// Stop everything. `STOP` cuts the work as well as the ear.
    pub async fn stop_all(&self, core: &Arc<Core>) {
        let ids: Vec<SessionId> = self.live.lock().unwrap().keys().copied().collect();
        for id in ids {
            if let Err(e) = self.stop(core, id).await {
                tracing::warn!("session {id} did not stop cleanly: {e}");
            }
        }
    }

    /// The provider's own id for a session, which a resume needs.
    pub fn native_id(&self, id: SessionId) -> Option<String> {
        self.live
            .lock()
            .unwrap()
            .get(&id)
            .and_then(|s| s.native_id())
    }

    pub fn is_live(&self, id: SessionId) -> bool {
        self.live.lock().unwrap().contains_key(&id)
    }

    pub fn live_count(&self) -> usize {
        self.live.lock().unwrap().len()
    }
}

/// Fold one provider event into the core.
///
/// Every arm goes through a `Core` method, so every one of them publishes.
/// That is the rule the whole design rests on: nothing changes state quietly.
fn apply(
    core: &Arc<Core>,
    id: SessionId,
    event: ProviderEvent,
    running: &mut Option<ActivityStep>,
) {
    match event {
        ProviderEvent::Started {
            native_id,
            tools,
            mcp_servers,
        } => {
            tracing::info!(
                "session {id} is up as {native_id}, {} tools, mcp servers {mcp_servers:?}",
                tools.len()
            );
            // Before the state change, so a client folding events in order has
            // the resume handle by the time the session looks alive.
            core.attach_native_id(id, native_id);
            core.set_session_state(id, SessionState::Thinking);
        }

        ProviderEvent::Thinking => core.set_session_state(id, SessionState::Thinking),

        ProviderEvent::TextDelta(text) => core.push_message_delta(id, text),

        ProviderEvent::ToolUse { name, command } => {
            core.set_session_state(id, SessionState::Executing);
            let step = ActivityStep {
                label: name,
                command,
                status: StepStatus::Running,
            };
            core.push_step(id, step.clone());
            *running = Some(step);
        }

        ProviderEvent::ToolResult { ok, .. } => {
            // Re-publish the remembered step with its outcome. The client folds
            // on label plus command, so this updates the row rather than adding
            // a second one — and a result with nothing running is dropped,
            // which beats inventing a step that was never announced.
            if let Some(mut step) = running.take() {
                step.status = if ok {
                    StepStatus::Ok
                } else {
                    StepStatus::Failed
                };
                core.push_step(id, step);
            }
        }

        ProviderEvent::Finished { summary, usage } => {
            tracing::info!(
                "session {id} finished: {} in, {} out, {} read from cache",
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_input_tokens
            );
            core.finish_session(id, summary);
        }

        ProviderEvent::Failed { message } => core.fail_session(id, message),

        // Decision 36's instrument, and the plan's return condition for the
        // local tier is written against this number. Loud when it is a warning,
        // because the failure it precedes — sessions refused mid-afternoon — is
        // one nobody connects to a threshold crossed days earlier.
        //
        // Showing it on screen is a change to the event contract and belongs
        // with its own decision; until then it is in the log and not lost.
        ProviderEvent::Quota {
            utilization,
            window,
            warning,
        } => {
            if warning {
                tracing::warn!(
                    "quota {window} à {:.0} % — le palier local devient pertinent",
                    utilization * 100.0
                );
            } else {
                tracing::debug!("quota {window} à {:.0} %", utilization * 100.0);
            }
        }

        // Logged, and the session carries on. Epic 01 asks for exactly this:
        // the CLI adds event types, and a bridge that dies on one breaks on an
        // update nobody asked for.
        ProviderEvent::Unknown { raw } => {
            tracing::debug!("session {id}: unmodelled event {raw}")
        }
    }
}
