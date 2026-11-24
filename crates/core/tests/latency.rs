//! Epic 10's comparison, end to end: a question that needs a delegation, a
//! **real** Claude Code session doing the work, and the answer coming back.
//!
//! For each main-agent engine — the CLI, Gemma 4 E4B, Voxtral Mini 3B — it
//! measures, from the moment the question is handed over:
//!
//! - the first text the main agent writes;
//! - the `delegate_task` call;
//! - and, from the moment the delegated session returns, the main agent's
//!   reformulation: its first text and its end.
//!
//! `#[ignore]`, because it **spends subscription quota** — the delegated
//! session is real on every engine, and the CLI engine spends the main agent's
//! turns too — and because it loads two local models. Asked for by the user on
//! 2026-11-18, « payant pas de souci, à désactiver plus tard ».
//!
//! ```powershell
//! cargo test -p aria-core --test latency -- --ignored --nocapture
//! # a subset, or another question:
//! $env:ARIA_BENCH_ENGINES  = "claude-code,local-gemma4-e4b"
//! $env:ARIA_BENCH_QUESTION = "..."
//! ```
//!
//! The question goes in as **text** for every engine, through the door the
//! keyboard uses. That keeps the comparison about the agents: no ASR on one
//! side, no audio prefill on the other.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aria_core::Core;
use aria_core::agent::local::{self, LocalConfig, LocalModel};
use aria_core::agent::marks::Mark;
use aria_core::agent::pilot::{self, Pilot};
use aria_core::events::Event;
use aria_core::permissions::{Channel, Decision};
use aria_core::providers::claude_code::{self, ClaudeCode};
use aria_core::transport::{self, SessionToken};

/// How long one engine gets for the whole exchange.
const DEADLINE: Duration = Duration::from_secs(300);

/// What one run measured. Milliseconds; `None` when it never happened.
#[derive(Debug, Default)]
struct Run {
    engine: String,
    load_ms: u128,
    first_text_ms: Option<u128>,
    first_heard_ms: Option<u128>,
    delegate_ms: Option<u128>,
    session_ms: Option<u128>,
    reform_first_ms: Option<u128>,
    reform_end_ms: Option<u128>,
    first_text: String,
    spoken_result: String,
    reformulation: String,
    failure: Option<String>,
}

fn ms(since: Instant, at: Instant) -> u128 {
    at.saturating_duration_since(since).as_millis()
}

