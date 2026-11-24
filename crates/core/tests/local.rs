//! Epic 10 against the real thing: `llama-server`, a real model, real tools.
//!
//! `#[ignore]` because it loads several gigabytes and takes the GPU — this
//! repository's rule for every test that reads weights. The model is named by
//! two environment variables, so the test needs neither `aria-voice` nor the
//! application:
//!
//! ```powershell
//! $env:ARIA_LOCAL_ID     = "local-voxtral-mini-3b"     # or local-gemma4-e4b
//! $env:ARIA_LOCAL_MODEL  = "C:\...\Voxtral-Mini-3B-2507-Q4_K_M.gguf"
//! $env:ARIA_LOCAL_MMPROJ = "C:\...\mmproj-Voxtral-Mini-3B-2507-Q8_0.gguf"
//! cargo test -p aria-core --test local -- --ignored --nocapture
//! ```

use std::sync::Arc;
use std::time::Instant;

use aria_core::Core;
use aria_core::agent::conversation::Told;
use aria_core::agent::local::{self, LocalAgent, LocalConfig};
use aria_core::agent::session::Speech;

fn config() -> LocalConfig {
    let var = |key: &str| std::env::var(key).unwrap_or_else(|_| panic!("{key} manquant"));
    let model = local::find(&var("ARIA_LOCAL_ID")).expect("un id de modèle local connu");
    LocalConfig {
        model,
        model_path: var("ARIA_LOCAL_MODEL").into(),
        mmproj_path: var("ARIA_LOCAL_MMPROJ").into(),
        server: local::sidecar::find("").expect("llama-server introuvable"),
        port: local::PORT,
        extra_args: std::env::var("ARIA_LOCAL_ARGS")
            .map(|args| args.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default(),
        persona: String::new(),
        skills: None,
        instructions_file: None,
        transcribe_after: true,
        log_file: None,
    }
}

/// One turn: what was written, which tools were called, and how long the
/// first word took.
async fn turn(
    input: &local::Input,
    speech: &mut tokio::sync::mpsc::Receiver<Speech>,
    told: Told,
) -> (String, Vec<String>, u128) {
    let started = Instant::now();
    let mut first = None;
    let mut tools = Vec::new();
    input.say(&told).await.expect("envoyé");
    while let Some(event) = speech.recv().await {
        match event {
            Speech::Delta(_) => {
                first.get_or_insert(started.elapsed().as_millis());
            }
            Speech::ToolUse { name, input } => {
                println!("  outil : {name} {input}");
                tools.push(name);
            }
            Speech::Finished { text, usage } => {
                println!("  prompt : {} tokens", usage.input_tokens);
                return (text, tools, first.unwrap_or(0));
            }
            Speech::Failed(message) => panic!("le tour a échoué : {message}"),
            _ => {}
        }
    }
    panic!("le flux s'est fermé")
}

/// What a refusal sounds like, in the words a 4B model reaches for.
fn refuses(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "je ne peux pas",
        "je n'ai pas accès",
        "je ne suis pas en mesure",
        "je ne suis pas capable",
        "pas la possibilité",
        "désolé, mais",
    ]
    .iter()
    .any(|pattern| text.contains(pattern))
}

/// The three requests Gemma refused on 2026-11-18, the first evening at the
/// microphone. Each must now be answered — or delegated, which is the other
/// acceptable outcome — and never refused.
#[tokio::test]
#[ignore = "charge un modèle local de plusieurs gigaoctets dans llama-server"]
async fn creative_requests_are_answered_never_refused() {
    let core = Core::new();
    let mut agent = LocalAgent::start(Arc::clone(&core), config()).await.expect("chargé");
    agent.warm_up().await.expect("préchauffé");
    let (input, mut speech, _task) = agent.split();

    let mut refused = Vec::new();
    for ask in [
        "Raconte-moi une blague.",
        "Invente une blague.",
        "Raconte-moi une petite histoire.",
    ] {
        let (text, tools, first) = turn(&input, &mut speech, Told::said(ask)).await;
        println!("« {ask} »\n  → « {text} » {tools:?} — premier texte à {first} ms\n");
        if tools.is_empty() && (text.trim().is_empty() || refuses(&text)) {
            refused.push(ask);
        }
    }
    assert!(refused.is_empty(), "refusé : {refused:?}");
}

#[tokio::test]
#[ignore = "charge un modèle local de plusieurs gigaoctets dans llama-server"]
async fn a_local_model_loads_warms_and_answers() {
    let core = Core::new();
    let config = config();
    let name = config.model.display_name;

    let started = Instant::now();
    let mut agent = LocalAgent::start(Arc::clone(&core), config).await.expect("chargé");
    println!("{name} chargé en {} ms", started.elapsed().as_millis());
    let warming = Instant::now();
    agent.warm_up().await.expect("préchauffé");
    println!("préchauffé en {} ms", warming.elapsed().as_millis());

    let (input, mut speech, _task) = agent.split();

    let (text, tools, first) = turn(&input, &mut speech, Told::said("Combien font sept fois huit ?")).await;
    println!("« {text} » — premier texte à {first} ms");
    // Printed, not asserted: the plumbing is what is under test, and on
    // 2026-11-18 Voxtral answered « Quatre-vingt-dix » — the model's
    // arithmetic, faithfully delivered.
    assert!(!text.trim().is_empty());
    if !(text.contains("56") || text.contains("cinquante-six")) {
        println!("  ⚠ réponse fausse — c'est le modèle, la chaîne l'a livrée intacte");
    }
    assert!(tools.is_empty(), "{tools:?}");

    // A delegation: no engine is attached in this test, so the tool refuses —
    // what is checked is that the model reached for it, and answered after.
    let (text, tools, first) = turn(
        &input,
        &mut speech,
        Told::said("Va voir combien de lignes fait le crate core, c'est juste un chiffre."),
    )
    .await;
    println!("« {text} » — premier texte à {first} ms");
    assert!(tools.iter().any(|t| t == "delegate_task"), "{tools:?}");

    // Sound: two seconds of silence. Nothing to understand — what is checked
    // is that an audio turn goes through the server and comes back.
    let (text, _, first) = turn(&input, &mut speech, Told::heard(vec![0.0; 32_000])).await;
    println!("audio : « {text} » — premier texte à {first} ms");
}
