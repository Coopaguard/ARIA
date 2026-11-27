//! What the front end is allowed to call, and the events it receives back.
//!
//! The only door between the window and the core. The window reaches the
//! system through nothing else, because this process is the single place
//! permissions are enforced.
//!
//! # The split, and why it is not arbitrary
//!
//! Commands go **through here**; events come back **over the WebSocket** the
//! core serves ([`aria_core::transport`]). That is the division
//! The original specification sets out — WebSocket *for the events* — and
//! each half gets something the other cannot give:
//!
//! - a command over IPC is scoped by `capabilities/default.json`, so the list
//!   of what a window may invoke is a file someone can read;
//! - an event stream from the core carries a sequence number and survives the
//!   window, which is what lets a reconnecting client replay what it missed
//!   instead of starting from a blank screen.
//!
//! Every command here is a thin wrapper. The rule it exists to protect: the
//! core publishes the event for every mutation, so no state ever changes
//! without the stream saying so. A command that mutated something itself would
//! put a hole in the activity feed that nobody can see.

pub mod hooks;

use std::sync::{Arc, Mutex};

use aria_core::audio::{AudioMode, Load, ModeTrigger};
use aria_core::permissions::{Channel, Decision, Profile, RequestId};
use aria_core::permissions::{PermissionRequest, Risk};
use aria_core::providers::LaunchSpec;
use aria_core::agent::journal::Entry;
use aria_core::sessions::{ActivityStep, SessionId, SessionState, StepStatus};
use aria_core::store::history::audit::Decided;
use aria_core::store::history::{Conversation, ConversationId, ProjectSession};
use aria_core::{Core, ProviderInfo, Snapshot};
use serde::Serialize;
use tauri::{AppHandle, State};

use crate::config::Config;
use crate::shell;

/// How the window opens the event stream.
///
/// The token is handed over here and nowhere else: not in a file, not on a
/// command line, not in the page source. Loopback keeps the network out but
/// not other local processes, and the tile sandbox is a local process by
/// design — see [`aria_core::transport`].
#[derive(Clone, Debug, Serialize)]
pub struct Connection {
    pub port: u16,
    pub token: String,
}

/// The core and the token, as Tauri's managed state.
pub struct Ipc {
    pub core: Arc<Core>,
    pub token: aria_core::transport::SessionToken,
}

#[tauri::command]
pub fn connection(ipc: State<'_, Ipc>) -> Connection {
    Connection {
        port: aria_core::transport::EVENTS_PORT,
        token: ipc.token.as_str().to_string(),
    }
}

/// Where we are, so the window can draw itself from nothing.
#[tauri::command]
pub fn snapshot(ipc: State<'_, Ipc>) -> Snapshot {
    ipc.core.snapshot()
}

/// The orb was clicked: open or close the voice channel.
///
/// Epic 04 is precise about the scope of this, and the distinction matters: the
/// orb is a **microphone switch**, not a shutdown of the audio chain. The models
/// stay loaded so resuming is immediate, and **Claude sessions run to
/// completion** — cutting the ear must never interrupt a task. With the orb
/// closed nothing is spoken either; the result still arrives on screen, it just
/// is not said out loud.
#[tauri::command]
pub fn toggle_audio(ipc: State<'_, Ipc>) -> Result<AudioMode, String> {
    ipc.core
        .apply_audio(ModeTrigger::Interface)
        .map(|change| change.to)
        // The refusal is passed on rather than swallowed: "no voice command
        // leads to off" is a sentence the interface should be able to show.
        .map_err(|e| e.to_string())
}

// There is no `set_default_profile` command. The profile is a stored setting,
// not an action, so it travels with the rest of them through `set_config` —
// otherwise the status strip and the settings screen each keep their own idea
// of it, and the next start-up reads the file and overrules both.

// ----------------------------------------------------------------- the shell
//
// Three states, and the two buttons on the overview that move between them.
// The shortcut does the rest, and it is a toggle: there is no main → overview
// edge, which epic 04 takes on knowingly.

/// The left wedge of the overview: open the full window.
#[tauri::command]
pub fn show_main(app: AppHandle) {
    shell::show_main(&app);
}

/// The right wedge of the overview: go to sleep.
///
/// Hides the windows and, if the setting says so, unloads the audio models.
/// Nothing else: the core and the sessions keep running, because cutting the
/// ear must never interrupt a task.
#[tauri::command]
pub fn sleep(app: AppHandle) {
    shell::sleep(&app);
}

/// Which of the three states we are in, for a window drawing itself.
#[tauri::command]
pub fn shell_state(app: AppHandle) -> &'static str {
    use tauri::Manager;
    match app.state::<Arc<shell::Shell>>().state() {
        shell::ShellState::Sleeping => "sleeping",
        shell::ShellState::Overview => "overview",
        shell::ShellState::Main => "main",
    }
}

// --------------------------------------------------------------- the settings

/// The settings, as the screen shows them.
#[tauri::command]
pub fn get_config(config: State<'_, Mutex<Config>>) -> Config {
    config.lock().unwrap().clone()
}

/// The graphics card found, for the switch that computes on it: its maker,
/// its name, the maker's stack and what actually runs.
#[tauri::command]
pub fn gpu_card() -> GpuCard {
    let card = aria_voice::gpu::card().clone();
    let installed = card.native.is_some_and(aria_voice::runtime::installed);
    GpuCard {
        card,
        loaded: aria_voice::runtime::stack(),
        installed,
    }
}

/// The card, and where its own runtime stands (decision 131): downloaded or
/// not, and which runtime this process actually loaded — they differ until
/// the next start after a download.
#[derive(serde::Serialize)]
pub struct GpuCard {
    #[serde(flatten)]
    card: aria_voice::gpu::Card,
    loaded: aria_voice::gpu::Stack,
    installed: bool,
}

