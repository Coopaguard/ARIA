//! Asking the user for a hook's secret, without any model seeing it.
//!
//! A session calls `ask_secret(hook, name, label)`. The core checks that the
//! hook declares that secret, publishes the **ask** — never a value — and
//! waits. The window draws a form of its own, outside any tile: a tile is
//! written by the model, and its script could carry what is typed out through
//! `ui/open-link`. The value goes from that form to [`Core::submit_secret`],
//! then straight into the store. The session is told « enregistré », nothing
//! more.
//!
//! Like a permission request, an ask does not expire. A session that ended
//! meanwhile loses nothing: the value is stored all the same.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tokio::sync::oneshot;

use super::{HookError, NoSecrets, Secrets, manifest};
use crate::Core;
use crate::events::Event;

/// Longest label the form shows.
pub const LABEL_MAX: usize = 120;
/// Longest value accepted. A password, a token: not a file.
pub const VALUE_MAX: usize = 4096;

/// What the form shows. No value, ever.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Ask {
    pub id: String,
    pub hook: String,
    /// The hook's name, as the user knows it.
    pub hook_name: String,
    pub name: String,
    /// What to type, in the session's words: « Mot de passe de la boîte mail ».
    pub label: String,
}

#[derive(Default)]
pub struct Desk {
    pending: Mutex<HashMap<String, (Ask, Option<oneshot::Sender<bool>>)>>,
}

impl Core {
    /// Hand the core the system store. `app/` owns it; the core never
    /// depends on a keyring.
    pub fn attach_secrets(&self, secrets: Arc<dyn Secrets>) {
        *self.secrets.lock().unwrap() = Some(secrets);
    }