fn question() -> String {
    std::env::var("ARIA_BENCH_QUESTION").unwrap_or_else(|_| {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("Cargo.toml");
        let manifest = std::fs::canonicalize(&manifest).unwrap_or(manifest);
        // Reading a file is something the main agent cannot do on any engine:
        // its only way to an answer is a delegation. And a read never goes
        // through the permission relay, so the session cannot stall on one.
        format!(
            "Combien de membres compte le workspace déclaré dans le fichier {} ? \
             Réponds juste par le nombre et leurs noms.",
            manifest.display().to_string().trim_start_matches(r"\\?\")
        )
    })
}

fn engines() -> Vec<String> {
    std::env::var("ARIA_BENCH_ENGINES")
        .map(|list| list.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_else(|_| {
            vec![
                claude_code::ID.to_string(),
                local::GEMMA4_E4B.id.to_string(),
                local::VOXTRAL_MINI_3B.id.to_string(),
            ]
        })
}

/// A local model's file: ARIA's own models root first, then the Hugging Face
/// cache, where the POC left both models.
fn weight(model: &LocalModel, file: &str) -> Option<PathBuf> {
    let root = std::env::var_os("ARIA_MODELS").map(PathBuf::from).or_else(|| {
        std::env::var_os("LOCALAPPDATA").map(|l| PathBuf::from(l).join("aria").join("models"))
    });
    if let Some(root) = root {
        let candidate = root.join(model.dir).join(file);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let snapshots = PathBuf::from(home)
        .join(".cache")
        .join("huggingface")
        .join("hub")
        .join(format!("models--{}", model.repo.replace('/', "--")))
        .join("snapshots");
    std::fs::read_dir(snapshots)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(file))
        .find(|path| path.is_file())
}

async fn start(core: &Arc<Core>, engine: &str) -> anyhow::Result<Pilot> {
    match local::find(engine) {
        None => Pilot::start(Arc::clone(core), pilot::config(core)).await,
        Some(model) => {
            let missing = || anyhow::anyhow!("poids de {} introuvables", model.display_name);
            let config = LocalConfig {
                model,
                model_path: weight(&model, model.model_file).ok_or_else(missing)?,
                mmproj_path: weight(&model, model.mmproj_file).ok_or_else(missing)?,
                server: local::sidecar::find("")
                    .ok_or_else(|| anyhow::anyhow!("llama-server introuvable"))?,
                port: local::PORT,
                extra_args: Vec::new(),
                persona: String::new(),
                skills: None,
                instructions_file: None,
                transcribe_after: true,
                log_file: None,
            };
            Pilot::start_local(Arc::clone(core), config).await
        }
    }
}

async fn run(core: &Arc<Core>, engine: &str, question: &str) -> Run {
    let mut run = Run {
        engine: engine.to_string(),
        ..Run::default()
    };

    let loading = Instant::now();
    let pilot = match start(core, engine).await {
        Ok(pilot) => pilot,
        Err(e) => {
            run.failure = Some(format!("démarrage : {e:#}"));
            return run;
        }
    };
    run.load_ms = loading.elapsed().as_millis();

    // Subscribed before the question, so nothing is missed.
    let mut marks = core.marks();
    let mut events = core.broker().subscribe();
    let fillers: Vec<&str> = aria_core::agent::filler::phrases().to_vec();

    let asked = Instant::now();
    core.heard_turn(question);

    let mut returned_at: Option<Instant> = None;
    let deadline = tokio::time::Instant::now() + DEADLINE;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => {
                run.failure = Some(format!("pas de reformulation en {DEADLINE:?}"));
                break;
            }
            marked = marks.recv() => {
                let Ok(marked) = marked else { continue };
                match marked.mark {
                    Mark::FirstText { aloud: true } if run.first_text_ms.is_none() => {
                        run.first_text_ms = Some(ms(asked, marked.at));
                    }
                    Mark::ToolCalled { name } if name.ends_with("delegate_task") && run.delegate_ms.is_none() => {
                        run.delegate_ms = Some(ms(asked, marked.at));
                    }
                    Mark::TurnFinished { aloud: true, text } if run.first_text.is_empty() => {
                        run.first_text = text;
                        // No delegation in the turn: nothing will come back,
                        // and waiting out the deadline measures nothing.
                        if run.delegate_ms.is_none() {
                            run.failure = Some("le tour s'est fini sans délégation".into());
                            break;
                        }
                    }
                    Mark::FirstText { aloud: false } => {
                        if let Some(back) = returned_at {
                            run.reform_first_ms.get_or_insert(ms(back, marked.at));
                        }
                    }
                    Mark::TurnFinished { aloud: false, text } => {
                        if let Some(back) = returned_at {
                            run.reform_end_ms = Some(ms(back, marked.at));
                            run.reformulation = text;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            event = events.recv() => {
                let Ok(envelope) = event else { continue };
                let now = Instant::now();
                match envelope.event {
                    Event::Spoken { text } => {
                        run.first_heard_ms.get_or_insert(ms(asked, now));
                        if returned_at.is_some() && run.spoken_result.is_empty() && !fillers.contains(&text.as_str()) {
                            run.spoken_result = text;
                        }
                    }
                    Event::TaskDone { .. } => {
                        returned_at = Some(now);
                        run.session_ms = run.delegate_ms.map(|d| ms(asked, now) - d);
                    }
                    Event::TaskError { message, .. } => {
                        run.failure = Some(format!("session déléguée : {message}"));
                        break;
                    }
                    // No one is at the window here. A session that asks would
                    // wait for ever — decision 8 — so the bench says yes.
                    Event::PermissionRequest { request } => {
                        println!("  autorisation accordée d'office : {}", request.tool);
                        let _ = core.answer_permission(request.id, Decision::Allow, Channel::Window);
                    }
                    _ => {}
                }
            }
        }
    }
    drop(pilot);
    run
}

fn cell(value: Option<u128>) -> String {
    value.map(|v| format!("{v} ms")).unwrap_or_else(|| "—".into())
}

#[tokio::test]
#[ignore = "consomme du quota (une vraie session déléguée par moteur) et charge deux modèles locaux"]
async fn a_delegated_question_end_to_end_on_every_engine() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,aria_core=info".into()),
        )
        .try_init();

    let core = Core::new();
    core.attach_provider(Arc::new(ClaudeCode::new()));

    // The agent's MCP endpoint and the delegated sessions' relay both live on
    // this listener, as in the application.
    let listener =
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, transport::EVENTS_PORT))
            .unwrap_or_else(|e| {
                panic!(
                    "le port {} est pris : ARIA tourne déjà. Ferme-la puis rejoue. ({e})",
                    transport::EVENTS_PORT
                )
            });
    {
        let core = Arc::clone(&core);
        tokio::spawn(async move {
            let _ = transport::serve_events_on(listener, core, SessionToken::new()).await;
        });
    }

    let question = question();
    println!("question : « {question} »\n");

    let mut runs = Vec::new();
    for engine in engines() {
        println!("===== {engine}");
        let run = run(&core, &engine, &question).await;
        println!("  chargé en        {} ms", run.load_ms);
        println!("  premier texte    {}  « {} »", cell(run.first_text_ms), run.first_text);
        println!("  premier son      {}  (phrase d'attente comprise)", cell(run.first_heard_ms));
        println!("  délégation       {}", cell(run.delegate_ms));
        println!("  session déléguée {}", cell(run.session_ms));
        println!("  dit mot pour mot « {} »", run.spoken_result);
        println!("  reformulation    {} → {}  « {} »", cell(run.reform_first_ms), cell(run.reform_end_ms), run.reformulation);
        if let Some(failure) = &run.failure {
            println!("  ⚠ {failure}");
        }
        runs.push(run);
        // The local engine's `llama-server` is killed on drop; give the port
        // back before the next one binds it.
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    println!("\n| Moteur | Chargement | Premier texte | Délégation | Session déléguée | Retour → 1er texte reformulé | Retour → fin |");
    println!("|---|---|---|---|---|---|---|");
    for run in &runs {
        println!(
            "| {} | {} ms | {} | {} | {} | {} | {} |",
            run.engine,
            run.load_ms,
            cell(run.first_text_ms),
            cell(run.delegate_ms),
            cell(run.session_ms),
            cell(run.reform_first_ms),
            cell(run.reform_end_ms),
        );
    }

    assert!(
        runs.iter().any(|run| run.reform_end_ms.is_some()),
        "aucun moteur n'a bouclé l'échange"
    );
}
