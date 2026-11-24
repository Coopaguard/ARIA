//! Windows, tray, global shortcut, and the states.
//!
//! Sleep and main window. The configurable shortcut toggles between the two;
//! closing the main window goes to sleep rather than quitting, and unloads the
//! audio models only.
//!
//! **The floating overview is disabled** (decision 128, 2026-11-20): the user
//! wants the shortcut to bring the main window back, not a detour through the
//! orb. The window, `Overview.vue` and [`show_overview`] stay in the tree,
//! unreachable, so turning it back on is one edge in
//! [`ShellState::after_shortcut`] rather than a rebuild.
//!
//! A pending permission request wakes the main window by itself: a request that
//! never expires would otherwise block forever, invisible, with no window to
//! show it in. Not `Win+C` by default — Windows 11 reassigns it.
//! See epic 04.
//!
//! # The state machine
//!
//! ```text
//!     SLEEPING <--shortcut / X--> MAIN
//! ```
//!
//! # Sleep is not shutdown
//!
//! Only the audio models are unloaded, and only if the setting says so. The
//! core, the sessions and the cadences keep running, because cutting the ear
//! must never interrupt a task. That is also why this module talks to
//! [`aria_core::Core`] through `apply_audio` rather than stopping anything:
//! there is nothing here that can end a session, by construction.

use std::sync::{Arc, Mutex};

use aria_core::Core;
use aria_core::audio::{AudioMode, ModeTrigger};
use aria_core::events::Event;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Runtime, WebviewWindow};

use crate::config::Config;

/// What the user can see of ARIA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellState {
    /// Nothing but a tray icon. The core and the sessions keep running.
    Sleeping,
    /// The floating orb alone, undecorated and always on top.
    Overview,
    /// The full window.
    Main,
}

impl ShellState {
    /// Where the global shortcut goes from here.
    ///
    /// Split out from the window work so the rule can be tested without a
    /// desktop — the Tauri calls cannot be, and a state machine nobody can run
    /// is a state machine that drifts.
    pub fn after_shortcut(self) -> ShellState {
        match self {
            // The main window, not the overview: decision 128.
            ShellState::Sleeping => ShellState::Main,
            // From either visible state, the shortcut sends you to sleep.
            ShellState::Overview | ShellState::Main => ShellState::Sleeping,
        }
    }

    /// Whether any window is on screen in this state.
    pub fn is_visible(self) -> bool {
        self != ShellState::Sleeping
    }
}

/// The shell's own state, beside the core's.
///
/// Deliberately **not** in `aria-core`: which window is showing is the one
/// thing the core has no business knowing, and the crate has to keep building
/// without Tauri. What crosses the line is narrow and one-way — sleep may ask
/// the core to unload the audio models, and the core's event stream may ask the
/// shell to wake up.
pub struct Shell {
    state: Mutex<ShellState>,
    /// The listening mode in force before sleep unloaded the models.
    ///
    /// Without it, waking would turn the microphone back on for someone who had
    /// deliberately switched it off before going to sleep — the shell would be
    /// overruling a choice it never made.
    audio_before_sleep: Mutex<Option<AudioMode>>,
}

impl Shell {
    pub fn new() -> Self {
        Self {
            // The application opens on its main window. Sleep is where it goes,
            // not where it starts: a service nobody can find on first launch is
            // a service nobody turns on.
            state: Mutex::new(ShellState::Main),
            audio_before_sleep: Mutex::new(None),
        }
    }

    pub fn state(&self) -> ShellState {
        *self.state.lock().unwrap()
    }
}

impl Default for Shell {
    fn default() -> Self {
        Self::new()
    }
}

fn window<R: Runtime>(app: &AppHandle<R>, label: &str) -> Option<WebviewWindow<R>> {
    app.get_webview_window(label)
}

/// Show the floating overview, hide everything else.
///
/// Unreachable since decision 128; kept so the overview can come back.
#[allow(dead_code)]
pub fn show_overview<R: Runtime>(app: &AppHandle<R>) {
    let shell = app.state::<Arc<Shell>>();
    wake_audio(app);

    if let Some(main) = window(app, "main") {
        let _ = main.hide();
    }
    if let Some(overview) = window(app, "overview") {
        place_overview(&overview);
        let _ = overview.show();
        // Always on top is declared in the config, but a window that was hidden
        // can come back behind the focused application on Windows. Re-asserting
        // it on show is the cheap fix, and the one the user notices when it is
        // missing.
        let _ = overview.set_always_on_top(true);
    }
    *shell.state.lock().unwrap() = ShellState::Overview;
}