/// Apply and persist the settings.
///
/// Three things happen in order, and the order is the point: the shortcut is
/// **registered first**, and a failure stops everything. Writing a shortcut
/// that could not be registered would leave the screen showing a combination
/// that does nothing, and the next start-up would read it back and fail the
/// same way, silently.
#[tauri::command]
pub fn set_config(
    app: AppHandle,
    ipc: State<'_, Ipc>,
    current: State<'_, Mutex<Config>>,
    voice: State<'_, crate::voice::speaking::Retuner>,
    config: Config,
) -> Result<(), String> {
    let previous = current.lock().unwrap().clone();

    // The voice takes effect at the next sentence (decision 123). It used to
    // wait for the next start-up, and « écouter » playing the new voice while
    // ARIA went on in the old one read as the setting doing nothing.
    if config.voice != previous.voice {
        voice.retune(config.voice.clone());
    }

    if config.global_shortcut != previous.global_shortcut {
        shell::register_shortcut(&app, &config.global_shortcut).map_err(|e| e.to_string())?;
    }

    // The profile is the core's business; everything else is the shell's.
    if config.profile != previous.profile {
        ipc.core.set_default_profile(config.profile);
    }

    // The delegation engine takes effect **now**, and that asymmetry with the
    // voice post is deliberate rather than an oversight. A provider is resolved
    // per call — `mcp::voice::delegate` looks it up each time, and
    // `Core::delegation_naming` asks it per permission decision — precisely so
    // a conversation can outlive the swap. The voice post cannot do that: its
    // process is already running with its model, its prompt and its tools, so
    // its engine is read at start-up and the screen says so on the row itself.
    if config.delegation != previous.delegation {
        tracing::info!(
            moteur = %config.delegation.engine,
            "moteur des tâches déléguées remplacé"
        );
        ipc.core
            .attach_provider(crate::engines::delegation(&config.delegation));
    }

    // The main agent's engine takes effect **now** too, since epic 10. It used
    // to wait for the next launch, which was bearable while there was one
    // engine; comparing a local model with the CLI means switching back and
    // forth, and a restart per comparison is not a comparison. Dropping the
    // pilot kills its process (and frees the GPU) before the new one loads,
    // and the voice thread swaps Parakeet in or out on its own — see
    // `crate::pilot::start`.
    let local = aria_core::agent::local::find(&config.agent.engine).is_some();
    let restart_agent = config.agent.engine != previous.agent.engine
        || (local && (config.agent.local != previous.agent.local || config.gpu != previous.gpu));
    // The ear follows on its own: the voice thread sees Parakeet on the wrong
    // side of the switch and reloads it (`Engine::misplaced`).
    aria_voice::asr::set_gpu(aria_voice::gpu::wanted(config.gpu));

    let path = crate::config::Config::path(&app).map_err(|e| e.to_string())?;
    if let Err(e) = config.save(&path) {
        // The settings are live even if the file could not be written. Say so
        // rather than roll back: reverting a change the user just made and
        // watched take effect is more confusing than telling them it will not
        // survive a restart.
        return Err(format!(
            "réglages appliqués, mais non enregistrés dans {} : {e}",
            path.display()
        ));
    }
    *current.lock().unwrap() = config;
    // After the settings are stored: `pilot::start` reads them back.
    if restart_agent {
        tracing::info!("moteur de l'agent principal remplacé : rechargement");
        let core = Arc::clone(&ipc.core);
        tauri::async_runtime::spawn(crate::pilot::restart(app, core));
    }
    Ok(())
}

// There is deliberately no "test this shortcut" command. Testing one means
// registering it, and registering it means unregistering the current one — so a
// user who tried a combination and then cancelled would walk away with a
// shortcut they never confirmed. Saving is the only path, and it reports
// "already taken" there.

