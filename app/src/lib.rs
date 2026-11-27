//! ARIA — the application.
//!
//! This crate owns the shell and nothing else: windows, tray, the global
//! shortcut, the three states, and the commands the front end may call.
//! Everything that must keep running with no window open lives in
//! `aria-core`, which knows nothing about Tauri.
//!
//! # Start-up order, and why it is this one
//!
//! The core comes up **before** the window, and serves its event stream
//! before any page can ask for it. Not tidiness: the window is a client of the
//! core, not its owner. Sleep is not shutdown, so the thing that survives
//! every window has to be the thing that starts first — and a front that could
//! race the stream it subscribes to would be a front that sometimes starts
//! blank for no reproducible reason.

mod config;
mod engines;
mod instructions;
mod history;
mod hooks;
mod ipc;
mod logs;
mod memory;
mod pilot;
mod shell;
mod update;
mod voice;

use std::sync::{Arc, Mutex, OnceLock};

use aria_core::Core;
use aria_core::transport::{self, SessionToken};
use tauri::{Manager, WindowEvent};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Held until `run` returns, which is the end of the process: dropping it
    // flushes the last lines to the file.
    let _log = logs::init();

    let core = Core::new();
    let token = SessionToken::new();
    let shell = Arc::new(shell::Shell::new());

    let mut builder = tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    // Pressed only. Without this the toggle fires twice per
                    // press — once down, once up — and lands back where it
                    // started, which looks exactly like the shortcut not
                    // working.
                    if event.state() == tauri_plugin_global_shortcut::ShortcutState::Pressed {
                        shell::toggle(app);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(update::PendingUpdate::default())
        .manage(Arc::clone(&core))
        .manage(Arc::clone(&shell))
        .manage(ipc::Ipc {
            core: Arc::clone(&core),
            token: token.clone(),
        })
        .setup({
            let core = Arc::clone(&core);
            let token = token.clone();
            move |app| {
                let handle = app.handle().clone();

                // The settings file, before anything reads a setting from it.
                let path = config::Config::path(&handle)?;
                let settings = config::Config::load(&path);
                tracing::info!("settings from {}", path.display());
                core.set_default_profile(settings.profile);

                // ONNX Runtime, before anything touches it — the memory's
                // vectoriser and the ear both do (decision 131). The card's
                // own stack if it was downloaded, the shipped DirectML
                // otherwise, which `bundle.resources` puts at the root of the
                // resources, beside the executable. Not fatal: the ear and the
                // memory say so on their own when they cannot start.
                if let Ok(dir) = app.path().resource_dir() {
                    aria_voice::tts::phonon::set_resources(dir.clone());
                    aria_voice::runtime::set_shipped(dir);
                }
                if let Err(e) = aria_voice::runtime::init() {
                    tracing::error!("ONNX Runtime introuvable : {e:#}");
                }
                // Downloads, byte by byte, for the first-run screen (T12).
                // Before anything can start one: the voice and the ear fetch
                // their weights on their own threads as soon as they spawn.
                {
                    let core = Arc::clone(&core);
                    aria_voice::models::observe(move |progress| {
                        core.report_download(progress.map(|p| aria_core::audio::Download {
                            file: p.file,
                            index: p.index,
                            count: p.count,
                            received: p.received,
                            size: p.size,
                        }));
                    });
                }

                // The engine for delegated sessions, chosen by id. Its
                // availability is checked at every launch rather than cached
                // here: a CLI updates itself, and this process outlives several
                // of its updates.
                core.attach_provider(engines::delegation(&settings.delegation));
                // The memory, before anything can be asked of it. Opened here
                // because decision 94 makes the root `app/`'s to resolve: the
                // core receives it and never looks for a home directory
                // itself.
                memory::attach(&handle, &core);
                // `~/.aria/db/aria.db`, and the thread this run writes into.
                // After the memory because it is the same rule — `app/`
                // resolves the path, the core receives it — and before the
                // pilot, because the conversation attaches its journal to
                // whatever thread is open the moment it is built.
                history::attach(&handle, &core);
                // And its vectoriser, on a thread nobody waits for — decision
                // 97, which corrected decision 83: an optional model must never
                // gate the voice.
                memory::spawn_vectoriser(&handle, Arc::clone(&core));
                // The two `AGENTS.md`, created if they are not there yet.
                // After the memory and for the same reason it is here at all:
                // `~` is Tauri's to resolve, and the core only ever receives
                // the paths (decision 106).
                instructions::attach(&handle, &core);
                // `~/.aria/hooks`, after the history: what was validated is in
                // `aria.db`.
                hooks::attach(&handle, &core);

                if let Err(e) = shell::register_shortcut(&handle, &settings.global_shortcut) {
                    // Not fatal: the tray and the windows still work, and the
                    // settings screen is where it gets fixed. Dying here would
                    // make a shortcut someone else grabbed into a reason the
                    // application will not start.
                    tracing::error!("{e}");
                }
                // The voice chain, on its own thread. Which device it uses is
                // what keys its calibration, so it is read here and not inside.
                let device = voice::default_input_id();
                // The authority to stop the voice, shared between the two
                // threads that can need it. Created here because neither owns
                // the other: the speech thread fills it when the player opens,
                // the microphone thread reads it to cut on energy. See
                // `voice::Barge`.
                let speaker: Arc<OnceLock<aria_voice::tts::Silencer>> = Arc::new(OnceLock::new());
                // What ARIA has just said, for the same two threads and the
                // same reason: the speech thread knows the words, the
                // microphone thread is the one that has to recognise them
                // coming back. The slow half of the echo defence — see
                // `aria_voice::echo::SpokenLog`.
                let spoken = Arc::new(aria_voice::echo::SpokenLog::new());
                let retuner = if settings.voice.enabled {
                    voice::speaking::spawn(
                        Arc::clone(&core),
                        settings.voice.clone(),
                        Arc::clone(&speaker),
                        Arc::clone(&spoken),
                    )
                } else {
                    tracing::info!("voix désactivée dans les réglages : ARIA répondra par écrit");
                    voice::speaking::Retuner::none()
                };
                // Decision 123: the settings reach the speech thread through
                // this, so a voice chosen is heard at the next sentence.
                app.manage(retuner);
                let pending = voice::calibration::Pending::new();
                app.manage(pending.clone());
                // Before the voice thread reads it: a launch on a local model
                // that hears must not load Parakeet only to drop it once the
                // pilot starts (epic 10).
                core.set_ear(pilot::ear_for(&settings.agent.engine));
                // Same reason: Parakeet is loaded once, on the right device.
                aria_voice::asr::set_gpu(aria_voice::gpu::wanted(settings.gpu));
                voice::spawn(
                    Arc::clone(&core),
                    settings.calibration_for(&device),
                    settings.name.clone(),
                    voice::Wake::resolve(app.path().resource_dir().ok(), &settings.wake),
                    pending,
                    speaker,
                    spoken,
                );

                app.manage(Mutex::new(settings));

                shell::install_tray(&handle)?;
                // A request that never expires has to have somewhere to appear.
                shell::watch_for_permission_requests(handle.clone(), Arc::clone(&core));

                // The two timers nobody else will run: the three-minute silence
                // mark and the two-minute fall-back out of active listening.
                // Spawned inside the async runtime because that is where
                // `tokio::spawn` has one.
                let watchdog = Arc::clone(&core);
                tauri::async_runtime::spawn(async move {
                    watchdog.spawn_watchdog();
                });
                // The hooks: run on their rhythm, said at the right moment.
                let hooked = Arc::clone(&core);
                tauri::async_runtime::spawn(async move {
                    aria_core::hooks::live::spawn(hooked);
                });

                // The listener is bound **here**, synchronously, and not inside
                // the task that serves it. Binding is what makes the port
                // reachable — the kernel queues connections until axum gets
                // round to accepting them — so doing it before the agent
                // starts turns an ordering question into an ordering fact.
                //
                // It matters because the agent's MCP server lives on this same
                // listener, the CLI connects to it at its first turn, and that
                // handshake losing a race is not an error: it is a model with
                // no tools, explaining politely that it cannot help.
                match transport::bind_events() {
                    Ok(listener) => {
                        let served = Arc::clone(&core);
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) =
                                transport::serve_events_on(listener, served, token).await
                            {
                                tracing::error!("event stream stopped: {e}");
                            }
                        });
                    }
                    // **Fatal**, and that is a change made on 2026-11-03 after
                    // this cost two rounds of diagnosis.
                    //
                    // Without this listener there is no event stream and no
                    // MCP endpoint for the agent: the window opens, shows an
                    // orb, accepts clicks, and nothing it does reaches
                    // anything. Worse, the *other* instance holds the ports,
                    // so the two halves of ARIA end up in different
                    // processes — a window talking to one core while its
                    // tiles are served by another, or by nobody.
                    //
                    // Starting half-dead is the expensive failure. Refusing
                    // to start, with the remedy in the message, is the cheap
                    // one.
                    Err(e) => {
                        let said = format!(
                            "le port {} est pris : ARIA tourne déjà. Ferme l'autre \
                             instance — sous Windows le wrapper npm meurt sans ses \
                             enfants, donc `taskkill /IM aria.exe /F` — puis relance. \
                             ({e})",
                            transport::EVENTS_PORT
                        );
                        // Exited rather than returned as an error: Tauri turns
                        // a failed setup into a panic, and a panic trace reads
                        // as a crash when this is a decision. One line, then
                        // out.
                        tracing::error!("{said}");
                        std::process::exit(1);
                    }
                }

                // The tiles' second origin, which MCP Apps requires to be
                // distinct from the window's — decision 10, proven workable
                // under Tauri by a throwaway prototype.
                //
                // Started even when no tile exists yet, and that is the point:
                // the origin has to be reachable before the window frames
                // anything from it, and `/health` makes "is it up" a question
                // with an answer.
                match aria_core::tiles::bind() {
                    Ok(listener) => {
                        core.tiles().set_serving(true);
                        let tiles = Arc::clone(core.tiles());
                        let watched = Arc::clone(&tiles);
                        tauri::async_runtime::spawn(async move {
                            if let Err(e) = aria_core::tiles::serve_on(listener, tiles).await {
                                tracing::error!("le bac à sable des tuiles s'est arrêté : {e}");
                                watched.set_serving(false);
                            }
                        });
                    }
                    // Said three ways, because a silent one costs an evening:
                    // in the log with the fix in it, on the snapshot so the
                    // window can write it inside the empty panel, and — by
                    // omission — as the absence of a port the CSP names.
                    //
                    // The usual cause on Windows is an instance still running:
                    // `tauri dev` restarts the app, and the npm wrapper dies
                    // without its children.
                    Err(e) => {
                        core.tiles().set_serving(false);
                        tracing::error!(
                            "aucune tuile ne pourra s'afficher : {e}. ARIA tourne-t-elle \
                             déjà ? Sous Windows le wrapper npm meurt sans ses enfants — \
                             `taskkill /IM aria.exe /F` puis relance."
                        );
                    }
                }

                // The main agent. Never fatal, which is the same rule as the
                // voice chain: without a CLI, ARIA still listens, still shows
                // its windows and still runs sessions launched by hand. It just
                // does not answer.
                //
                // Held in `pilot::Running` rather than managed directly,
                // because reopening a conversation **replaces** it: the CLI
                // keeps its own history for the life of its process, so a
                // thread replayed into a session that already held another one
                // would leave both in its context.
                handle.manage(pilot::Running::default());
                let piloted = Arc::clone(&core);
                let piloting = handle.clone();
                tauri::async_runtime::spawn(pilot::start(piloting, piloted));
                Ok(())
            }
        })
        .on_window_event(|window, event| {
            let WindowEvent::CloseRequested { api, .. } = event else {
                return;
            };
            let app = window.app_handle();
            let stay = app
                .state::<Mutex<config::Config>>()
                .lock()
                .unwrap()
                .stay_in_background;

            if !stay {
                // The setting says so, and epic 04 asks for it to be said
                // explicitly on screen: closing the window **stops ARIA**.
                // Running sessions go with it.
                return;
            }

            // Otherwise the X is a route to sleep, not a way out. Quitting is
            // the tray menu, which is a deliberate gesture rather than the
            // reflex of closing a window.
            api.prevent_close();
            shell::sleep(app);
        });

    // `generate_handler!` takes a plain list — an attribute inside it does not
    // compile — so the debug-only bench command needs its own branch.
    #[cfg(debug_assertions)]
    {
        builder = builder.invoke_handler(tauri::generate_handler![
            ipc::connection,
            ipc::snapshot,
            ipc::tile_call,
            ipc::tile_open_link,
            ipc::submit_tile_form,
            ipc::close_tile,
            ipc::toggle_audio,
            ipc::answer_permission,
            ipc::stop_all,
            ipc::provider_info,
            ipc::engines,
            ipc::launch_session,
            ipc::say_to_agent,
            ipc::try_voice,
            ipc::kyutai_voices,
            ipc::kyutai_status,
            ipc::prepare_kyutai,
            ipc::phonon_models,
            ipc::phonon_voices,
            ipc::hf_token,
            ipc::save_hf_token,
            ipc::prepare_phonon,
            ipc::shipped_prompt,
            ipc::set_listening,
            ipc::measure_noise,
            ipc::stop_session,
            ipc::conversations,
            ipc::hooks::hooks_list,
            ipc::hooks::hook_trial_step,
            ipc::hooks::hook_trial_answer,
            ipc::hooks::hook_set_enabled,
            ipc::hooks::hook_delete,
            ipc::hooks::secret_asks,
            ipc::hooks::secret_submit,
            ipc::hooks::secret_cancel,
            ipc::hooks::hook_ask_secret,
            ipc::conversation_thread,
            ipc::conversation_sessions,
            ipc::open_conversation,
            ipc::new_conversation,
            ipc::restart_agent,
            ipc::forget_conversation,
            ipc::audit,
            ipc::show_main,
            ipc::sleep,
            ipc::shell_state,
            ipc::get_config,
            ipc::gpu_card,
            ipc::set_config,
            update::update_check,
            update::update_install,
            ipc::play_scenario,
            ipc::scenarios,
        ]);
    }
    #[cfg(not(debug_assertions))]
    {
        builder = builder.invoke_handler(tauri::generate_handler![
            ipc::connection,
            ipc::snapshot,
            ipc::tile_call,
            ipc::tile_open_link,
            ipc::submit_tile_form,
            ipc::close_tile,
            ipc::toggle_audio,
            ipc::answer_permission,
            ipc::stop_all,
            ipc::provider_info,
            ipc::engines,
            ipc::launch_session,
            ipc::say_to_agent,
            ipc::try_voice,
            ipc::kyutai_voices,
            ipc::kyutai_status,
            ipc::prepare_kyutai,
            ipc::phonon_models,
            ipc::phonon_voices,
            ipc::hf_token,
            ipc::save_hf_token,
            ipc::prepare_phonon,
            ipc::shipped_prompt,
            ipc::set_listening,
            ipc::measure_noise,
            ipc::stop_session,
            ipc::conversations,
            ipc::hooks::hooks_list,
            ipc::hooks::hook_trial_step,
            ipc::hooks::hook_trial_answer,
            ipc::hooks::hook_set_enabled,
            ipc::hooks::hook_delete,
            ipc::hooks::secret_asks,
            ipc::hooks::secret_submit,
            ipc::hooks::secret_cancel,
            ipc::hooks::hook_ask_secret,
            ipc::conversation_thread,
            ipc::conversation_sessions,
            ipc::open_conversation,
            ipc::new_conversation,
            ipc::restart_agent,
            ipc::forget_conversation,
            ipc::audit,
            ipc::show_main,
            ipc::sleep,
            ipc::shell_state,
            ipc::get_config,
            ipc::gpu_card,
            ipc::set_config,
            update::update_check,
            update::update_install,
        ]);
    }

    builder
        .run(tauri::generate_context!())
        .expect("error while running ARIA");
}