/// Show the main window, hide the overview.
pub fn show_main<R: Runtime>(app: &AppHandle<R>) {
    let shell = app.state::<Arc<Shell>>();
    wake_audio(app);

    if let Some(overview) = window(app, "overview") {
        let _ = overview.hide();
    }
    if let Some(main) = window(app, "main") {
        let _ = main.show();
        let _ = main.unminimize();
        let _ = main.set_focus();
    }
    *shell.state.lock().unwrap() = ShellState::Main;
}

/// Hide every window. The core keeps working.
pub fn sleep<R: Runtime>(app: &AppHandle<R>) {
    let shell = app.state::<Arc<Shell>>();

    for label in ["main", "overview"] {
        if let Some(w) = window(app, label) {
            let _ = w.hide();
        }
    }

    // Only the audio models, and only if the setting says so.
    let unload = app
        .state::<Mutex<Config>>()
        .lock()
        .unwrap()
        .unload_audio_on_sleep;
    if unload {
        let core = app.state::<Arc<Core>>();
        let mode = core.audio_mode();
        if mode != AudioMode::Off {
            *shell.audio_before_sleep.lock().unwrap() = Some(mode);
            // The interface trigger is the only one that may reach off, and
            // this is the interface acting.
            let _ = core.apply_audio(ModeTrigger::Interface);
        }
    }

    *shell.state.lock().unwrap() = ShellState::Sleeping;
}

/// Put listening back the way it was before sleep, if sleep had cut it.
///
/// Back to the resting mode, never to active: the name has to be said again.
/// And if the microphone was already off when sleep came, nothing is restored —
/// that was a choice, not a side effect.
fn wake_audio<R: Runtime>(app: &AppHandle<R>) {
    let shell = app.state::<Arc<Shell>>();
    let remembered = shell.audio_before_sleep.lock().unwrap().take();
    let Some(mode) = remembered else { return };
    if mode == AudioMode::Off {
        return;
    }
    let core = app.state::<Arc<Core>>();
    if core.audio_mode() == AudioMode::Off {
        // Costs about 3.8 s of model loading, measured in T1. The interface
        // shows it through the `audio.mode` event this publishes.
        let _ = core.apply_audio(ModeTrigger::Interface);
    }
}

/// What the global shortcut does: a toggle, nothing more.
pub fn toggle<R: Runtime>(app: &AppHandle<R>) {
    let from = app.state::<Arc<Shell>>().state();
    // Open but buried under other windows, or minimised: the shortcut brings it
    // to the front. Hiding a window nobody could see would read as a dead key.
    if from == ShellState::Main {
        if let Some(main) = window(app, "main") {
            let buried = !main.is_focused().unwrap_or(true)
                || main.is_minimized().unwrap_or(false);
            if buried {
                show_main(app);
                return;
            }
        }
    }
    tracing::debug!("shortcut: {from:?} -> {:?}", from.after_shortcut());
    match from.after_shortcut() {
        ShellState::Main => show_main(app),
        ShellState::Sleeping => sleep(app),
        // `after_shortcut` no longer returns this (decision 128); matching it
        // exhaustively means it has to be thought about here if it comes back.
        ShellState::Overview => show_overview(app),
    }
}

/// Register the global shortcut, replacing whatever was registered before.
///
/// Returns the error rather than logging it, because the settings screen has to
/// be able to say *this combination is taken* instead of silently keeping the
/// old one while showing the new.
pub fn register_shortcut<R: Runtime>(app: &AppHandle<R>, accelerator: &str) -> anyhow::Result<()> {
    use tauri_plugin_global_shortcut::GlobalShortcutExt;

    let shortcut: tauri_plugin_global_shortcut::Shortcut = accelerator
        .parse()
        .map_err(|e| anyhow::anyhow!("« {accelerator} » n'est pas un raccourci valide : {e}"))?;

    let manager = app.global_shortcut();
    // Unregister everything rather than the previous accelerator: the previous
    // one is only known to the config, and if those two ever disagree the
    // orphan is a shortcut nobody can get rid of.
    let _ = manager.unregister_all();
    manager
        .register(shortcut)
        .map_err(|e| anyhow::anyhow!("« {accelerator} » est déjà pris : {e}"))?;
    tracing::info!("global shortcut: {accelerator}");
    Ok(())
}