/// Answer a pending request from the window.
///
/// Always [`Channel::Window`]: this is the door the window speaks through, so
/// it cannot claim to be the voice. The first channel to answer wins, and a
/// refusal here means the voice got there first — the window withdraws its
/// invitation saying so, rather than overwriting a decision whose action may
/// already be under way.
#[tauri::command]
pub fn answer_permission(
    ipc: State<'_, Ipc>,
    id: RequestId,
    decision: Decision,
) -> Result<(), String> {
    ipc.core
        .answer_permission(id, decision, Channel::Window)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// STOP: stop listening **and** the running sessions.
///
/// The other half of epic 04's pair, and the distinction is the whole point:
/// the orb cuts the ear, STOP cuts the work. Two commands, two scopes, and
/// confusing them is how a user loses a task they only meant to stop listening
/// to.
///
/// Killing the sessions needs the bridge, which lands in T5. Until then this
/// cuts the ear and marks the sessions, which is the honest half rather than a
/// button that pretends.
#[tauri::command]
pub async fn stop_all(ipc: State<'_, Ipc>) -> Result<(), String> {
    // Off, not hybrid: this is a hand on the interface, which is the only thing
    // that may reach off.
    if ipc.core.audio_mode() != AudioMode::Off {
        let _ = ipc.core.apply_audio(ModeTrigger::Interface);
    }
    // The voice and the answer being written, too: STOP that leaves ARIA
    // finishing her sentence is not a stop (2026-11-23).
    ipc.core.interrupt_agent();
    // Real processes now, not a mark on a list. Each one gets its stdin closed
    // so it writes its session state and stays resumable; a process that
    // ignores that is killed.
    ipc.core.stop_all_sessions().await;
    Ok(())
}

// ------------------------------------------------------------------ the engine

/// What the settings screen shows about the active engine.
///
/// The profile table is **asked of the provider**, never written here. That is
/// the whole of decision 5, and it is what makes a second engine a mapping to
/// declare rather than interface code to change.
#[tauri::command]
pub async fn provider_info(ipc: State<'_, Ipc>) -> Result<Option<ProviderInfo>, String> {
    Ok(ipc.core.provider_info().await)
}

/// What each post may run on, and what it is running on.
///
/// Two lists, because the user asked for two selectors on 2026-11-12 — one for
/// the agent that speaks, one for the sessions it delegates to. They are
/// separate settings because they are separate jobs; see
/// `crate::config::Delegation`.
///
/// `selected` is the **resolved** id, not the one in the file: `aria.toml` is
/// hand-edited, and a screen showing an engine that is not the one running is
/// worse than no screen at all.
#[tauri::command]
pub fn engines(config: State<'_, Mutex<Config>>) -> crate::engines::Engines {
    crate::engines::listing(&config.lock().unwrap())
}

/// Delegate a task to the engine.
///
/// The profile is the global default unless overridden: the main agent opens
/// sessions of very different natures — a throwaway weather lookup and a day of
/// coding — and forcing them to the same guard rail makes no sense.
#[tauri::command]
pub async fn launch_session(
    ipc: State<'_, Ipc>,
    prompt: String,
    profile: Option<Profile>,
) -> Result<SessionId, String> {
    let title = prompt.chars().take(60).collect::<String>();
    ipc.core
        .launch_session(
            title,
            LaunchSpec {
                prompt,
                profile: profile.unwrap_or_else(|| ipc.core.default_profile()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| e.to_string())
}

/// Measure what this microphone does when nobody is speaking.
///
/// The first tier of decision 38, and the half that could only be done from a
/// command line until now. It runs through the **listening chain**, which
/// already holds the device: a second capture on the same microphone is a
/// request the backend may refuse, and where it does not refuse it is two
/// streams competing for one buffer — a measurement of the wrong thing.
///
/// Nothing is stored here. The screen shows the numbers and the user decides,
/// because a threshold applied without being seen is one nobody can undo.
#[tauri::command]
pub async fn measure_noise(
    pending: State<'_, crate::voice::calibration::Pending>,
    seconds: f32,
) -> Result<crate::voice::calibration::Noise, String> {
    let pending = pending.inner().clone();
    // Off the UI thread: this blocks for the length of the measurement.
    tauri::async_runtime::spawn_blocking(move || {
        pending.measure(seconds.clamp(2.0, 20.0), crate::voice::default_input_id())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Choose the listening policy: everything, or only when named.
///
/// A different gesture from [`toggle_audio`], which opens and closes the ear.
/// Two controls, two scopes — the same split the orb and STOP already carry.
/// From `Off` this is refused, because there is nothing to apply a policy to,
/// and the refusal comes back as a sentence rather than a silent no-op.
#[tauri::command]
pub fn set_listening(ipc: State<'_, Ipc>, everything: bool) -> Result<AudioMode, String> {
    let trigger = if everything {
        ModeTrigger::ListenToAll
    } else {
        ModeTrigger::NamedOnly
    };
    ipc.core
        .apply_audio(trigger)
        .map(|change| change.to)
        .map_err(|refusal| refusal.to_string())
}

/// What a preview says. Long enough to judge a voice, short enough to sit
/// through twenty-seven times.
const PREVIEW: &str = "Bonjour, je suis Aria. Voici à quoi je ressemble.";

/// Say one sentence with a voice, to hear it before choosing it.
///
/// On its own player, opened and dropped for this one sentence: the running
/// voice is mid-answer as often as not, and trying a candidate must not cut
/// ARIA off. The cost is a WASAPI stream opened for a second, which is what
/// a preview is.
///
/// # Every engine is previewed the same way now, which it was not
///
/// This used to say that Kyutai was different: the preview had to **attach to
/// a running server**, because reading a hundred-megabyte model to hear one
/// sentence would be a worse answer than refusing — so twenty-seven voices sat
/// there with « écouter » greyed out until somebody pressed « préparer ». A
/// `Warm` piece of Tauri state existed for nothing else, and is gone.
///
/// Decision 56 removed the server and the distinction with it. The weights are
/// held by `aria_voice::tts::kyutai`'s own registry, so a second voice costs
/// the small prompt file on top of them and the first costs the load either
/// way. Every engine is now simply opened.
#[tauri::command]
pub async fn try_voice(
    engine: crate::config::Engine,
    id: String,
    language: String,
    model: String,
    quantize: bool,
) -> Result<(), String> {
    use aria_voice::tts::{Player, Playing};
    // Off the UI thread: synthesis is tens of milliseconds and playback is
    // real time, and neither belongs in an IPC handler's thread.
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let voice = candidate(engine, &id, &language, &model, quantize)?;
        let player = Player::open(voice).map_err(|e| format!("{e:#}"))?;
        player.say(PREVIEW).map_err(|e| format!("{e:#}"))?;
        while player.state() == Playing::Speaking {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The voice to try, by engine.
fn candidate(
    engine: crate::config::Engine,
    id: &str,
    language: &str,
    model: &str,
    quantize: bool,
) -> Result<Box<dyn aria_voice::tts::Voice>, String> {
    match engine {
        crate::config::Engine::Kyutai => {
            use aria_voice::tts::kyutai;
            // The voice is left empty-means-default rather than resolved here,
            // because `Setup::resolved_voice` is the one place that knows what
            // each language falls back to — and previewing "la meilleure pour
            // la langue" has to play the voice that will actually be used.
            Ok(Box::new(
                kyutai::KyutaiVoice::open(&kyutai::Setup {
                    language: kyutai::model_for(language).to_string(),
                    voice: id.trim().to_string(),
                    // The setting is followed rather than forced to f32: a
                    // tool for hearing a voice before choosing it must play
                    // the weights that will speak. It costs nothing extra in
                    // the ordinary case, because the real player asks for the
                    // same precision and gets the same resident copy — only
                    // toggling the switch holds two.
                    quantize,
                })
                .map_err(|e| format!("{e:#}"))?,
            ))
        }
        crate::config::Engine::Phonon => {
            use aria_voice::tts::phonon;
            Ok(Box::new(
                phonon::PhononVoice::open(&phonon::Setup {
                    model: model.to_string(),
                    voice: id.trim().to_string(),
                    language: language.to_string(),
                })
                .map_err(|e| format!("{e:#}"))?,
            ))
        }
    }
}

/// One voice Kyutai Pocket TTS can speak with.
///
/// Carries the licence, because these come from several corpora under
/// different licences and the file name says nothing about which: the screen
/// shows the flag beside the choice.
#[derive(serde::Serialize)]
pub struct KyutaiVoice {
    pub id: String,
    pub commercial: bool,
}

/// The voices the released weights carry an embedding for.
///
/// Not read off the machine, unlike [`voices`]: this is a catalogue published
/// with the model, so it is the same list on every machine and asking the
/// server for it would mean having one running before the settings screen can
/// be opened.
#[tauri::command]
pub fn kyutai_voices() -> Vec<KyutaiVoice> {
    aria_voice::tts::kyutai::VOICES
        .iter()
        .map(|v| KyutaiVoice {
            id: v.id.to_string(),
            commercial: v.commercial,
        })
        .collect()
}

/// What Kyutai costs on this machine right now.
///
/// # Four fields fewer, because three questions stopped existing
///
/// It carried `binary`, `present` and `releases` — where `pocket-tts.exe` was
/// looked for, whether it was there, and the release page to fetch it from —
/// and `running`, whether a server answered. Decision 56 linked the model, so
/// there is no file to install and no port to answer on. What is left is the
/// only thing a person can still act on: where four hundred megabytes go, and
/// whether they are already there.
#[derive(serde::Serialize)]
pub struct KyutaiStatus {
    /// Where the weights live, which ARIA decides rather than inherits — see
    /// `kyutai::cache_dir`, and the surprise that made it necessary.
    pub cache: String,
    /// Whether that cache already holds a model. A probe of someone else's
    /// layout, so it decides what a button says and never a result.
    pub weights: bool,
    /// Whether the weights are in **memory**, this instant.
    ///
    /// The difference between « écouter » answering in a moment and taking a
    /// few seconds. Not a gate any more — a preview loads what it needs —
    /// which is why the screen can state it instead of disabling things.
    pub loaded: bool,
}

#[tauri::command]
pub fn kyutai_status() -> KyutaiStatus {
    use aria_voice::tts::kyutai;
    KyutaiStatus {
        cache: kyutai::cache_dir().display().to_string(),
        weights: kyutai::weights_cached(),
        loaded: kyutai::models_in_memory() > 0,
    }
}

/// Put the Kyutai model on the machine now, rather than at the next launch.
///
/// # Why this is a model load and not a download
///
/// It would look tidier to fetch the files with `hf-hub` and report bytes.
/// It would also be a **second source of truth**: which files a language needs
/// is Pocket TTS's own resolution, embedded in its binary — one of two
/// repositories depending on whether the gated one answers, a revision pinned
/// per language inside each config, int8 variants under `--quantize`. A copy of
/// that list here would break silently the day Kyutai bumps a revision, and it
/// would break in the worst way: a download that succeeds, followed by a
/// server that downloads something else anyway.
///
/// Since the library already uses `hf-hub` against a cache ARIA imposes
/// (`kyutai::cache_dir`), the honest way to pre-download is to **ask it to
/// load the model** and then let go. Whatever was missing is now on disk, and
/// whatever was there was not fetched twice.
///
/// « Let go » now means something different, and better: the weights stay
/// resident in `kyutai`'s registry rather than in a child process somebody has
/// to keep alive. So there is nothing to hold here — no `Warm` state, no
/// server to find answering at the next start-up — and the sub-menu above can
/// audition twenty-seven voices against the copy this call left behind.
///
/// So the gain is not the download itself — it is **when** it happens. Chosen
/// in the settings, with the bar on screen and the log scrolling, instead of
/// silently at the next start-up while someone waits for an orb that will not
/// light up for four hundred megabytes.
///
/// Registered as [`Load::Voice`], so the orb's bar is the one from decision 51
/// rather than a second progress indicator that means something slightly
/// different.
#[tauri::command]
pub async fn prepare_kyutai(
    ipc: State<'_, Ipc>,
    language: String,
    voice: String,
    quantize: bool,
) -> Result<String, String> {
    use aria_voice::tts::kyutai;
    let core = Arc::clone(&ipc.core);
    let setup = kyutai::Setup {
        language: kyutai::model_for(&language).to_string(),
        voice,
        quantize,
    };
    let had = kyutai::weights_cached();
    core.begin_load(Load::Voice);
    // Opened and dropped: what is kept is the model, by the registry inside
    // the engine, and the voice prompt on top of it was the cheap half.
    let opened =
        tauri::async_runtime::spawn_blocking(move || kyutai::KyutaiVoice::open(&setup).map(|_| ()))
            .await;
    core.finished_load(Load::Voice);

    match opened.map_err(|e| e.to_string())? {
        Ok(()) if had => {
            Ok("Le modèle était déjà là. Il est en mémoire : les voix sont écoutables.".into())
        }
        Ok(()) => Ok(format!(
            "Modèle téléchargé dans {} et chargé. Les voix sont écoutables.",
            kyutai::cache_dir().display()
        )),
        Err(e) => Err(format!("{e:#}")),
    }
}

/// One Phonon model on offer, with what choosing it costs.
#[derive(serde::Serialize)]
pub struct PhononModel {
    pub repo: String,
    pub language: String,
    /// Whether its licence allows commercial use. Every French one is `false`.
    pub commercial: bool,
    /// Whether it needs an accepted licence and a token.
    pub gated: bool,
    /// Whether its files are already on this machine.
    pub cached: bool,
    /// The languages of the voices offered with it.
    pub voices: Vec<String>,
}

/// The Phonon models, with their licences.
///
/// A fixed list rather than a Hub query, so the settings screen opens without
/// the network — and because each licence is a fact we checked rather than one
/// the API reports usefully.
#[tauri::command]
pub fn phonon_models() -> Vec<PhononModel> {
    use aria_voice::tts::phonon;
    phonon::MODELS
        .iter()
        .map(|m| PhononModel {
            repo: m.repo.to_string(),
            language: m.language.to_string(),
            commercial: m.commercial,
            gated: m.gated,
            cached: phonon::weights_cached(m.repo),
            voices: m.voices.iter().map(|v| v.to_string()).collect(),
        })
        .collect()
}

/// One Phonon voice, as the sub-menu shows it.
#[derive(serde::Serialize)]
pub struct PhononVoice {
    /// What the setting stores: stable across a relabelled catalogue.
    pub id: String,
    /// What a person reads — `Noémie`, `Augustin`.
    pub name: String,
    pub language: String,
    pub gender: String,
    pub description: String,
}

/// Every voice Phonon can speak with. The screen shows those of the chosen
/// model, see [`PhononModel::voices`].
///
/// Read from the set the installer carries. All `apache-2.0`, unlike the models — so
/// nothing here has to be flagged.
#[tauri::command]
pub async fn phonon_voices() -> Result<Vec<PhononVoice>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        aria_voice::tts::phonon::catalogue("")
            .map(|voices| {
                voices
                    .into_iter()
                    .map(|v| PhononVoice {
                        id: v.voice_id,
                        name: v.name,
                        language: v.language,
                        gender: v.gender,
                        description: v.description,
                    })
                    .collect()
            })
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Put the Phonon model on the machine now, rather than at the next launch.
///
/// Same shape and same reasoning as [`prepare_kyutai`], with one difference
/// that matters: there is no server to keep alive afterwards. The model is
/// loaded to prove it loads and to leave its files cached, then dropped —
/// previews reload it in about a quarter of a second, which is the whole
/// advantage of being in-process.
#[tauri::command]
pub async fn prepare_phonon(
    ipc: State<'_, Ipc>,
    model: String,
    voice: String,
    language: String,
) -> Result<String, String> {
    use aria_voice::tts::phonon;
    let core = Arc::clone(&ipc.core);
    let had = phonon::resolve(&model, &language).is_some_and(|m| phonon::weights_cached(m.repo));
    let setup = phonon::Setup {
        model: model.clone(),
        voice,
        language,
    };

    core.begin_load(Load::Voice);
    let outcome =
        tauri::async_runtime::spawn_blocking(move || phonon::PhononVoice::open(&setup).map(|_| ()))
            .await;
    core.finished_load(Load::Voice);

    match outcome.map_err(|e| e.to_string())? {
        Ok(()) if had => Ok("Le modèle était déjà là, et il charge correctement.".into()),
        Ok(()) => Ok(format!(
            "Modèle téléchargé dans {} et chargé.",
            phonon::cache_dir().display()
        )),
        Err(e) => Err(format!("{e:#}")),
    }
}

/// What ARIA is told it is, shipped part included.
///
/// Returned so the settings screen can **show** the rules the persona is added
/// to. A field that says "your text is appended" without showing to what is a
/// field people fill in blind — and the shipped lines are the ones that make
/// delegation and verbatim repetition work.
#[tauri::command]
pub fn shipped_prompt() -> &'static str {
    aria_core::agent::tools::SYSTEM_PROMPT
}

/// Say something to the main agent, from the keyboard.
///
/// The same door the microphone comes through — `heard_turn` — so what is typed
/// and what is spoken are one conversation, and the agent cannot tell them
/// apart. Epic 04 asks for exactly that: cutting the ear must never interrupt
/// anything, so you hand back to the keyboard and pick the orb up afterwards.
///
/// The transcript is published too, because the window's transcript area is
/// what shows that the turn was taken at all.
///
/// Returns whether anyone was listening. A `false` is not an error — ARIA runs
/// with its agent off if the CLI is missing — but it is the difference between
/// "it said nothing" and "nothing is running", and the interface owes the user
/// that distinction.
#[tauri::command]
pub fn say_to_agent(ipc: State<'_, Ipc>, text: String) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    ipc.core.push_transcript(text, true);
    let listening = ipc.core.conversation().is_some();
    ipc.core.heard_turn(text);
    listening
}

/// The **Arrêter** button decision 8 puts on a session that has gone quiet.
///
/// Decision 8 again, from the other side: this is the *only* way a session is
/// killed. Nothing in the core does it on a timer.
#[tauri::command]
pub async fn stop_session(ipc: State<'_, Ipc>, id: SessionId) -> Result<(), String> {
    ipc.core.stop_session(id).await.map_err(|e| e.to_string())
}

/// What a scenario is, as the window lists it.
///
/// Debug builds only. Each one drives the **real** core — real events, real
/// sequence numbers, real replay, real permission desk — and the only fiction
/// is that no subprocess is running.
#[cfg(debug_assertions)]
#[derive(Clone, Debug, serde::Serialize)]
pub struct Scenario {
    pub id: &'static str,
    pub label: &'static str,
    /// One line saying what it exercises, shown under the name.
    pub about: &'static str,
}

/// Everything the window may play.
///
/// Written out rather than derived: the order is the order they appear, and
/// it goes from the one that exercises the most to the ones that isolate a
/// single piece.
#[cfg(debug_assertions)]
#[tauri::command]
pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            id: "weather",
            label: "Météo, de bout en bout",
            about: "Session, étapes, autorisation, réponse par phrases, tuile avec formulaire.",
        },
        // One file per scenario, so each can be judged on its own — a tile
        // carrying all three hid the one that did not render behind the two
        // that did.
        Scenario {
            id: "audio",
            label: "Tuile musique",
            about: "Un fichier audio (mp3, flac…) lu depuis le bac à sable, avec requêtes de plage.",
        },
        Scenario {
            id: "video",
            label: "Tuile vidéo",
            about: "Une vidéo servie depuis le bac à sable.",
        },
        Scenario {
            id: "pdf",
            label: "Tuile PDF",
            about: "Le visionneur PDF de la fenêtre, dans une tuile.",
        },
        Scenario {
            id: "clock",
            label: "Tuile horloge",
            about: "Une tuile qui bouge toute seule : script en ligne sous la CSP.",
        },
        Scenario {
            id: "refresh",
            label: "Tuile actualisée",
            about: "Une tuile posée, puis réécrite par sa session trois secondes après.",
        },
        Scenario {
            id: "permission",
            label: "Demande d'autorisation",
            about: "Une demande qui reste en attente : elle n'expire pas.",
        },
        Scenario {
            id: "secret",
            label: "Secret d'un hook",
            about: "Une session demande un mot de passe : le formulaire d'ARIA, sans carte, puis le trousseau.",
        },
        Scenario {
            id: "failure",
            label: "Session qui échoue",
            about: "Ce qui s'affiche quand une session se termine mal.",
        },
    ]
}

/// Play one of them.
///
/// Debug builds only, so it cannot ship by accident.
#[cfg(debug_assertions)]
#[tauri::command]
pub async fn play_scenario(ipc: State<'_, Ipc>, id: String) -> Result<(), String> {
    match id.as_str() {
        "audio" => return play_file(&ipc, &AUDIO).await,
        "video" => return play_file(&ipc, &VIDEO).await,
        "pdf" => return play_file(&ipc, &PDF).await,
        "clock" => return play_clock(&ipc).await,
        "refresh" => return play_refresh(&ipc).await,
        "permission" => return play_permission(&ipc).await,
        "secret" => return hooks::play_secret(&ipc).await,
        "failure" => return play_failure(&ipc).await,
        // The weather one is the original and stays the default: an unknown
        // id plays it rather than doing nothing, because a button that does
        // nothing is the worst way to learn a name was mistyped.
        _ => {}
    }
    use std::time::Duration;
    use tokio::time::sleep;

    let core = Arc::clone(&ipc.core);
    let session = core.open_session("Quel temps fera-t-il demain ?", None);

    core.set_session_state(session, SessionState::Thinking);
    sleep(Duration::from_millis(600)).await;

    core.push_step(
        session,
        ActivityStep {
            label: "WebFetch".into(),
            command: Some("api.open-meteo.com/v1/forecast".into()),
            status: StepStatus::Running,
        },
    );
    core.set_session_state(session, SessionState::Executing);
    sleep(Duration::from_millis(900)).await;

    core.push_step(
        session,
        ActivityStep {
            label: "WebFetch".into(),
            command: Some("api.open-meteo.com/v1/forecast".into()),
            status: StepStatus::Ok,
        },
    );

    // The request carries the command itself, not just the tool name. Epic 02
    // requires that from the V0: a pattern registry that only ever sees tool
    // names can express no pattern.
    core.ask_permission(PermissionRequest::new(
        session,
        "Bash",
        Some("Set-Location D:/Code && git status".into()),
        "lire l'état du dépôt",
        Risk::Low,
    ));
    sleep(Duration::from_millis(400)).await;

    for fragment in [
        "Demain, ",
        "ciel couvert sur Lyon, ",
        "12 degrés au plus chaud, ",
        "averses en fin d'après-midi.",
    ] {
        core.push_message_delta(session, fragment);
        sleep(Duration::from_millis(220)).await;
    }

    // And the tile, which is what T7 is for: pushed through the real registry,
    // served from the real sandbox origin, framed by the real layer. The only
    // fiction is the forecast — the tile says so itself, so nobody mistakes
    // demonstration data for a measurement.
    // Avec son formulaire, parce que c'est la seule façon de le voir sans
    // dépenser du quota — et parce qu'un formulaire déclaré par une session
    // et dessiné par la fenêtre ne se vérifie qu'à l'œil.
    core.show_tile(
        session,
        "Météo · Lyon",
        SCENARIO_TILE,
        Vec::new(),
        Some(aria_core::tiles::Form {
            submit: "Recalculer".into(),
            fields: vec![
                aria_core::tiles::Field {
                    name: "ville".into(),
                    label: "Ville".into(),
                    kind: aria_core::tiles::FieldKind::Text,
                    options: Vec::new(),
                    value: "Lyon".into(),
                },
                aria_core::tiles::Field {
                    name: "jours".into(),
                    label: "Horizon".into(),
                    kind: aria_core::tiles::FieldKind::Choice,
                    options: vec!["demain".into(), "3 jours".into(), "la semaine".into()],
                    value: "demain".into(),
                },
                aria_core::tiles::Field {
                    name: "alertes".into(),
                    label: "Alertes".into(),
                    kind: aria_core::tiles::FieldKind::Toggle,
                    options: Vec::new(),
                    value: String::new(),
                },
            ],
        }),
    );

    core.finish_session(session, "Prévision du jour récupérée");
    Ok(())
}

/// One kind of file a demonstration tile can carry.
#[cfg(debug_assertions)]
struct DemoKind {
    /// The served name's stem, and what the tile calls it.
    name: &'static str,
    title: &'static str,
    /// Names a file directly, ahead of any search.
    env: &'static str,
    /// Tried after the variable and before the search, when it exists.
    ///
    /// The developer's own file, asked for by name on 2026-11-16: a variable
    /// has to be set in the very shell that runs `npm run dev`, and that is
    /// one step too many for a debug button. Debug builds only, and a path
    /// that does not exist on another machine is simply skipped.
    preferred: Option<&'static str>,
    extensions: &'static [&'static str],
    /// `NAME` is replaced by the served name.
    markup: &'static str,
}

#[cfg(debug_assertions)]
const AUDIO: DemoKind = DemoKind {
    name: "audio",
    title: "Musique",
    env: "ARIA_DEMO_AUDIO",
    preferred: Some(r"D:\MP3\Linkin Park\2024 - From zero (FLAC)\02. The Emptiness Machine.flac"),
    extensions: &["flac", "mp3", "m4a", "wav", "ogg"],
    markup: r#"<p class="label">Audio</p><audio controls src="f/NAME"></audio>"#,
};

#[cfg(debug_assertions)]
const VIDEO: DemoKind = DemoKind {
    name: "video",
    title: "Vidéo",
    env: "ARIA_DEMO_VIDEO",
    preferred: None,
    extensions: &["mp4", "webm", "m4v"],
    markup: r#"<p class="label">Vidéo</p><video controls width="100%" src="f/NAME"></video>"#,
};

#[cfg(debug_assertions)]
const PDF: DemoKind = DemoKind {
    name: "document",
    title: "Document PDF",
    env: "ARIA_DEMO_PDF",
    preferred: None,
    extensions: &["pdf"],
    // No `<embed>`: Chromium refuses its PDF viewer inside a sandboxed frame,
    // so the window frames the declared PDF beside the tile — see
    // `aria_core::tiles::Tile::documents`. The tile only says what it is.
    markup: r#"<p class="label">Document</p><p>affiché sous la tuile par ARIA</p>"#,
};

/// A tile carrying one real file.
///
/// # Why it hunts for files instead of shipping them
///
/// What is being checked is the **plumbing** — ranges, content types, the
/// policy, the viewer — and that needs real files, which have no business in
/// a repository. So it takes the file named by the kind's own variable
/// (`ARIA_DEMO_AUDIO`, `ARIA_DEMO_VIDEO`, `ARIA_DEMO_PDF`), or the first one
/// found in the folders of [`demo_folders`], and says where it looked rather
/// than failing on an empty folder.
#[cfg(debug_assertions)]
async fn play_file(ipc: &State<'_, Ipc>, kind: &DemoKind) -> Result<(), String> {
    let core = Arc::clone(&ipc.core);
    let session = core.open_session(&format!("Montre-moi un fichier : {}", kind.title), None);
    core.set_session_state(session, SessionState::Executing);

    let folders = demo_folders();
    let found = std::env::var(kind.env)
        .ok()
        .filter(|path| std::path::Path::new(path).is_file())
        .or_else(|| {
            kind.preferred
                .filter(|path| std::path::Path::new(path).is_file())
                .map(String::from)
        })
        .or_else(|| {
            folders
                .iter()
                .find_map(|folder| first_with(std::path::Path::new(folder), kind.extensions, 3))
        });

    let mut files = Vec::new();
    let mut body = String::new();
    match found {
        Some(path) => {
            // The served name is ours, not the file's: a real name carries
            // spaces, accents and parentheses, and the point here is the
            // plumbing rather than URL-escaping someone's music library.
            let served = format!("{}.{}", kind.name, extension_of(&path));
            body.push_str(&kind.markup.replace("NAME", &served));
            let shown = std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            body.push_str(&format!(
                r#"<p class="note">{}</p>"#,
                shown.replace('&', "&amp;").replace('<', "&lt;")
            ));
            files.push(aria_core::tiles::TileFile { name: served, path });
        }
        None => body.push_str(&format!(
            r#"<p class="label">{}</p><p class="note bad">aucun fichier trouvé ({})</p>"#,
            kind.title,
            kind.extensions.join(", ")
        )),
    }
    body.push_str(&format!(
        // One line: a `\` continuation does not exist in a raw string, and
        // the backslash ended up on screen.
        r#"<hr><p class="note">cherché dans <code>{}</code> — <code>{}</code> désigne un fichier, <code>ARIA_DEMO_FILES</code> des dossiers séparés par « ; »</p>"#,
        folders.join(" ; ").replace('&', "&amp;").replace('<', "&lt;"),
        kind.env
    ));

    core.show_tile(session, kind.title, body, files, None);
    core.finish_session(session, "Fichier affiché");
    Ok(())
}

/// Where a demonstration looks: `ARIA_DEMO_FILES` (separated by `;`), or the
/// user's Downloads, Music, Videos and Documents, in that order.
#[cfg(debug_assertions)]
fn demo_folders() -> Vec<String> {
    if let Ok(list) = std::env::var("ARIA_DEMO_FILES") {
        let folders: Vec<String> = list
            .split(';')
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .map(String::from)
            .collect();
        if !folders.is_empty() {
            return folders;
        }
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    ["Downloads", "Music", "Videos", "Documents"]
        .iter()
        .map(|sub| format!("{home}/{sub}"))
        .collect()
}

/// The first file under `folder` with one of these extensions.
///
/// Files of a folder before its subfolders, everything sorted, at most
/// `depth` levels down — so two runs on the same disk show the same file,
/// because a demonstration that picks differently each time is one nobody can
/// compare against the last; and a music library is `artist/album/track`,
/// which is why one level is not enough.
#[cfg(debug_assertions)]
fn first_with(folder: &std::path::Path, extensions: &[&str], depth: usize) -> Option<String> {
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    let matching = entries.iter().find(|path| {
        path.is_file() && extensions.contains(&extension_of(&path.display().to_string()).as_str())
    });
    if let Some(path) = matching {
        return Some(path.display().to_string());
    }
    if depth == 0 {
        return None;
    }
    entries
        .iter()
        .filter(|path| path.is_dir())
        .find_map(|dir| first_with(dir, extensions, depth - 1))
}

#[cfg(debug_assertions)]
fn extension_of(path: &str) -> String {
    path.rsplit('.').next().unwrap_or_default().to_lowercase()
}

/// A tile that moves on its own — decision 68.
#[cfg(debug_assertions)]
async fn play_clock(ipc: &State<'_, Ipc>) -> Result<(), String> {
    let core = Arc::clone(&ipc.core);
    let session = core.open_session("Quelle heure est-il ?", None);
    core.show_tile(
        session,
        "Horloge",
        r#"<p class="label">Heure locale</p>
<p class="big" id="t">&ndash;</p>
<p class="note">rendue une seule fois ; c'est le script qui la fait battre</p>
<script>
  function tic() {
    document.getElementById("t").textContent =
      new Date().toLocaleTimeString("fr-FR");
  }
  tic(); setInterval(tic, 1000);
</script>"#,
        Vec::new(),
        None,
    );
    core.finish_session(session, "Horloge affichée");
    Ok(())
}

/// A tile, then the same tile rewritten by its session — decision 73.
#[cfg(debug_assertions)]
async fn play_refresh(ipc: &State<'_, Ipc>) -> Result<(), String> {
    use std::time::Duration;
    use tokio::time::sleep;

    let core = Arc::clone(&ipc.core);
    let session = core.open_session("Compte les fichiers du projet", None);
    core.set_session_state(session, SessionState::Executing);
    let uri = core.show_tile(
        session,
        "Décompte",
        r#"<p class="label">Fichiers</p><p class="big">…</p>
           <p class="note">la session compte encore</p>"#,
        Vec::new(),
        None,
    );

    // Long enough to see the first state, short enough not to wonder whether
    // the button worked.
    sleep(Duration::from_millis(3000)).await;
    core.refresh_tile(
        &uri,
        Some(
            r#"<p class="label">Fichiers</p><p class="big">1 284</p>
               <p class="note ok">même tuile, même place, contenu remplacé</p>"#
                .to_string(),
        ),
    );
    core.finish_session(session, "Décompte terminé");
    Ok(())
}

/// A permission request, left pending — decision 8: it does not expire.
#[cfg(debug_assertions)]
async fn play_permission(ipc: &State<'_, Ipc>) -> Result<(), String> {
    let core = Arc::clone(&ipc.core);
    let session = core.open_session("Fais le ménage dans le dossier", None);
    core.set_session_state(session, SessionState::Executing);
    core.ask_permission(PermissionRequest::new(
        session,
        "Bash",
        Some("Remove-Item D:/Code/ARIA/target -Recurse -Force".into()),
        "supprimer le dossier de compilation",
        Risk::High,
    ));
    Ok(())
}

/// A session that ends badly, because that view is never exercised otherwise.
#[cfg(debug_assertions)]
async fn play_failure(ipc: &State<'_, Ipc>) -> Result<(), String> {
    use std::time::Duration;
    use tokio::time::sleep;

    let core = Arc::clone(&ipc.core);
    let session = core.open_session("Déploie en production", None);
    core.set_session_state(session, SessionState::Thinking);
    sleep(Duration::from_millis(700)).await;
    core.push_step(
        session,
        ActivityStep {
            label: "Bash".into(),
            command: Some("npm run deploy".into()),
            status: StepStatus::Running,
        },
    );
    core.set_session_state(session, SessionState::Executing);
    sleep(Duration::from_millis(1200)).await;
    core.push_step(
        session,
        ActivityStep {
            label: "Bash".into(),
            command: Some("npm run deploy".into()),
            status: StepStatus::Failed,
        },
    );
    core.fail_session(session, "le script de déploiement est sorti en erreur 1");
    Ok(())
}

/// The scenario's tile. A **fragment**, like any other: the look below is
/// ARIA's own, applied by the core, and this is what proves it — if the
/// scenario tile came out styled it would prove nothing about what a session
/// can produce.
///
/// Deliberately plain markup, using only the vocabulary `show_tile`'s
/// description announces. Change this and the description together, or the
/// demonstration stops demonstrating.
#[cfg(debug_assertions)]
const SCENARIO_TILE: &str = r#"<p class="label">Lyon &middot; donn&eacute;es de d&eacute;monstration</p>
<div class="row">
  <span class="big">12&deg;</span>
  <span>
    <div>Ciel couvert</div>
    <div class="note">averses en fin d'apr&egrave;s-midi</div>
  </span>
</div>
<table>
  <tr><th>Moment</th><th class="num">&deg;C</th><th>Ciel</th></tr>
  <tr><td>Matin</td><td class="num">8</td><td>Couvert</td></tr>
  <tr><td>Apr&egrave;s-midi</td><td class="num">12</td><td class="warn">Averses</td></tr>
  <tr><td>Soir</td><td class="num">9</td><td>Couvert</td></tr>
</table>"#;

// ------------------------------------------------------------------- tiles

/// A tile asked the host to run a tool.
///
/// # Why this goes through the host and not straight to the core
///
/// SEP-1865 puts the boundary here: "the host MUST reject a `tools/call` from a
/// view for a tool that does not carry `app` visibility". The tile speaks
/// JSON-RPC over `postMessage` to the window, the window relays it here, and
/// this is the process that decides — which is exactly what
/// a throwaway prototype proved on 2026-10-23, and what decision 10
/// means by "the boundary is not the `sandbox` attribute".
///
/// The window is **not** trusted to filter: it is the same process as the
/// relay, so a check it did would be a check the tile's neighbour did. The
/// answer is therefore computed here, from
/// [`aria_core::tiles::may_call_from_tile`], which today says no to everything.
#[tauri::command]
pub fn tile_call(tool: String, _arguments: serde_json::Value) -> Result<serde_json::Value, String> {
    if !aria_core::tiles::may_call_from_tile(&tool) {
        tracing::warn!("une tuile a tenté d'appeler {tool} — refusé");
        return Err(format!(
            "L'outil {tool} ne porte pas la visibilité « app » : une tuile ne peut pas l'appeler."
        ));
    }
    // Unreachable while the door is closed. Left as an explicit refusal rather
    // than `unreachable!()`: the day a tool carries `app` visibility, this is
    // the line that has to be written, and a panic here would be a tile
    // crashing the application.
    Err("Aucun outil n'est encore exécutable depuis une tuile.".into())
}

/// The window closed a tile.
///
/// Decision 11: a tile is an object of the interface's session, so closing it
/// is the only removal in the product — and it has to make the tile
/// **unreachable**, not merely invisible. The window is not the only thing
/// that can hold the URL.
#[tauri::command]
pub fn close_tile(ipc: State<'_, Ipc>, uri: String) -> bool {
    let gone = ipc.core.tiles().remove(&uri);
    if gone {
        tracing::debug!("tuile fermée : {uri}");
    }
    gone
}

/// A tile asked to open a link — SEP-1865's `ui/open-link`.
///
/// # Why it leaves ARIA rather than opening inside it
///
/// Decided on 2026-11-01, after measuring the alternative: `netflix.com`
/// answers `X-Frame-Options: DENY` and `youtube.com` answers `SAMEORIGIN`, so
/// a tile cannot frame them whatever our policy says — the site refuses, not
/// us. And relaxing `frame-src` to allow remote framing would reopen the
/// exfiltration channel `connect-src 'none'` closes: a tile cannot make a
/// request, but it could put data in the URL of a frame.
///
/// So the link opens in the **user's own browser**, with their session, their
/// DRM and their full screen — and outside ARIA, where it can do nothing to
/// ARIA. An embedded browser panel is noted as a future evolution; it is a
/// different concept with a different threat model, and must not be a tile.
///
/// # What is not checked, and the risk that leaves
///
/// Only the scheme, in [`aria_core::tiles::openable`]. There is no
/// confirmation: the user asked for the simple thing, and this is it.
///
/// The consequence, stated rather than hidden: **a tile can open a link
/// without anyone clicking**, because the host cannot tell a click from the
/// tile's own script. It is bounded — the link is a web address, it opens
/// outside the application, and the log below names it — but it is a real
/// property. The permission desk is one call away the day it matters.
#[tauri::command]
pub fn tile_open_link(url: String) -> Result<(), String> {
    if !aria_core::tiles::openable(&url) {
        tracing::warn!("une tuile a tenté d'ouvrir {url} — refusé");
        return Err("Seuls les liens http et https peuvent être ouverts depuis une tuile.".into());
    }
    // Loud on purpose: this is the one thing a tile does that leaves the
    // machine's screen, so "why did my browser just open" has an answer.
    tracing::info!("ouverture dans le navigateur : {url}");
    open::that_detached(&url).map_err(|e| format!("le navigateur n'a pas pu s'ouvrir : {e}"))
}

/// A tile's form came back filled in.
///
/// # Why the window sends values and not markup
///
/// The form was **declared** by the session and **drawn by the window**,
/// outside the sandboxed frame — decision 69. That is what makes these
/// values the user's: the tile's script cannot read them, because they were
/// never in its document, and cannot submit them, because it has no hand on
/// this call.
///
/// Only declared fields survive the trip:
/// [`aria_core::tiles::Form::report`] walks the declaration rather than the
/// payload, so a key nobody asked for never reaches the session.
///
/// Answering may **relaunch** the session, since a tile outlives the work
/// that drew it. See `Core::deliver_to_session`.
#[tauri::command]
pub async fn submit_tile_form(
    ipc: State<'_, Ipc>,
    uri: String,
    answers: std::collections::HashMap<String, String>,
) -> Result<(), String> {
    let Some(tile) = ipc.core.tiles().get(&uri) else {
        return Err("Cette tuile n'existe plus.".into());
    };
    let Some(form) = tile.form.as_ref() else {
        return Err("Cette tuile ne porte pas de formulaire.".into());
    };

    let text = form.report(&tile.title, &answers);
    let core = Arc::clone(&ipc.core);
    let reached = core
        .deliver_to_session(tile.session, &text)
        .await
        .map_err(|e| format!("{e:#}"))?;

    // Re-pointed when the session was resumed under a new id, so a second
    // submission reaches the one that answered the first rather than starting
    // a third.
    if reached != tile.session {
        ipc.core.tiles().reattach(&uri, reached);
    }
    Ok(())
}

// ----------------------------------------------------- the conversations

/// The left-hand menu: every saved thread, most recently used first.
///
/// Also says which one is live, so the menu can mark it without a second
/// round trip and without inferring it from the order.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversations {
    pub current: Option<ConversationId>,
    pub threads: Vec<Conversation>,
    /// Whether anything is being saved at all.
    ///
    /// `false` when `aria.db` would not open. Said rather than merely logged,
    /// which is decision 5's rule: an empty menu that means "nothing saved
    /// yet" and one that means "nothing is being saved" look identical, and
    /// the user finds out tomorrow.
    pub saving: bool,
}

#[tauri::command]
pub fn conversations(ipc: State<'_, Ipc>) -> Conversations {
    Conversations {
        current: ipc.core.current_conversation(),
        threads: ipc.core.conversations(),
        saving: ipc.core.history_attached(),
    }
}

/// One thread's turns, for the panel that shows it.
#[tauri::command]
pub fn conversation_thread(ipc: State<'_, Ipc>, id: ConversationId) -> Vec<Entry> {
    ipc.core
        .with_history("la lecture d'un fil", |history| history.thread(id))
        .unwrap_or_default()
}

/// The project sessions of one thread, with those that can no longer be
/// resumed still in the list and marked.
#[tauri::command]
pub fn conversation_sessions(ipc: State<'_, Ipc>, id: ConversationId) -> Vec<ProjectSession> {
    ipc.core.project_sessions(id)
}

/// Reopen a conversation: the user clicked a line in the left-hand menu.
///
/// Three things happen, in this order, and the order is the whole of it:
///
/// 1. the core makes that thread the live one and hands back its turns;
/// 2. the pilot is **replaced** — a fresh CLI process, because the old one
///    still holds the conversation being left and would answer out of it;
/// 3. the thread is replayed into the new session as context it must not
///    speak.
///
/// Epic 05: reopening relaunches **no project session**. The agent relaunches
/// what it needs when it needs it, which is the same moment it decides whether
/// a session is worth keeping at all.
#[tauri::command]
pub async fn open_conversation(
    app: AppHandle,
    ipc: State<'_, Ipc>,
    id: ConversationId,
) -> Result<(), String> {
    let core = Arc::clone(&ipc.core);
    let Some(thread) = core.open_conversation(id) else {
        return Err("cette conversation est introuvable".into());
    };
    crate::pilot::restart(app, Arc::clone(&core)).await;
    match core.conversation() {
        Some(conversation) => conversation.resume(thread).await,
        // The agent did not come back up. The thread is still the live one, so
        // what is said next lands in it; only the past is not handed over.
        None => tracing::warn!("fil reprise sans agent : le contexte n'a pas été transmis"),
    }
    Ok(())
}

/// Start the main agent again, on the thread already open — the « Réessayer »
/// under a fault, once the user has logged the CLI back in (2026-11-27).
#[tauri::command]
pub async fn restart_agent(app: AppHandle, ipc: State<'_, Ipc>) -> Result<(), String> {
    let core = Arc::clone(&ipc.core);
    let thread = core
        .current_conversation()
        .and_then(|id| core.open_conversation(id));
    crate::pilot::restart(app, Arc::clone(&core)).await;
    if let (Some(thread), Some(conversation)) = (thread, core.conversation()) {
        conversation.resume(thread).await;
    }
    Ok(())
}

/// Start a fresh thread, and a fresh session with it.
#[tauri::command]
pub async fn new_conversation(app: AppHandle, ipc: State<'_, Ipc>) -> Result<(), String> {
    let core = Arc::clone(&ipc.core);
    if core.start_conversation().is_none() {
        return Err("aucun historique n'est ouvert".into());
    }
    crate::pilot::restart(app, core).await;
    Ok(())
}

/// Forget a thread and everything hanging off it. The audit is untouched.
#[tauri::command]
pub fn forget_conversation(ipc: State<'_, Ipc>, id: ConversationId) {
    ipc.core.forget_conversation(id);
}

/// The audit, newest first — who allowed what, and through which channel.
#[tauri::command]
pub fn audit(ipc: State<'_, Ipc>, limit: Option<usize>) -> Vec<Decided> {
    ipc.core.audit(limit.unwrap_or(200))
}

/// Whether a Hugging Face token is set — never the token itself. The
/// first-run screen asks for one when this is `false` and the chosen voice
/// model is gated and not on this machine.
#[tauri::command]
pub fn hf_token() -> bool {
    aria_voice::models::token().is_some()
}

/// Store a token where `hf auth login` would, for every Hugging Face tool on
/// the machine. Validated for shape only: the Hub is the judge of the rest, at
/// the next download.
#[tauri::command]
pub fn save_hf_token(
    token: String,
    config: State<'_, Mutex<Config>>,
    voice: State<'_, crate::voice::speaking::Retuner>,
) -> Result<(), String> {
    aria_voice::models::save_token(&token).map_err(|e| format!("{e:#}"))?;
    // The voice may have failed at start-up for want of this very token: ask
    // for it again, which reloads it if nothing is speaking.
    let setting = config.lock().unwrap().voice.clone();
    voice.retune(setting);
    Ok(())
}
