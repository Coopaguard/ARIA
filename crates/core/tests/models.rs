//! Which model does the work (decision 110).
//!
//! Asked for by the user on 2026-11-12, in one sentence and with its own
//! example: with Haiku holding the conversation, Haiku again for a simple
//! request such as the weather, Sonnet for the rest, Opus for code.
//!
//! Until then a delegated session carried **no model at all** and ran on
//! whatever the engine happened to default to — a weather lookup and a day of
//! coding treated identically, which is both slower and dearer than either
//! deserves.
//!
//! # Why the test lives here
//!
//! The value crosses three pieces that each know a different half: the voice
//! agent writes a *word*, `Conversation::delegate` resolves it against the
//! provider's list, and the provider spells the flag. A unit test on any one of
//! them passes while the chain is broken — which is exactly how a tier would
//! silently become the engine's default.

mod common;

use std::sync::Arc;

use aria_core::Core;
use aria_core::agent::conversation::Conversation;
use aria_core::agent::tools::DelegateTask;
use aria_core::providers::ModelTier;
use common::Capturing;

/// Delegate one task and read the `LaunchSpec` the provider was handed.
async fn delegated(model: Option<&str>) -> aria_core::providers::LaunchSpec {
    let core = Core::new();
    let (conversation, _channels) = Conversation::new(Arc::clone(&core));
    let provider = Capturing::new();
    let seen = Arc::clone(&provider.seen);

    conversation
        .delegate(
            &provider,
            DelegateTask {
                task: "chercher la météo".into(),
                context: None,
                model: model.map(str::to_string),
                project: None,
                follows: None,
            },
        )
        .await
        .expect("the session launches");

    seen.lock().unwrap().clone().expect("a spec was handed over")
}

/// The whole of the request, asserted on the three words the model writes.
///
/// Decision 126: a delegated task runs at `standard` at least, so `rapide`
/// written out of habit lands on the standard model. Only the voice post keeps
/// the fastest one.
#[tokio::test]
async fn each_tier_the_model_asks_for_reaches_the_engine_with_standard_as_floor() {
    for tier in ModelTier::ALL {
        let spec = delegated(Some(tier.word())).await;
        let expected = match tier {
            ModelTier::Expert => "modele-expert",
            _ => "modele-standard",
        };
        assert_eq!(
            spec.model.as_deref(),
            Some(expected),
            "le palier « {} » n'a pas atteint le bon modèle",
            tier.word()
        );
    }
}

/// A dispatcher that does not think about it gets standard, which is the right
/// default for ordinary work.
#[tokio::test]
async fn no_tier_at_all_is_standard() {
    let spec = delegated(None).await;
    assert_eq!(spec.model.as_deref(), Some("modele-standard"));
}

/// The value is written by a language model. A misspelling must cost nothing —
/// failing the call would lose the user their answer in order to save a token.
#[tokio::test]
async fn a_tier_nobody_recognises_still_runs_the_task() {
    let spec = delegated(Some("costaud")).await;
    assert_eq!(spec.model.as_deref(), Some("modele-standard"));
    assert_eq!(spec.prompt.contains("chercher la météo"), true);
}

/// And the flag Claude Code builds from it, in full.
///
/// `docs/cli/claude-code-cli.md` §8 measured `--model haiku` resolving to
/// `claude-haiku-4-5-20251001` — a generation behind what was asked for, and
/// silently. So what travels is the full identifier, and this is the assertion
/// that would notice an alias creeping back in.
#[test]
fn the_engine_is_handed_the_full_model_name() {
    use aria_core::providers::AgentProvider;

    let claude = aria_core::providers::claude_code::ClaudeCode::new();
    let models = claude.models();
    assert!(!models.is_empty(), "Claude Code déclare ses modèles");

    for tier in ModelTier::ALL {
        let model = aria_core::providers::model_for(&models, tier)
            .unwrap_or_else(|| panic!("un modèle pour « {} »", tier.word()));
        // Full names carry their family and their generation; an alias is a
        // bare word.
        assert!(
            model.id.starts_with("claude-") && model.id.matches('-').count() >= 3,
            "« {} » ressemble à un alias",
            model.id
        );
        assert!(!model.note.is_empty(), "chaque modèle s'explique à l'écran");
    }

    // The fastest is what the voice post gets by default: it answers every
    // sentence and is never billed per token.
    let fastest = aria_core::providers::fastest(&models).expect("un modèle le plus rapide");
    assert_eq!(fastest.tier, ModelTier::Fast);
    assert_eq!(fastest.id, aria_core::agent::session::MODEL);
}
