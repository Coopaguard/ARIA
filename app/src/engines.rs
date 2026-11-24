//! Which engine each post runs on, built from configuration.
//!
//! The core declares what exists — `aria_core::providers::engine::choices` —
//! and this is where an **id out of a hand-edited TOML file** becomes a running
//! provider. It belongs in `app/` for the same reason the memory root and the
//! two `AGENTS.md` do: the core receives what it is given and never goes
//! looking.
//!
//! # Why a typo costs a setting and never the assistant
//!
//! `aria.toml` is a file a person edits. An id that matches nothing is
//! therefore an ordinary occurrence, not an exceptional one, and the only
//! acceptable outcome is the default engine plus a line in the log saying what
//! was asked for and what was used. The alternative — no provider attached — is
//! an assistant that listens, answers, and silently cannot delegate anything,
//! which is the kind of failure that gets diagnosed an evening later.
//!
//! # Two posts, two selectors
//!
//! Asked for by the user on 2026-11-12: "l'agent principal sur quelle CLI,
//! quel moteur local, quelle API" and the same question again for delegation.
//! The two are separate settings because they are separate jobs — see
//! `crate::config::Delegation` — and only the delegation one reaches a trait
//! today. The voice post is driven by `aria_core::agent::session`, which speaks
//! Claude Code; its second engine was always going to be tier B, a local model
//! behind a sidecar, and that is an `EngineKind::Local` rather than another CLI.

use std::sync::Arc;

use aria_core::providers::claude_code::ClaudeCode;
use aria_core::providers::{
    AgentProvider, EngineChoice, ModelChoice, ModelTier, Post, engine, model_for,
};

use crate::config::Delegation;

/// The provider for delegated sessions.
pub fn delegation(settings: &Delegation) -> Arc<dyn AgentProvider> {
    let id = resolve(Post::Delegation, &settings.engine);
    match id.as_str() {
        #[cfg(debug_assertions)]
        aria_core::providers::echo::ID => Arc::new(aria_core::providers::echo::Echo),
        // Claude Code, and the arm every unknown id has already been folded
        // into by `resolve`.
        _ => Arc::new(
            ClaudeCode::new()
                .with_command(settings.command.clone()),
        ),
    }
}

/// An id this build can honour, falling back to the first choice.
fn resolve(post: Post, asked: &str) -> String {
    if engine::is_known(post, asked) {
        return asked.to_string();
    }
    let fallback = engine::choices(post)
        .first()
        .map(|choice| choice.id.clone())
        .unwrap_or_else(|| aria_core::providers::claude_code::ID.to_string());
    if !asked.trim().is_empty() {
        tracing::warn!(
            demande = asked,
            utilise = %fallback,
            "moteur inconnu dans la configuration"
        );
    }
    fallback
}

/// Both dropdowns and what is selected in each, for the settings screen.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Engines {
    pub voice: PostEngines,
    pub delegation: PostEngines,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct PostEngines {
    /// The id actually in use — resolved, so a typo in the file shows the
    /// engine that is running rather than the one that is not.
    pub selected: String,
    pub choices: Vec<EngineChoice>,
    /// The models that engine offers, cheapest first.
    ///
    /// Asked of the provider, never listed here. Serves the two jobs the user
    /// named on 2026-11-12: the dropdown for the voice post\x27s own model, and —
    /// through [`PostEngines::tiers`] — showing which model each tier a
    /// delegated session may ask for actually resolves to.
    pub models: Vec<ModelChoice>,
    /// What each tier resolves to on that engine, for the screen to show.
    ///
    /// Shown rather than left implicit: the voice agent chooses `rapide`,
    /// `standard` or `expert` on its own, and the user is entitled to see what
    /// those three words will actually run before one of them runs.
    pub tiers: Vec<ResolvedTier>,
}

/// One tier, and the model it lands on.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ResolvedTier {
    pub tier: ModelTier,
    /// The word the voice agent writes.
    pub word: String,
    /// `None` on an engine that declares no models: the session then runs on
    /// that engine\x27s own default, which every delegated session did until
    /// 2026-11-12.
    pub model: Option<ModelChoice>,
}