/// Put the overview in the bottom-right corner of the work area.
///
/// The work area, not the screen: on Windows the taskbar would otherwise sit on
/// top of it. Positioned every time it is shown, so unplugging a monitor does
/// not leave it off-screen with no way to get it back — the one failure mode of
/// a window with no decorations.
fn place_overview<R: Runtime>(overview: &WebviewWindow<R>) {
    let Ok(Some(monitor)) = overview.current_monitor() else {
        return;
    };
    let screen = monitor.size();
    let Ok(size) = overview.outer_size() else {
        return;
    };
    let margin = 24u32;
    let x = screen.width.saturating_sub(size.width + margin) as i32;
    // A guess at the taskbar's height, because Tauri does not expose the work
    // area. Wrong by a few pixels on an unusual setup, which beats being hidden
    // behind the taskbar on a normal one.
    let y = screen.height.saturating_sub(size.height + margin + 48) as i32;
    let _ = overview.set_position(tauri::PhysicalPosition::new(x, y));
}

/// The tray icon: what is left to click once every window is hidden.
///
/// Without it, sleep would be indistinguishable from having quit — and the
/// setting that turns sleeping off says, in so many words, that the difference
/// matters.
pub fn install_tray<R: Runtime>(app: &AppHandle<R>) -> anyhow::Result<()> {
    let main = MenuItem::with_id(app, "main", "Fenêtre principale", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quitter ARIA", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&main, &quit])?;

    TrayIconBuilder::with_id("aria")
        .icon(app.default_window_icon().cloned().ok_or_else(|| {
            anyhow::anyhow!("no default window icon; `bundle.icon` is what provides it")
        })?)
        .tooltip("ARIA")
        .menu(&menu)
        // Left click toggles the main window, which is the gesture people try
        // first.
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "main" => show_main(app),
            "quit" => {
                // The only deliberate way out. Sessions in flight stop with the
                // process, which is why this is a menu entry and not the X on a
                // window.
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// Wake the main window when a permission request arrives with nothing showing.
///
/// This is the hole epic 04 names: a request **never expires** and lives under
/// its session in the right-hand menu — but asleep there is no menu, so it
/// would block forever, invisible. Watching the core's own stream is how the
/// shell learns about it without the core knowing a window exists.
pub fn watch_for_permission_requests<R: Runtime>(app: AppHandle<R>, core: Arc<Core>) {
    tauri::async_runtime::spawn(async move {
        let mut events = core.broker().subscribe();
        loop {
            match events.recv().await {
                Ok(envelope) => {
                    if !matches!(envelope.event, Event::PermissionRequest { .. }) {
                        continue;
                    }
                    // Something is already on screen, so the request has
                    // somewhere to appear and nothing needs waking.
                    if app.state::<Arc<Shell>>().state().is_visible() {
                        continue;
                    }
                    tracing::info!("a permission request is waking the main window");
                    // Window work belongs on the main thread.
                    let moved = app.clone();
                    let _ = app.run_on_main_thread(move || show_main(&moved));
                }
                // Lagging here only means requests were published faster than
                // this loop read them; the next one still wakes the window, and
                // the pending list is complete either way.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shortcut_wakes_to_the_main_window_never_to_the_overview() {
        // Decision 128 reverses decision 9: the user wants the main window
        // back, not the floating orb.
        assert_eq!(ShellState::Sleeping.after_shortcut(), ShellState::Main);
    }

    #[test]
    fn the_shortcut_is_a_toggle() {
        assert_eq!(ShellState::Main.after_shortcut(), ShellState::Sleeping);
        assert_eq!(ShellState::Overview.after_shortcut(), ShellState::Sleeping);
    }

    #[test]
    fn pressing_twice_from_anywhere_visible_lands_on_the_main_window() {
        for from in [ShellState::Main, ShellState::Overview] {
            assert_eq!(
                from.after_shortcut().after_shortcut(),
                ShellState::Main,
                "the second press has to bring something back, or the shortcut \
                 reads as broken"
            );
        }
    }

    #[test]
    fn sleeping_is_the_only_state_with_nothing_on_screen() {
        assert!(!ShellState::Sleeping.is_visible());
        assert!(ShellState::Overview.is_visible());
        assert!(ShellState::Main.is_visible());
    }

    #[test]
    fn the_application_does_not_start_asleep() {
        // A background service nobody can find on first launch is a background
        // service nobody turns on.
        assert_eq!(Shell::new().state(), ShellState::Main);
    }
}
