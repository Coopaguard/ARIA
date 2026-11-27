//! Who holds the main agent, and why it can be replaced.
//!
//! The pilot was managed directly in Tauri's state until 2026-11-13, which was
//! enough while it was started once and never touched again. Reopening a
//! conversation changed that, and for a reason worth stating plainly:
//!
//! **The CLI keeps its own history for the life of its process.** So a thread
//! replayed into the session that has just been holding another one leaves
//! *both* in its context, and the agent can then answer a question from the
//! conversation the user walked away from — confidently, because from where it
//! sits the two are one conversation. Switching threads therefore means a new
//! process, not a new message.
//!
//! Dropping a [`Pilot`] is exactly that: it aborts its tasks and kills the
//! subprocess with the [`Speaker`](aria_core::agent::session::Speaker). The
//! design already did the work; what was missing was somewhere to put the
//! replacement, which is what [`Running`] is.

use std::sync::{Arc, Mutex};

use aria_core::Core;
use aria_core::agent::local::{LocalConfig, LocalModel};
use aria_core::agent::pilot::{Pilot, agent_fault};
use aria_core::audio::{Ear, Load};
use aria_core::events::Part;
use tauri::{AppHandle, Manager};

/// The main agent, when there is one.
///
/// `None` before it has started, and after a start that failed — which is not
/// an error state: without a CLI, ARIA still listens, still shows its windows
/// and still runs sessions launched by hand. It just does not answer.
#[derive(Default)]
pub struct Running {
    pilot: Mutex<Option<Pilot>>,
    /// The skills line, and which engine it was read from.
    ///
    /// Kept because reading it costs seconds — `claude mcp list` health-checks
    /// every server — and a change of thread restarts the post: the user would
    /// pay that wait on every conversation they reopen. Keyed on the engine,
    /// because the delegated engine can be swapped in settings and the next
    /// start must then read the new one's connectors.
    skills: Mutex<Option<(&'static str, Option<String>)>>,
}

impl Running {
    /// Put a new agent in, and drop whatever was there.
    ///
    /// The old pilot is dropped **outside** the lock: dropping it kills a
    /// subprocess, and doing that while holding a mutex every IPC command may
    /// want is how a window freezes for no visible reason.
    fn replace(&self, pilot: Option<Pilot>) {
        let previous = std::mem::replace(&mut *self.pilot.lock().unwrap(), pilot);
        drop(previous);
    }
}

/// Longest the connectors are waited for before the post starts without them.
///
/// Measured on 2026-11-13 at a few seconds for five servers. Past this, a
/// server that hangs its health check would be holding the voice hostage.
const CONNECTORS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// What the delegated sessions can reach, as one line of the voice prompt.
///
/// Decision 117. Asked of the **delegated** engine, because that is who will
/// do the work; `None` when there is no engine, nothing connected, or no answer
/// in time — each of which costs a line, never the post.
async fn skills_line(app: &AppHandle, core: &Core) -> Option<String> {
    let provider = core.provider()?;
    let running = app.state::<Running>();
    if let Some((id, line)) = running.skills.lock().unwrap().as_ref()
        && *id == provider.id()
    {
        return line.clone();
    }
    let line = match tokio::time::timeout(CONNECTORS_TIMEOUT, provider.connectors()).await {
        Ok(connectors) => aria_core::agent::skills::prompt_line(&connectors),
        Err(_) => {
            tracing::warn!("connecteurs : pas de réponse en {CONNECTORS_TIMEOUT:?}, l'agent démarre sans");
            // Not cached: a slow start is worth retrying at the next one.
            return None;
        }
    };
    match &line {
        Some(line) => tracing::info!("compétences déléguées : {line}"),
        None => tracing::info!("aucun connecteur joignable : l'agent n'en sera pas informé"),
    }
    *running.skills.lock().unwrap() = Some((provider.id(), line.clone()));
    line
}

/// Start the agent and hand it to [`Running`].
///
/// Never fatal. The settings are read here rather than passed in, so a restart
/// picks up an engine or a model changed in the meantime without a second path
/// through which they could disagree.
pub async fn start(app: AppHandle, core: Arc<Core>) {
    let (agent, model) = {
        let settings = app.state::<Mutex<crate::config::Config>>();
        let settings = settings.lock().unwrap();
        let agent = settings.agent.clone();
        // An empty model field is not a mistake, it is "choose for me" — and
        // the answer is the fastest the engine offers. The voice post answers
        // every sentence and is never billed per token, so fastest is right
        // there rather than a compromise.
        let model = if agent.model.trim().is_empty() {
            crate::engines::default_voice_model(&settings).unwrap_or_default()
        } else {
            agent.model.clone()
        };
        (agent, model)
    };

    // Said to the voice thread first, so it starts swapping Parakeet in or out
    // while the agent loads — the two waits overlap instead of adding up.
    core.set_ear(ear_for(&agent.engine));
    let skills = skills_line(&app, &core).await;

    if let Some(local) = aria_core::agent::local::find(&agent.engine) {
        start_local(app, core, local, agent, skills).await;
        return;
    }

    let mut config =
        aria_core::agent::pilot::config_with(&core, &agent.command, &model, &agent.persona);
    config.skills = skills;
    let started = Pilot::start(Arc::clone(&core), config).await;
    installed(&app, &core, started, "agent principal indisponible");
}

/// Hand a started agent to [`Running`], or say on screen why there is none.
fn installed(app: &AppHandle, core: &Core, started: anyhow::Result<Pilot>, failed: &str) {
    match started {
        Ok(pilot) => {
            core.set_fault(Part::Agent, None);
            app.state::<Running>().replace(Some(pilot));
        }
        Err(e) => {
            tracing::error!("{failed} : {e:#}");
            core.set_fault(Part::Agent, Some(agent_fault(&format!("{e:#}"))));
            app.state::<Running>().replace(None);
        }
    }
}

/// What the ear must hand over to the engine named in the settings.
///
/// Read at start-up as well, **before** the voice thread starts, so a launch on
/// a local model never loads Parakeet only to drop it a second later.
pub fn ear_for(engine: &str) -> Ear {
    if aria_core::agent::local::find(engine).is_some() {
        Ear::Audio
    } else {
        Ear::Transcript
    }
}

/// A local model that hears, as the main agent — epic 10.
///
/// Under [`Load::Agent`] from first to last, and that is the user's rule for
/// this epic, stated on 2026-11-18: **no lazy loading**. The bar covers the
/// download if the files are missing, `llama-server` reading the model, and the
/// warm-up request. When it ends, the model answers at full speed.
async fn start_local(
    app: AppHandle,
    core: Arc<Core>,
    model: LocalModel,
    agent: crate::config::Agent,
    skills: Option<String>,
) {
    core.begin_load(Load::Agent);
    let started = std::time::Instant::now();
    let gpu = app.state::<Mutex<crate::config::Config>>().lock().unwrap().gpu;
    let loaded = load_local(Arc::clone(&core), model, &agent, gpu, skills).await;
    // The engine may have been changed again while this one loaded — a
    // download is minutes. A pilot for an engine nobody wants any more is
    // dropped here rather than installed over the newer choice.
    let still_wanted = app
        .state::<Mutex<crate::config::Config>>()
        .lock()
        .unwrap()
        .agent
        .engine
        == model.id;
    match loaded {
        Ok(_) if !still_wanted => {
            tracing::info!(
                modele = model.display_name,
                "modèle local chargé, mais le moteur a changé entre-temps : abandonné"
            );
        }
        Ok(pilot) => {
            tracing::info!(
                modele = model.display_name,
                ms = started.elapsed().as_millis(),
                "modèle local chargé et chaud"
            );
            core.set_fault(Part::Agent, None);
            app.state::<Running>().replace(Some(pilot));
        }
        Err(e) if still_wanted => {
            core.finished_load(Load::Agent);
            fall_back_to_cli(app, core, model, agent, e).await;
            return;
        }
        Err(e) => tracing::error!("agent principal local indisponible : {e:#}"),
    }
    core.finished_load(Load::Agent);
}

/// The backup: Claude Code, when the local model would not load.
///
/// The user's rule since Gemma became the default on 2026-11-18 — the CLI is
/// the backup. A missing `llama-server`, a download cut short, a GPU without
/// the memory: each would otherwise leave an ARIA that hears and never
/// answers. **The setting is left alone**: the next start tries the local
/// model again, so a fixed cause needs no trip to the settings screen.
///
/// The ear goes back to a transcript, so Parakeet reloads under its own bar
/// while the CLI starts.
async fn fall_back_to_cli(
    app: AppHandle,
    core: Arc<Core>,
    model: LocalModel,
    agent: crate::config::Agent,
    error: anyhow::Error,
) {
    tracing::error!(
        modele = model.display_name,
        "modèle local indisponible, Claude Code prend le relais : {error:#}"
    );
    core.set_ear(Ear::Transcript);
    let model_id = if agent.model.trim().is_empty() {
        let mut cli = app.state::<Mutex<crate::config::Config>>().lock().unwrap().clone();
        cli.agent.engine = aria_core::providers::claude_code::ID.into();
        crate::engines::default_voice_model(&cli).unwrap_or_default()
    } else {
        agent.model.clone()
    };
    let mut config =
        aria_core::agent::pilot::config_with(&core, &agent.command, &model_id, &agent.persona);
    config.skills = skills_line(&app, &core).await;
    let started = Pilot::start(Arc::clone(&core), config).await;
    installed(&app, &core, started, "secours Claude Code indisponible lui aussi");
}

async fn load_local(
    core: Arc<Core>,
    model: LocalModel,
    agent: &crate::config::Agent,
    gpu: bool,
    skills: Option<String>,
) -> anyhow::Result<Pilot> {
    // Blocking — `hf-hub`'s client is synchronous, and five gigabytes take
    // minutes the first time. A file already in the Hugging Face cache, where
    // the POC put both models, is linked rather than fetched again.
    let files = tokio::task::spawn_blocking(move || {
        let files = aria_voice::models::brain(
            model.repo,
            model.dir,
            [model.model_file, model.mmproj_file],
        );
        aria_voice::models::ensure_weights(files.clone(), |weight, index, total| {
            tracing::info!(
                "modèle local manquant {index}/{total} : {} depuis {}",
                weight.remote,
                weight.repo
            );
        })?;
        anyhow::Ok(files)
    })
    .await??;

    let server = aria_core::agent::local::sidecar::find(&agent.local.server).ok_or_else(|| {
        anyhow::anyhow!(
            "llama-server introuvable — `winget install ggml.llamacpp`, ou son chemin dans \
             les réglages (agent.local.server)"
        )
    })?;
    // Beside ARIA's own log, in `~/.aria/logs` (see `crate::logs`). Not swept
    // by its seven days: `llama-server` rewrites this one file on each start.
    let logs = crate::logs::dir().unwrap_or_else(|| aria_voice::local_root().join("logs"));
    let _ = std::fs::create_dir_all(&logs);

    let config = LocalConfig {
        model,
        model_path: files[0].local.clone(),
        mmproj_path: files[1].local.clone(),
        server,
        port: aria_core::agent::local::PORT,
        extra_args: agent.local.server_args(gpu),
        persona: agent.persona.clone(),
        skills,
        instructions_file: core.home().main,
        transcribe_after: !agent.local.no_transcript,
        log_file: Some(logs.join("llama-server.log")),
    };
    Pilot::start_local(core, config).await
}

/// Stop the agent, then start a fresh one.
///
/// What reopening a conversation does. The old one goes first and on purpose:
/// two live sessions would both be attached to the core for as long as the
/// second took to warm up, and `Core::attach_conversation` holds exactly one —
/// so a `delegate_task` arriving in that window would land in whichever had
/// attached last, which is not a thing to leave to timing.
pub async fn restart(app: AppHandle, core: Arc<Core>) {
    app.state::<Running>().replace(None);
    start(app, core).await;
}
