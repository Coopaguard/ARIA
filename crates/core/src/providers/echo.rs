//! The second provider, and epic 03's exit condition made executable.
//!
//! Epic 03 does not ask for an abstraction; it asks for a **proof**:
//!
//! > a second, fake provider — even one that only echoes, even one that can
//! > honour two profiles out of four — plugs in **without touching the
//! > interface code**, and the interface says which profiles are missing.
//!
//! This is that provider. It answers by repeating what it was asked, which is
//! useless in itself and exactly the point: nothing here is about the quality
//! of an answer, everything is about whether the seams hold. What it proves,
//! item by item:
//!
//! - **two profiles out of four.** `Plan` and `Autonomous` map to something;
//!   `Strict` and `Normal` come back `native: None`, and the settings screen has
//!   to say so rather than offer a setting with no effect (decision 5);
//! - **an engine that cannot be asked.** `relays_permissions: false`, so the
//!   supervisor compiles a [`StaticPolicy`] instead of handing it a relay — the
//!   mechanism the user chose on 2026-11-12, and the one three of the four real
//!   CLIs would need. Without a consumer it would be dead code, and a
//!   permission mechanism nobody exercises is a permission mechanism that does
//!   not work;
//! - **an engine with no MCP of its own.** `mcp_per_session: false`, so it is
//!   handed no servers at all and a session there has neither tiles nor memory.
//!
//! # Why it ships, and only in debug
//!
//! Because a provider that exists only inside `#[cfg(test)]` proves the trait
//! compiles, not that the **interface** holds — and the interface is what epic
//! 03 is about. In a debug build it appears in the engine dropdown, so the
//! second entry is something that can actually be selected and watched. It is
//! absent from a release build: an engine that repeats your words is not a
//! product feature, and the precedent is the scenarios tab, debug-only for the
//! same reason.

use tokio::sync::mpsc;

use super::{
    AgentProvider, AgentSession, Availability, Capabilities, LaunchSpec, McpToolNaming, NativeMode,
    ProfileSupport, ProviderEvent, Usage,
};
use crate::permissions::Profile;

pub const ID: &str = "echo";

/// A provider that repeats, and declares rather less than Claude Code.
#[derive(Debug, Default)]
pub struct Echo;

struct EchoSession {
    id: String,
    events: mpsc::Sender<ProviderEvent>,
}

#[async_trait::async_trait]
impl AgentSession for EchoSession {
    async fn send(&mut self, text: &str) -> anyhow::Result<()> {
        answer(&self.events, text).await;
        Ok(())
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    fn native_id(&self) -> Option<String> {
        Some(self.id.clone())
    }
}

/// Repeat the prompt, then close the turn.
///
/// The marked last line is there because the whole return path depends on it:
/// `agent::delegation::spoken_from` takes it and the core pushes it straight to
/// the voice. An echo that did not produce one would exercise the *fallback*
/// instead of the path, which is the less interesting half.
async fn answer(events: &mpsc::Sender<ProviderEvent>, text: &str) {
    let said = text.trim();
    let _ = events
        .send(ProviderEvent::TextDelta(format!("J'ai entendu : {said}")))
        .await;
    let _ = events
        .send(ProviderEvent::Finished {
            summary: format!(
                "J'ai entendu : {said}\n\n{} Je ne sais que répéter : {said}",
                crate::agent::delegation::SPOKEN_MARKER
            ),
            usage: Usage::default(),
        })
        .await;
}

#[async_trait::async_trait]
impl AgentProvider for Echo {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "Écho (démonstration)"
    }