    /// The store, or one that holds nothing.
    pub fn secrets(&self) -> Arc<dyn Secrets> {
        self.secrets
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| Arc::new(NoSecrets))
    }

    /// Ask the user for a secret a hook declares. **Waits until answered.**
    ///
    /// `Ok(true)` when it was stored, `Ok(false)` when the user cancelled.
    pub async fn ask_secret(&self, hook: &str, name: &str, label: &str) -> Result<bool, HookError> {
        let found = self
            .hooks()
            .and_then(|hooks| super::discover(hooks.root()).into_iter().find(|h| h.id == hook))
            .ok_or_else(|| HookError::Unknown(hook.into()))?;
        let manifest = found
            .manifest
            .map_err(|e| HookError::Broken(hook.into(), e))?;
        if !manifest::is_secret_name(name) || !manifest.secrets.iter().any(|s| s == name) {
            return Err(HookError::Secret(format!(
                "le hook « {hook} » ne déclare pas le secret « {name} » : ajoute-le à `secrets` \
                 dans hook.toml d'abord"
            )));
        }
        let label = label.trim();
        if label.is_empty() || label.chars().count() > LABEL_MAX {
            return Err(HookError::Secret(format!(
                "le libellé doit dire quoi saisir, en {LABEL_MAX} caractères au plus"
            )));
        }
        let ask = Ask {
            id: uuid::Uuid::new_v4().to_string(),
            hook: hook.into(),
            hook_name: manifest.name,
            name: name.into(),
            label: label.into(),
        };
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.secret_desk.pending.lock().unwrap();
            if pending.values().any(|(a, _)| a.hook == ask.hook && a.name == ask.name) {
                return Err(HookError::Secret(format!(
                    "le secret « {name} » est déjà demandé à l'utilisateur"
                )));
            }
            pending.insert(ask.id.clone(), (ask.clone(), Some(tx)));
        }
        tracing::info!(hook, name, "secret demandé à l'utilisateur");
        self.broker.publish(Event::SecretAsked { ask });
        // A dropped sender is a desk that went away: nothing was stored.
        Ok(rx.await.unwrap_or(false))
    }

    /// What the form has to show, for a window that opens late.
    pub fn secret_asks(&self) -> Vec<Ask> {
        self.secret_desk
            .pending
            .lock()
            .unwrap()
            .values()
            .map(|(a, _)| a.clone())
            .collect()
    }

    /// The user typed it. Stored, then the ask is closed. The value is
    /// neither logged nor published.
    pub fn submit_secret(&self, id: &str, value: &str) -> Result<(), String> {
        if value.is_empty() {
            return Err("Rien n'a été saisi.".into());
        }
        if value.chars().count() > VALUE_MAX {
            return Err(format!("{VALUE_MAX} caractères au plus."));
        }
        let ask = self
            .secret_desk
            .pending
            .lock()
            .unwrap()
            .get(id)
            .map(|(a, _)| a.clone())
            .ok_or_else(|| "Cette demande n'existe plus.".to_string())?;
        // Stored before the ask is closed: a store that refuses leaves the
        // form on screen, with the reason.
        self.secrets().set(&ask.hook, &ask.name, value)?;
        self.close_secret(id, true);
        Ok(())
    }

    pub fn cancel_secret(&self, id: &str) {
        self.close_secret(id, false);
    }

    fn close_secret(&self, id: &str, saved: bool) {
        let Some((ask, tx)) = self.secret_desk.pending.lock().unwrap().remove(id) else {
            return;
        };
        if let Some(tx) = tx {
            let _ = tx.send(saved);
        }
        tracing::info!(hook = %ask.hook, name = %ask.name, saved, "demande de secret close");
        self.broker.publish(Event::SecretClosed { id: id.into(), saved });
        self.broker.publish(Event::HooksChanged);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::hooks::Hooks;

    /// The store, as a map. What the tests stand in for the keyring with.
    #[derive(Default)]
    pub struct Memory(pub Mutex<HashMap<(String, String), String>>);

    impl Secrets for Memory {
        fn get(&self, hook: &str, name: &str) -> Option<String> {
            self.0.lock().unwrap().get(&(hook.into(), name.into())).cloned()
        }
        fn set(&self, hook: &str, name: &str, value: &str) -> Result<(), String> {
            self.0.lock().unwrap().insert((hook.into(), name.into()), value.into());
            Ok(())
        }
        fn delete(&self, hook: &str, name: &str) {
            self.0.lock().unwrap().remove(&(hook.into(), name.into()));
        }
    }

    fn core_with_hook(root: &std::path::Path) -> (Arc<Core>, Arc<Memory>) {
        let dir = root.join("mails");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("hook.toml"),
            "name = \"Mails\"\nscript = \"c.sh\"\nevery = \"5m\"\nsecrets = [\"imap\"]\n",
        )
        .unwrap();
        std::fs::write(dir.join("c.sh"), "").unwrap();
        let core = Core::new();
        core.attach_hooks(Arc::new(Hooks::new(root)));
        let store = Arc::new(Memory::default());
        core.attach_secrets(store.clone());
        (core, store)
    }

    /// The whole way: asked, shown without a value, typed, stored, answered.
    #[tokio::test]
    async fn a_secret_goes_from_the_form_to_the_store_and_nowhere_else() {
        let root = tempfile::tempdir().unwrap();
        let (core, store) = core_with_hook(root.path());
        let mut events = core.broker().subscribe();
        let asking = {
            let core = Arc::clone(&core);
            tokio::spawn(async move { core.ask_secret("mails", "imap", "Mot de passe IMAP").await })
        };
        while core.secret_asks().is_empty() {
            tokio::task::yield_now().await;
        }
        let ask = core.secret_asks().remove(0);
        assert_eq!((ask.hook_name.as_str(), ask.label.as_str()), ("Mails", "Mot de passe IMAP"));
        core.submit_secret(&ask.id, "hunter2-long").unwrap();
        assert_eq!(asking.await.unwrap(), Ok(true));
        assert_eq!(store.get("mails", "imap").as_deref(), Some("hunter2-long"));
        assert!(core.secret_asks().is_empty());
        // Nothing published carries the value.
        while let Ok(envelope) = events.try_recv() {
            let text = serde_json::to_string(&envelope.event).unwrap();
            assert!(!text.contains("hunter2"), "{text}");
        }
    }

    #[tokio::test]
    async fn only_a_declared_secret_can_be_asked_for() {
        let root = tempfile::tempdir().unwrap();
        let (core, _) = core_with_hook(root.path());
        assert!(matches!(core.ask_secret("mails", "banque", "Code").await, Err(HookError::Secret(_))));
        assert!(matches!(core.ask_secret("absent", "imap", "Code").await, Err(HookError::Unknown(_))));
        assert!(matches!(core.ask_secret("mails", "imap", "  ").await, Err(HookError::Secret(_))));
    }

    #[tokio::test]
    async fn a_cancelled_ask_stores_nothing() {
        let root = tempfile::tempdir().unwrap();
        let (core, store) = core_with_hook(root.path());
        let asking = {
            let core = Arc::clone(&core);
            tokio::spawn(async move { core.ask_secret("mails", "imap", "Mot de passe").await })
        };
        while core.secret_asks().is_empty() {
            tokio::task::yield_now().await;
        }
        let id = core.secret_asks()[0].id.clone();
        assert!(core.submit_secret(&id, "").is_err(), "an empty value is not an answer");
        core.cancel_secret(&id);
        assert_eq!(asking.await.unwrap(), Ok(false));
        assert_eq!(store.get("mails", "imap"), None);
    }

    /// The tab and `hooks_list` see which secrets are stored, never what.
    #[test]
    fn the_list_names_secrets_and_never_shows_them() {
        let root = tempfile::tempdir().unwrap();
        let (core, store) = core_with_hook(root.path());
        let keys = |core: &Core| core.hook_list()[0].secrets.clone();
        assert_eq!(keys(&core), [crate::hooks::SecretKey { name: "imap".into(), stored: false }]);
        store.set("mails", "imap", "hunter2-long").unwrap();
        assert!(keys(&core)[0].stored);
        let listed = serde_json::to_string(&core.hook_list()).unwrap();
        assert!(!listed.contains("hunter2"), "{listed}");
    }

    /// Reading the hooks and asking for a secret raise no card; the rest do.
    #[tokio::test]
    async fn listing_and_asking_raise_no_card() {
        let naming = crate::providers::McpToolNaming::default();
        let core = Core::new();
        let session = core.open_session("hooks", None);
        for tool in ["mcp__aria__hooks_list", "mcp__aria__ask_secret"] {
            assert!(crate::mcp::hooks::is_unasked(naming, tool));
            assert_eq!(
                core.decide_permission(session, tool, None).await,
                crate::permissions::Decision::Allow,
                "{tool}"
            );
        }
        for tool in ["mcp__aria__hook_trial", "mcp__aria__hook_delete", "mcp__aria__hook_set_enabled"] {
            assert!(!crate::mcp::hooks::is_unasked(naming, tool), "{tool}");
        }
        assert!(!crate::mcp::hooks::is_unasked(naming, "mcp__other__hooks_list"));
    }

    /// A deleted hook takes its secrets with it.
    #[test]
    fn deleting_a_hook_deletes_its_secrets() {
        let root = tempfile::tempdir().unwrap();
        let (core, store) = core_with_hook(root.path());
        store.set("mails", "imap", "x").unwrap();
        core.hook_delete("mails").unwrap();
        assert_eq!(store.get("mails", "imap"), None);
    }
}
