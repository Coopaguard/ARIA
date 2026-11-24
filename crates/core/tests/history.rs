//! Saving a conversation and picking it up again — epic 05, written
//! 2026-11-13.
//!
//! The pieces have their own unit tests; this is the chain. A turn has to
//! leave `agent::journal`, cross the sink, land in `store::history`, survive a
//! restart, and come back out as something the agent can be handed. A unit test
//! on any one of those passes while the chain is cut, and the symptom is
//! silent: ARIA keeps talking, and the thread is simply not there tomorrow.

mod common;

use std::sync::Arc;

use aria_core::Core;
use aria_core::agent::clock;
use aria_core::agent::conversation::{Conversation, Told};
use aria_core::agent::journal::Entry;
use aria_core::agent::tools::DelegateTask;
use aria_core::permissions::{Channel, Decision, Risk};
use aria_core::store::history::History;
use common::Capturing;

/// A core with somewhere to write and a thread open — what `app/` sets up.
fn saving_core() -> Arc<Core> {
    let core = Core::new();
    core.attach_history(History::in_memory().expect("open"));
    core.start_conversation().expect("a thread opens");
    core
}

fn task(project: Option<&str>) -> DelegateTask {
    DelegateTask {
        task: "regarder la météo".into(),
        context: None,
        model: None,
        project: project.map(str::to_string),
        follows: None,
    }
}

#[test]
fn what_is_said_and_answered_lands_in_the_thread_without_anyone_asking() {
    let core = saving_core();
    let (conversation, _channels) = Conversation::new(Arc::clone(&core));

    conversation.heard("quelle heure est-il");
    conversation.answered("il est quatorze heures");

    let id = core.current_conversation().expect("a thread");
    let thread = core
        .with_history("lecture", |history| history.thread(id))
        .expect("read");
    assert_eq!(thread.len(), 2, "le puits n'a pas eu à être appelé à la main");
    assert!(matches!(&thread[0], Entry::Said { text, .. } if text == "quelle heure est-il"));
    assert!(matches!(&thread[1], Entry::Answered { text, .. } if text == "il est quatorze heures"));
}

#[test]
fn the_title_of_the_menu_comes_from_the_first_thing_said() {
    let core = saving_core();
    let (conversation, _channels) = Conversation::new(Arc::clone(&core));
    conversation.answered("je vous écoute");
    conversation.heard("prépare-moi un projet");

    let listed = core.conversations();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].title, "prépare-moi un projet");
    assert_eq!(listed[0].turns, 2);
}

#[test]
fn a_core_with_nowhere_to_write_still_holds_a_conversation() {
    // No `attach_history`: exactly what a run whose `aria.db` would not open
    // gets, and the one thing that must not break is the talking.
    let core = Core::new();
    let (conversation, _channels) = Conversation::new(Arc::clone(&core));
    conversation.heard("bonjour");
    conversation.answered("bonjour");
    assert!(!core.history_attached());
    assert!(core.conversations().is_empty());
    assert_eq!(conversation.turns(), 2, "le fil vivant est intact");
}

#[tokio::test]
async fn a_task_with_a_project_is_kept_and_one_without_leaves_nothing() {
    let core = saving_core();
    let (conversation, _channels) = Conversation::new(Arc::clone(&core));
    let provider = Capturing::new();

    conversation
        .delegate(&provider, task(Some("le site vitrine")))
        .await
        .expect("launch");
    conversation
        .delegate(&provider, task(None))
        .await
        .expect("launch");

    let id = core.current_conversation().expect("a thread");
    let kept = core.project_sessions(id);
    assert_eq!(kept.len(), 1, "la tâche jetable n'a rien laissé derrière elle");
    assert_eq!(kept[0].project, "le site vitrine");
    assert!(!kept[0].expired);
}

#[tokio::test]
async fn a_project_session_gets_its_native_id_when_the_engine_names_it() {
    let core = saving_core();
    let (conversation, _channels) = Conversation::new(Arc::clone(&core));
    let provider = Capturing::new();

    conversation
        .delegate(&provider, task(Some("le site vitrine")))
        .await
        .expect("launch");

    let id = core.current_conversation().expect("a thread");
    let session = core.project_sessions(id)[0].id;
    // The row is written at launch, before the engine has named anything.
    assert_eq!(core.project_sessions(id)[0].native, None);

    core.attach_native_id(session, "sess-abc");

    assert_eq!(
        core.project_sessions(id)[0].native.as_deref(),
        Some("sess-abc"),
        "c'est ce que --resume prendra"
    );
}

#[tokio::test]
async fn naming_a_session_a_throwaway_one_never_writes_a_row() {
    let core = saving_core();
    let (conversation, _channels) = Conversation::new(Arc::clone(&core));
    let provider = Capturing::new();

    conversation.delegate(&provider, task(None)).await.expect("launch");
    let id = core.current_conversation().expect("a thread");
    // And the engine naming it changes nothing: `attach_native` is an update,
    // so there is no row for it to create.
    let sessions = core.sessions();
    core.attach_native_id(sessions[0].id, "sess-xyz");
    assert!(core.project_sessions(id).is_empty());
}