    /// Two of four, and the holes are the point.
    ///
    /// `Strict` and `Normal` both mean "ask me", and an engine that cannot ask
    /// has no equivalent for either. Saying `None` is what lets the settings
    /// screen tell the user instead of offering a dead setting.
    fn profile_mapping(&self) -> Vec<ProfileSupport> {
        vec![
            ProfileSupport {
                profile: Profile::Plan,
                native: Some(NativeMode {
                    name: "plan".into(),
                    explanation: "répète la demande sans rien faire".into(),
                }),
            },
            ProfileSupport {
                profile: Profile::Strict,
                native: None,
            },
            ProfileSupport {
                profile: Profile::Normal,
                native: None,
            },
            ProfileSupport {
                profile: Profile::Autonomous,
                native: Some(NativeMode {
                    name: "auto".into(),
                    explanation: "répète la demande, et rien d'autre".into(),
                }),
            },
        ]
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            relays_permissions: false,
            mcp_per_session: false,
            instructions_flag: false,
            discovers_instructions_in_cwd: false,
            resumes: false,
            naming: McpToolNaming::DoubleUnderscore,
        }
    }

    /// Always ready. There is no binary to find.
    async fn availability(&self) -> Availability {
        Availability::Ready {
            version: "démonstration".into(),
        }
    }

    async fn launch(
        &self,
        spec: LaunchSpec,
    ) -> anyhow::Result<(
        Box<dyn AgentSession>,
        mpsc::Receiver<ProviderEvent>,
    )> {
        // Said out loud in the log, because this is the whole of decision 109
        // happening: no relay went out, a frozen list came in instead, and the
        // settings screen is telling the user the same thing in French.
        match &spec.policy {
            Some(policy) => tracing::info!(
                permis = policy.allow.len(),
                refuses = policy.deny.len(),
                inconnu_permis = policy.allow_unknown,
                "écho lancé sous une politique figée"
            ),
            None => tracing::warn!(
                "écho lancé sans politique : le superviseur a cru que ce moteur savait demander"
            ),
        }

        let (tx, rx) = mpsc::channel(16);
        let _ = tx
            .send(ProviderEvent::Started {
                native_id: "echo-1".into(),
                tools: Vec::new(),
                mcp_servers: spec.mcp_servers.iter().map(|s| s.name.clone()).collect(),
            })
            .await;
        if !spec.prompt.trim().is_empty() {
            answer(&tx, &spec.prompt).await;
        }
        Ok((
            Box::new(EchoSession {
                id: "echo-1".into(),
                events: tx,
            }),
            rx,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Epic 03's exit condition, asserted rather than claimed: a second
    /// provider declares holes, and the hole is *readable*.
    #[test]
    fn a_second_provider_may_honour_two_profiles_out_of_four() {
        let mapping = Echo.profile_mapping();
        assert_eq!(mapping.len(), 4, "every profile has a row, hole or not");
        assert!(super::super::native_for(&mapping, Profile::Plan).is_some());
        assert!(super::super::native_for(&mapping, Profile::Autonomous).is_some());
        // The two that mean "ask me", on an engine that cannot ask.
        assert!(super::super::native_for(&mapping, Profile::Strict).is_none());
        assert!(super::super::native_for(&mapping, Profile::Normal).is_none());
    }

    #[test]
    fn it_says_what_it_cannot_do() {
        let said = Echo.capabilities().shortcomings();
        assert!(
            said.iter().any(|line| line.contains("demander")),
            "{said:?}"
        );
        assert!(said.iter().any(|line| line.contains("tuiles")), "{said:?}");
    }

    /// The marked line, because the whole return path hangs off it: the core
    /// pushes it straight to the voice without regenerating it.
    #[tokio::test]
    async fn it_answers_on_the_line_the_core_reads() {
        let (session, mut events) = Echo
            .launch(LaunchSpec {
                prompt: "dis bonjour".into(),
                ..LaunchSpec::default()
            })
            .await
            .expect("l'écho se lance");
        assert_eq!(session.native_id().as_deref(), Some("echo-1"));

        let mut summary = None;
        while let Ok(event) = events.try_recv() {
            if let ProviderEvent::Finished { summary: said, .. } = event {
                summary = Some(said);
            }
        }
        let summary = summary.expect("un tour fini");
        assert_eq!(
            crate::agent::delegation::spoken_from(&summary),
            "Je ne sais que répéter : dis bonjour"
        );
    }
}
