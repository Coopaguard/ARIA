//! Updates, from GitHub releases.
//!
//! The window asks whether a newer release exists; at start it installs it
//! at once, later it offers it (`ui/src/lib/update.ts`). The install happens
//! here. `latest.json` is published on the latest GitHub
//! release by `.github/workflows/release.yml`; every package in it is signed,
//! and the plugin refuses one whose signature does not match the public key in
//! `tauri.conf.json`.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_updater::{Update, UpdaterExt};

/// Download progress, in percent, sent to the window.
const EVENT_PROGRESS: &str = "update://progress";

/// ARIA starts without the answer; nobody waits on the network for it.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// The update found by [`update_check`], kept for [`update_install`] — checking
/// again at install time could install another version than the one accepted.
#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<Update>>);

#[derive(Serialize)]
pub struct Available {
    version: String,
    current: String,
    notes: Option<String>,
}

/// A newer version, if there is one.
///
/// `None` as well when the check fails — no network, GitHub unreachable, no
/// release yet: an update is an offer, and a failure is worth a log line,
/// never a window.
#[tauri::command]
pub async fn update_check(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<Option<Available>, ()> {
    let found = match app.updater_builder().timeout(CHECK_TIMEOUT).build() {
        Ok(updater) => updater.check().await,
        Err(error) => Err(error),
    };
    let update = match found {
        Ok(Some(update)) => update,
        Ok(None) => return Ok(None),
        Err(error) => {
            tracing::info!("update check failed: {error}");
            return Ok(None);
        }
    };
    let available = Available {
        version: update.version.clone(),
        current: update.current_version.clone(),
        notes: update.body.clone(),
    };
    *pending.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(update);
    Ok(Some(available))
}

/// Download and install the update [`update_check`] found.
///
/// On Windows the installer is launched and this process ends at once — the
/// installer starts ARIA again. Running sessions end with it; a `llama-server`
/// left behind is reaped at the next start (`sidecar::reap_orphan`).
#[tauri::command]
pub async fn update_install(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<(), String> {
    let update = pending
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .ok_or("aucune mise à jour en attente")?;

    let mut received = 0usize;
    let mut last = u8::MAX;
    update
        .download_and_install(
            |chunk, total| {
                received += chunk;
                let Some(total) = total.filter(|&t| t > 0) else {
                    return;
                };
                let percent = ((received as u64 * 100) / total).min(100) as u8;
                if percent != last {
                    last = percent;
                    let _ = app.emit(EVENT_PROGRESS, percent);
                }
            },
            || {},
        )
        .await
        .map_err(|e| format!("mise à jour impossible : {e}"))?;

    // Not reached on Windows: the process has ended by now.
    app.restart();
}