#[cfg(test)]
mod window_origin {
    /// The window's dev origin lives in two files, and they have to agree.
    ///
    /// `app/tauri.conf.json` says where the page comes from in development;
    /// `aria_core::tiles::HOST_ORIGINS` says who may frame a tile. If they
    /// drift, every tile in `npm run dev` renders empty — which is exactly
    /// what happened on 2026-11-03, from the other direction, and cost two
    /// rounds of looking at ports.
    ///
    /// Read from the file rather than written out, so the test cannot agree
    /// with a stale copy of it.
    #[test]
    fn the_dev_url_is_an_origin_a_tile_accepts_being_framed_by() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).expect("tauri.conf.json");
        let dev_url = config["build"]["devUrl"]
            .as_str()
            .expect("un devUrl dans tauri.conf.json");

        // Origin = scheme + host + port, which is what `frame-ancestors`
        // matches on. A trailing path would not match and would be a silent
        // mismatch rather than an error.
        let origin = dev_url.trim_end_matches('/');
        assert!(
            aria_core::tiles::HOST_ORIGINS.contains(&origin),
            "« {origin} » n'est pas dans HOST_ORIGINS {:?} : en `npm run dev`, \
             la fenêtre ne pourrait encadrer aucune tuile",
            aria_core::tiles::HOST_ORIGINS
        );
    }

    /// The other direction: the sandbox must never be in that list.
    #[test]
    fn a_tile_is_not_an_origin_a_tile_accepts() {
        assert!(
            !aria_core::tiles::HOST_ORIGINS.contains(&aria_core::tiles::origin().as_str()),
            "une tuile pourrait en encadrer une autre"
        );
    }
}