/// A provider built only to be asked what it offers.
///
/// Cheap — two strings — and it keeps the model list in the one place that
/// declares it. The alternative, a table in `app/` or in the window, is the
/// thing decision 5 exists against.
fn ask(post: Post, id: &str, command: &str) -> Vec<ModelChoice> {
    let _ = post;
    // A local model is its own and only model: the tiers are a delegation
    // matter, and the voice post's model dropdown has nothing to offer.
    if aria_core::agent::local::find(id).is_some() {
        return Vec::new();
    }
    match id {
        #[cfg(debug_assertions)]
        aria_core::providers::echo::ID => aria_core::providers::echo::Echo.models(),
        _ => ClaudeCode::new()
            .with_command(command.to_string())
            .models(),
    }
}

fn post_engines(post: Post, id: &str, command: &str) -> PostEngines {
    let selected = resolve(post, id);
    let models = ask(post, &selected, command);
    // Only the tiers a delegated session can land on: never the fast one
    // (decision 126).
    let tiers = ModelTier::ALL
        .into_iter()
        .filter(|tier| *tier != ModelTier::Fast)
        .map(|tier| ResolvedTier {
            tier,
            word: tier.word().to_string(),
            model: model_for(&models, tier).cloned(),
        })
        .collect();
    PostEngines {
        selected,
        choices: engine::choices(post),
        models,
        tiers,
    }
}

pub fn listing(config: &crate::config::Config) -> Engines {
    Engines {
        voice: post_engines(
            Post::Voice,
            &config.agent.engine,
            &config.agent.command,
        ),
        delegation: post_engines(
            Post::Delegation,
            &config.delegation.engine,
            &config.delegation.command,
        ),
    }
}

/// The model the voice post should start on when nothing has been chosen.
///
/// The **fastest**, said in one line by the user on 2026-11-12. It holds the
/// conversation, answers every sentence and is never billed per token, so
/// fastest is the right answer there rather than a compromise.
pub fn default_voice_model(config: &crate::config::Config) -> Option<String> {
    let models = ask(
        Post::Voice,
        &resolve(Post::Voice, &config.agent.engine),
        &config.agent.command,
    );
    aria_core::providers::fastest(&models).map(|model| model.id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_known_id_is_used_as_asked() {
        assert_eq!(
            resolve(Post::Delegation, aria_core::providers::claude_code::ID),
            aria_core::providers::claude_code::ID
        );
    }

    /// The whole point of this module: `aria.toml` is hand-edited, so an id
    /// that matches nothing is ordinary. It must cost the setting and never the
    /// assistant — an unattached provider is an ARIA that answers and silently
    /// cannot delegate.
    #[test]
    fn an_unknown_id_falls_back_rather_than_leaving_no_engine() {
        assert_eq!(
            resolve(Post::Delegation, "gemini-cli"),
            aria_core::providers::claude_code::ID
        );
        assert_eq!(
            resolve(Post::Voice, "n'importe quoi"),
            aria_core::providers::claude_code::ID
        );
    }

    /// An empty id is a fresh file, not a mistake, so it is not logged as one.
    /// And it lands on the same engine a fresh file is given.
    #[test]
    fn an_empty_id_is_the_default_and_not_a_complaint() {
        assert_eq!(resolve(Post::Voice, ""), crate::config::Agent::default().engine);
    }

    /// Epic 03's exit condition at the interface: a second engine is offered
    /// for delegation, in a debug build, without this module knowing anything
    /// about it beyond its id.
    #[test]
    fn the_second_engine_is_selectable_for_delegation_in_debug() {
        let listing = listing(&crate::config::Config::default());
        // Claude Code and the two local models of epic 10.
        assert_eq!(listing.voice.choices.len(), 3);
        assert_eq!(
            listing.delegation.choices.len(),
            if cfg!(debug_assertions) { 2 } else { 1 }
        );
        // And what is selected is a real choice in both lists.
        for post in [&listing.voice, &listing.delegation] {
            assert!(
                post.choices.iter().any(|c| c.id == post.selected),
                "{post:?}"
            );
        }
    }

    /// The two posts keep their own binary. One field for both would make
    /// choosing an engine for one silently repoint the other.
    #[test]
    fn each_post_carries_its_own_binary() {
        let config = crate::config::Config::default();
        assert!(!config.agent.command.is_empty());
        assert!(!config.delegation.command.is_empty());
    }
}
