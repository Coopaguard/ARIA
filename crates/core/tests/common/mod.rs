//! Doubles the integration tests share.
//!
//! `allow(dead_code)`: each integration target compiles this whole module and
//! uses a part of it, so anything another target needs reads as dead here.
//!
//! Not a test target of its own — a subdirectory of `tests/` is never
//! compiled as one — and that is why it lives here rather than beside the
//! scenario that first needed it.

#![allow(dead_code)]

use aria_core::permissions::Profile;
use aria_core::providers::{
    AgentProvider, AgentSession, Availability, LaunchSpec, ModelChoice, ModelTier, NativeMode,
    ProfileSupport, ProviderEvent, Usage,
};

/// A session that answers like a real one would: some prose about what it did,
/// then the spoken line it was asked for.
///
/// The prose matters. A double that returned only the clean sentence would
/// never exercise [`aria_core::agent::delegation::spoken_from`], which is the
/// piece standing between a Markdown answer and a text-to-speech engine.
pub struct Scripted {
    pub answer: &'static str,
    pub fail: bool,
}

pub struct ScriptedSession;

#[async_trait::async_trait]
impl AgentSession for ScriptedSession {
    async fn send(&mut self, _text: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn stop(&mut self) -> anyhow::Result<()> {
        Ok(())
    }
    fn native_id(&self) -> Option<String> {
        Some("scripted-1".into())
    }
}

#[async_trait::async_trait]
impl AgentProvider for Scripted {
    fn id(&self) -> &'static str {
        "scripted"
    }
    fn display_name(&self) -> &'static str {
        "Session scriptée"
    }
    fn profile_mapping(&self) -> Vec<ProfileSupport> {
        vec![ProfileSupport {
            profile: Profile::Normal,
            native: Some(NativeMode {
                name: "default".into(),
                explanation: "ne fait rien".into(),
            }),
        }]
    }
    async fn availability(&self) -> Availability {
        Availability::Ready {
            version: "0".into(),
        }
    }
    async fn launch(
        &self,
        _spec: LaunchSpec,
    ) -> anyhow::Result<(
        Box<dyn AgentSession>,
        tokio::sync::mpsc::Receiver<ProviderEvent>,
    )> {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        let _ = tx
            .send(ProviderEvent::Started {
                native_id: "scripted-1".into(),
                tools: vec![],
                mcp_servers: vec![],
            })
            .await;
        if self.fail {
            let _ = tx
                .send(ProviderEvent::Failed {
                    message: "le processus a disparu".into(),
                })
                .await;
        } else {
            let _ = tx
                .send(ProviderEvent::Finished {
                    summary: self.answer.into(),
                    usage: Usage::default(),
                })
                .await;
        }
        Ok((Box::new(ScriptedSession), rx))
    }
}

/// What a real session's last message looks like: an explanation, Markdown and
/// all, then the line it was told to end on.
pub const WEATHER: &str = "\
J'ai consulté trois services de prévision pour Lyon.

- **Matin** : 23 °C, ciel dégagé
- **Après-midi** : 27 °C

Les trois sources concordent.
À DIRE : Il fera vingt-trois degrés ce matin et vingt-sept degrés cet après-midi.";

/// A provider that keeps the [`LaunchSpec`] it was handed, and nothing else.
///
/// Where a session **runs** is decided three layers above the process — the app
/// resolves `~/.aria/subagents`, the core holds it, `Conversation::delegate`
/// puts it in the spec — and the only place the three can be seen to agree is
/// the spec itself. Decision 108.
pub struct Capturing {
    pub seen: std::sync::Arc<std::sync::Mutex<Option<LaunchSpec>>>,
}

impl Capturing {
    pub fn new() -> Self {
        Self {
            seen: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

impl Default for Capturing {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AgentProvider for Capturing {
    fn id(&self) -> &'static str {
        "capturing"
    }
    fn display_name(&self) -> &'static str {
        "Session observée"
    }
    fn profile_mapping(&self) -> Vec<ProfileSupport> {
        vec![ProfileSupport {
            profile: Profile::Normal,
            native: Some(NativeMode {
                name: "default".into(),
                explanation: "ne fait rien".into(),
            }),
        }]
    }
    async fn availability(&self) -> Availability {
        Availability::Ready {
            version: "0".into(),
        }
    }

    /// One model per tier, so a test can see which one a tier resolved to.
    ///
    /// Named after the tier rather than after any real model: what is under
    /// test is the **mapping**, and a double that answered `claude-opus-5-5`
    /// would be testing a string table twice.
    fn models(&self) -> Vec<ModelChoice> {
        ModelTier::ALL
            .into_iter()
            .map(|tier| ModelChoice {
                id: format!("modele-{}", tier.word()),
                display_name: tier.word().into(),
                tier,
                note: String::new(),
            })
            .collect()
    }

    async fn launch(
        &self,
        spec: LaunchSpec,
    ) -> anyhow::Result<(
        Box<dyn AgentSession>,
        tokio::sync::mpsc::Receiver<ProviderEvent>,
    )> {
        *self.seen.lock().unwrap() = Some(spec);
        // Left open and silent: this double exists for the spec, and a session
        // that never finishes is what keeps the test to the one question.
        let (_tx, rx) = tokio::sync::mpsc::channel(16);
        Ok((Box::new(ScriptedSession), rx))
    }
}

/// A provider whose sessions can no longer be resumed.
///
/// Epic 05's rule has exactly one testable consequence — try the resume, and
/// handle the failure — and it needs an engine that refuses one. `--resume` on
/// an id the CLI has swept fails exactly like this, and `cleanupPeriodDays`
/// lives in the user's own settings, so the case is ordinary rather than
/// exotic.
pub struct Refusing;

#[async_trait::async_trait]
impl AgentProvider for Refusing {
    fn id(&self) -> &'static str {
        "refusing"
    }
    fn display_name(&self) -> &'static str {
        "Session périmée"
    }
    fn profile_mapping(&self) -> Vec<ProfileSupport> {
        vec![ProfileSupport {
            profile: Profile::Normal,
            native: Some(NativeMode {
                name: "default".into(),
                explanation: "ne fait rien".into(),
            }),
        }]
    }
    async fn availability(&self) -> Availability {
        Availability::Ready {
            version: "0".into(),
        }
    }

    async fn launch(
        &self,
        spec: LaunchSpec,
    ) -> anyhow::Result<(
        Box<dyn AgentSession>,
        tokio::sync::mpsc::Receiver<ProviderEvent>,
    )> {
        if spec.resume.is_some() {
            anyhow::bail!("No conversation found with session ID");
        }
        let (_tx, rx) = tokio::sync::mpsc::channel(16);
        Ok((Box::new(ScriptedSession), rx))
    }
}
