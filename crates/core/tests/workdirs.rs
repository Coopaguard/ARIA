//! Where each agent runs (decision 108).
//!
//! Asked for by the user on 2026-11-12: the main agent starts in `~/.aria`, and
//! a delegated session in `~/.aria/subagents` unless a task names somewhere
//! better. Both were somewhere nobody chose — the post under `%TEMP%`, and a
//! session wherever the application happened to be launched from, which in
//! development is this repository.
//!
//! Tested here rather than beside the code because the value crosses three
//! layers that each know a different half: `app/` resolves `~`, the core holds
//! it, and `Conversation::delegate` puts it in the `LaunchSpec`. A unit test on
//! any one of them passes while the chain is broken.

mod common;

use std::sync::Arc;

use aria_core::Core;
use aria_core::agent::instructions::Home;
use aria_core::agent::session::{Config, claude_md_above};
use aria_core::agent::tools::DelegateTask;
use aria_core::agent::{conversation::Conversation, pilot};
use common::Capturing;

/// A `~/.aria` and its `subagents`, under a temporary directory.
fn home(dir: &std::path::Path) -> Home {
    let aria = dir.join(".aria");
    let subagents = aria.join("subagents");
    std::fs::create_dir_all(&subagents).expect("mkdir");
    Home {
        dir: Some(aria),
        subagents_dir: Some(subagents),
        ..Home::default()
    }
}

#[test]
fn the_main_agent_runs_in_the_folder_the_user_owns() {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = Core::new();

    // Before `app/` has looked, the throwaway directory — because a home that
    // cannot be resolved must cost a path and not the assistant.
    assert_eq!(pilot::config(&core).cwd, Config::fallback_cwd());

    core.set_home(home(dir.path()));
    assert_eq!(pilot::config(&core).cwd, dir.path().join(".aria"));
}

#[tokio::test]
async fn a_delegated_session_lands_in_the_subagents_folder() {
    let dir = tempfile::tempdir().expect("tempdir");
    let core = Core::new();
    core.set_home(home(dir.path()));

    let (conversation, _channels) = Conversation::new(Arc::clone(&core));
    let provider = Capturing::new();
    let seen = Arc::clone(&provider.seen);

    conversation
        .delegate(
            &provider,
            DelegateTask {
                task: "chercher la météo".into(),
                context: None,
                model: None,
                project: None,
                follows: None,
            },
        )
        .await
        .expect("the session launches");

    let spec = seen.lock().unwrap().clone().expect("a spec was handed over");
    assert_eq!(
        spec.cwd,
        Some(dir.path().join(".aria").join("subagents")),
        "une session déléguée sans dossier nommé atterrit dans le dossier par défaut"
    );
}

/// `None` is not "nowhere" — it is "wherever the application was launched
/// from". That was the old behaviour, and it is worth an assertion that the
/// floor only disappears when there is genuinely no home.
#[tokio::test]
async fn with_no_home_resolved_a_session_names_no_folder_at_all() {
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
                model: None,
                project: None,
                follows: None,
            },
        )
        .await
        .expect("the session launches");

    assert_eq!(seen.lock().unwrap().clone().unwrap().cwd, None);
}

/// The risk the move introduced, and the reason the check walks up.
///
/// `%TEMP%/aria-agent` had parents that hold nothing of ours. `~/.aria` has the
/// user's home for a parent, where a `CLAUDE.md` is an ordinary thing to find —
/// and the CLI walks up to it. The symptom would be a voice assistant answering
/// about a repository, with nothing pointing at the cause.
#[test]
fn a_claude_md_above_the_folder_is_found_and_not_only_one_inside_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let aria = dir.path().join(".aria");
    std::fs::create_dir_all(&aria).expect("mkdir");

    assert_eq!(claude_md_above(&aria), None);

    // One in the parent, which is exactly the new case.
    let above = dir.path().join("CLAUDE.md");
    std::fs::write(&above, "# un dépôt").expect("write");
    assert_eq!(claude_md_above(&aria), Some(above));

    // And one in the folder itself still wins, being nearer.
    let inside = aria.join("CLAUDE.md");
    std::fs::write(&inside, "# ici").expect("write");
    assert_eq!(claude_md_above(&aria), Some(inside));
}

/// A directory of that name is not a `CLAUDE.md`, and refusing to start over
/// one would be a launch failure nobody could diagnose.
#[test]
fn a_directory_called_claude_md_is_not_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join("CLAUDE.md")).expect("mkdir");
    assert_eq!(claude_md_above(dir.path()), None);
}