#[tokio::test]
async fn reopening_a_thread_hands_it_back_as_context_the_agent_must_not_speak() {
    let core = saving_core();
    let first = core.current_conversation().expect("a thread");
    {
        let (conversation, _channels) = Conversation::new(Arc::clone(&core));
        conversation.heard("prépare-moi un projet en Rust");
        conversation.answered("c'est noté");
    }

    // A second thread, as starting the application again would open.
    core.start_conversation().expect("a second thread");
    assert_ne!(core.current_conversation(), Some(first));

    // And the user clicks the first one. A fresh pilot, hence a fresh
    // conversation — which is the point: the CLI's own history goes with its
    // process.
    let thread = core.open_conversation(first).expect("reopen");
    assert_eq!(thread.len(), 2);

    let (resumed, mut channels) = Conversation::new(Arc::clone(&core));
    resumed.resume(thread).await;

    let told = channels.to_agent.try_recv().expect("the agent is told");
    assert!(
        !told.answer_is_spoken(),
        "un fil repris est du contexte, jamais une réplique à dire"
    );
    assert!(matches!(told, Told::Resumed { .. }));
    assert!(told.text().contains("prépare-moi un projet en Rust"));
    assert!(told.text().contains("c'est noté"));
    assert_eq!(core.current_conversation(), Some(first));
}

#[tokio::test]
async fn a_reopened_thread_is_not_written_down_a_second_time() {
    let core = saving_core();
    let id = core.current_conversation().expect("a thread");
    {
        let (conversation, _channels) = Conversation::new(Arc::clone(&core));
        conversation.heard("une phrase");
    }

    let thread = core.open_conversation(id).expect("reopen");
    let (resumed, _channels) = Conversation::new(Arc::clone(&core));
    resumed.resume(thread).await;

    assert_eq!(
        core.conversations()[0].turns,
        1,
        "restore ne renvoie pas le fil au magasin dont il sort"
    );

    // And what is said after the reopen does land, in the same thread.
    resumed.heard("une seconde phrase");
    assert_eq!(core.conversations()[0].turns, 2);
}

#[tokio::test]
async fn an_empty_thread_is_not_worth_a_turn() {
    let core = saving_core();
    let id = core.current_conversation().expect("a thread");
    let thread = core.open_conversation(id).expect("reopen");
    let (resumed, mut channels) = Conversation::new(Arc::clone(&core));
    resumed.resume(thread).await;
    assert!(
        channels.to_agent.try_recv().is_err(),
        "un fil vide est une conversation neuve sous un autre nom"
    );
}

#[test]
fn switching_threads_sends_the_next_turns_to_the_new_one() {
    let core = saving_core();
    let first = core.current_conversation().expect("a thread");
    let (conversation, _channels) = Conversation::new(Arc::clone(&core));
    conversation.heard("dans le premier fil");

    let second = core.start_conversation().expect("a second thread");
    conversation.heard("dans le second");

    // The sink asks the core which thread is live on *each* entry, which is
    // what makes this true without the conversation knowing anything about it.
    let threads = core.conversations();
    let by_id = |id| {
        threads
            .iter()
            .find(|c| c.id == id)
            .expect("listed")
            .turns
    };
    assert_eq!(by_id(first), 1);
    assert_eq!(by_id(second), 1);
}

#[test]
fn what_the_registry_allows_by_itself_is_still_written_down() {
    let core = saving_core();
    core.record_decision(
        None,
        "Bash",
        Some("git status".into()),
        Risk::Read,
        Decision::Allow,
        Channel::Registry,
    );
    let journal = core.audit(10);
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0].channel, Channel::Registry);
    assert_eq!(journal[0].command.as_deref(), Some("git status"));
}

#[test]
fn the_audit_outlives_the_conversations_it_came_from() {
    let core = saving_core();
    let id = core.current_conversation().expect("a thread");
    core.record_decision(
        None,
        "Bash",
        Some("rm -rf /".into()),
        Risk::High,
        Decision::Deny,
        Channel::Window,
    );
    core.forget_conversation(id);

    assert!(core.conversations().is_empty());
    assert_eq!(
        core.audit(10).len(),
        1,
        "un journal d'audit qu'on peut effacer en oubliant un fil n'en est pas un"
    );
}

#[test]
fn a_thread_untouched_for_thirty_days_is_swept_at_the_next_start() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("aria.db");
    {
        let history = History::open(&path).expect("open");
        let old = history
            .start(clock::now() - chrono::Duration::days(40))
            .expect("start");
        history
            .append(
                old,
                &Entry::Said {
                    at: clock::now() - chrono::Duration::days(40),
                    text: "il y a longtemps".into(),
                },
            )
            .expect("append");
    }

    // `attach_history` sweeps, which is also when the CLI runs its own
    // `cleanupPeriodDays` pass.
    let core = Core::new();
    core.attach_history(History::open(&path).expect("reopen"));
    assert!(core.conversations().is_empty());
}

/// Epic 05's one rule about resumability, and its only testable consequence.
#[tokio::test]
async fn a_resume_that_fails_marks_the_session_and_keeps_its_row() {
    let core = saving_core();
    let id = core.current_conversation().expect("a thread");

    // A project session from an earlier run: registered, named by the engine,
    // and no longer running — which is the only state a resume is ever
    // attempted from.
    let session = core.open_session("le site vitrine", None);
    core.remember_project_session(session, "le site vitrine");
    core.attach_native_id(session, "sess-balayee");

    // The engine has swept it since — `cleanupPeriodDays` is the user's, and
    // can be one day. Nothing about the date said so.
    core.attach_provider(Arc::new(common::Refusing));
    let refused = core.deliver_to_session(session, "et le formulaire ?").await;
    assert!(refused.is_err(), "une reprise impossible doit échouer proprement");

    let kept = core.project_sessions(id);
    assert_eq!(kept.len(), 1, "l'entrée reste dans le fil");
    assert!(kept[0].expired, "et elle est marquée plus reprenable");
    assert_eq!(kept[0].project, "le site vitrine");
}
