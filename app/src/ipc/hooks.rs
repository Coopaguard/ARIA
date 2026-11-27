//! The hooks tab: list, try run by run, switch, delete. See `aria_core::hooks`.
//!
//! The same five operations the voice post has, so neither door can do what
//! the other cannot.

use aria_core::hooks::Row;
use aria_core::hooks::runner::Verdict;
use serde_json::{Value, json};
use tauri::State;

use super::Ipc;

#[tauri::command]
pub fn hooks_list(ipc: State<'_, Ipc>) -> Vec<Row> {
    ipc.core.hook_list()
}

/// One run of the trial. Its result waits for the user's verdict.
#[tauri::command]
pub async fn hook_trial_step(ipc: State<'_, Ipc>, id: String) -> Result<Value, String> {
    let step = ipc.core.hook_trial_step(&id).await.map_err(|e| e.to_string())?;
    let mut out = match &step.ran.verdict {
        Verdict::Nothing => json!({ "result": "nothing" }),
        Verdict::Event(e) => json!({
            "result": "event", "why": e.why, "say": e.say, "level": e.level, "data": e.data,
        }),
        Verdict::Failed(m) => json!({ "result": "failed", "message": m }),
    };
    out["number"] = json!(step.number);
    out["refused"] = json!(step.refused);
    out["elapsed_ms"] = json!(step.ran.elapsed.as_millis() as u64);
    Ok(out)
}

/// The user's verdict on that run: `run_again`, `validated` or `discarded`.
#[tauri::command]
pub fn hook_trial_answer(ipc: State<'_, Ipc>, id: String, right: bool) -> Result<aria_core::hooks::Next, String> {
    ipc.core.hook_trial_answer(&id, right).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn hook_set_enabled(ipc: State<'_, Ipc>, id: String, enabled: bool) -> Result<(), String> {
    ipc.core.hook_set_enabled(&id, enabled).map_err(|e| e.to_string())
}

/// Remove the hook, its folder and its secrets.
#[tauri::command]
pub fn hook_delete(ipc: State<'_, Ipc>, id: String) -> Result<(), String> {
    ipc.core.hook_delete(&id).map_err(|e| e.to_string())
}

/// The secrets sessions are waiting for, for a window that opens late.
#[tauri::command]
pub fn secret_asks(ipc: State<'_, Ipc>) -> Vec<aria_core::hooks::secrets::Ask> {
    ipc.core.secret_asks()
}

/// What the user typed in ARIA's own form, straight to the system store.
///
/// The one place a secret value crosses the IPC. It is not logged, not
/// published, and the session that asked only learns that it was saved.
#[tauri::command]
pub fn secret_submit(ipc: State<'_, Ipc>, id: String, value: String) -> Result<(), String> {
    ipc.core.submit_secret(&id, &value)
}

#[tauri::command]
pub fn secret_cancel(ipc: State<'_, Ipc>, id: String) {
    ipc.core.cancel_secret(&id);
}

/// Type a hook's secret from the tab, or replace it: opens ARIA's form and
/// returns at once, since the form is answered later.
#[tauri::command]
pub fn hook_ask_secret(ipc: State<'_, Ipc>, id: String, name: String) {
    let core = std::sync::Arc::clone(&ipc.core);
    tauri::async_runtime::spawn(async move {
        if let Err(e) = core.ask_secret_from_tab(&id, &name).await {
            tracing::warn!(hook = %id, "secret non demandé : {e}");
        }
    });
}

/// The scenario « Secret d'un hook »: a session asks for a hook's password.
///
/// The real path, end to end: the permission decision a session's
/// `ask_secret` goes through (answered without a card since 2026-11-27), then
/// ARIA's own form, then the system store. Debug builds only.
/// It writes a made-up hook, `scenario-mails`, into `~/.aria/hooks`; deleting
/// it from the tab also removes the secret typed for it.
#[cfg(debug_assertions)]
pub async fn play_secret(ipc: &State<'_, Ipc>) -> Result<(), String> {
    use aria_core::sessions::{ActivityStep, SessionState, StepStatus};

    let core = std::sync::Arc::clone(&ipc.core);
    let root = core
        .hooks()
        .ok_or("les hooks ne sont pas attachés")?
        .root()
        .to_path_buf();
    let dir = root.join("scenario-mails");
    let script = if cfg!(windows) { "check.ps1" } else { "check.sh" };
    let body = if cfg!(windows) {
        "if (-not $env:ARIA_SECRET_IMAP) { [Console]::Error.WriteLine('secret absent'); exit 2 }\n"
    } else {
        "[ -z \"$ARIA_SECRET_IMAP\" ] && { echo 'secret absent' >&2; exit 2; }\nexit 0\n"
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join("hook.toml"),
        format!(
            "name = \"Mails (scénario)\"\nscript = \"{script}\"\nevery = \"5m\"\nsecrets = [\"imap\"]\n"
        ),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(dir.join(script), body).map_err(|e| e.to_string())?;

    let session = core.open_session("Surveille mes mails toutes les cinq minutes", None);
    core.set_session_state(session, SessionState::Executing);
    core.push_step(
        session,
        ActivityStep {
            label: "Write".into(),
            command: Some(dir.join("hook.toml").display().to_string()),
            status: StepStatus::Ok,
        },
    );

    // In the background: the card and the form both wait for the user.
    tauri::async_runtime::spawn(async move {
        let tool = format!("mcp__{}__ask_secret", aria_core::mcp::SERVER_NAME);
        let decision = core.decide_permission(session, &tool, None).await;
        if !matches!(
            decision,
            aria_core::permissions::Decision::Allow
                | aria_core::permissions::Decision::AllowAlways { .. }
        ) {
            core.push_message_delta(session, "Tu as refusé : je ne demande pas le mot de passe.");
            core.finish_session(session, "Refusé");
            return;
        }
        core.set_session_state(session, SessionState::Executing);
        let step = |status| ActivityStep {
            label: "ask_secret".into(),
            command: Some("scenario-mails / imap".into()),
            status,
        };
        core.push_step(session, step(StepStatus::Running));
        let asked = core
            .ask_secret(
                "scenario-mails",
                "imap",
                "Mot de passe IMAP de ta boîte mail (scénario : tape une valeur bidon)",
            )
            .await;
        match asked {
            Ok(true) => {
                core.push_step(session, step(StepStatus::Ok));
                core.push_message_delta(
                    session,
                    "Le mot de passe est rangé dans le trousseau. Je ne l'ai pas vu.",
                );
                core.finish_session(session, "Secret enregistré");
            }
            Ok(false) => {
                core.push_step(session, step(StepStatus::Failed));
                core.push_message_delta(session, "Tu as annulé : rien n'est enregistré.");
                core.finish_session(session, "Annulé");
            }
            Err(e) => core.fail_session(session, e.to_string()),
        }
    });
    Ok(())
}
