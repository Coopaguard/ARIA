//! `~/.aria/hooks`, resolved here and handed to the core, and the system store
//! its secrets live in. See `aria_core::hooks`.
//!
//! The folder is the user's, like the memory: they open it, sessions write
//! into it, and the core validates what runs.

use std::sync::Arc;

use aria_core::Core;
use aria_core::agent::instructions::DIR;
use aria_core::hooks::{Hooks, Secrets};
use tauri::{AppHandle, Manager};

/// The folder's name under `~/.aria`.
pub const HOOKS_DIR: &str = "hooks";

/// The service every hook secret is filed under in the system store. The
/// account is `<hook>/<name>`.
const SERVICE: &str = "ARIA hooks";

/// Resolve the folder, create it, and hand it and the store to the core.
/// After `history::attach`: the validations are read from `aria.db`.
pub fn attach(app: &AppHandle, core: &Core) {
    core.attach_secrets(Arc::new(Keyring));
    let root = match app.path().home_dir() {
        Ok(home) => home.join(DIR).join(HOOKS_DIR),
        Err(e) => {
            tracing::error!("dossier personnel introuvable, hooks ignorés : {e}");
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&root) {
        tracing::error!("{} n'a pas pu être créé, hooks ignorés : {e}", root.display());
        return;
    }
    core.attach_hooks(Arc::new(Hooks::new(root)));

    if let Some(subagents) = core.home().subagents_dir {
        ship_skill(&subagents.join(aria_core::hooks::SKILL_PATH));
    }
}

/// The skill sessions read before writing a hook.
///
/// Written when absent, and **rewritten when ARIA's own copy is still the one
/// on disk**: beside it, `.shipped` holds the fingerprint of what was last
/// written. A file whose fingerprint differs was edited by the user and is
/// theirs; it is left alone.
fn ship_skill(skill: &std::path::Path) {
    let hash = aria_core::hooks::sha256_hex;
    let marker = skill.with_file_name(".shipped");
    let wanted = aria_core::hooks::SKILL;
    let ours = match std::fs::read(skill) {
        Err(_) => true,
        Ok(current) if current == wanted.as_bytes() => return,
        Ok(current) => std::fs::read_to_string(&marker).is_ok_and(|m| m.trim() == hash(&current)),
    };
    if !ours {
        tracing::info!("skill des hooks modifié par l'utilisateur : laissé tel quel");
        return;
    }
    let written = skill
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(skill, wanted))
        .and_then(|()| std::fs::write(&marker, hash(wanted.as_bytes())));
    match written {
        Ok(()) => tracing::info!("skill des hooks écrit"),
        Err(e) => tracing::warn!("le skill des hooks n'a pas pu être écrit : {e}"),
    }
}

/// The platform's own store: Credential Manager on Windows, Secret Service
/// on Linux, Keychain on macOS. Nothing is ever written to a file: without a
/// store, `set` refuses and says so.
struct Keyring;

fn entry(hook: &str, name: &str) -> keyring::Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, &format!("{hook}/{name}"))
}

impl Secrets for Keyring {
    fn get(&self, hook: &str, name: &str) -> Option<String> {
        match entry(hook, name).and_then(|e| e.get_password()) {
            Ok(value) => Some(value),
            Err(keyring::Error::NoEntry) => None,
            Err(e) => {
                tracing::warn!(hook, name, "le trousseau n'a pas rendu le secret : {e}");
                None
            }
        }
    }

    fn set(&self, hook: &str, name: &str, value: &str) -> Result<(), String> {
        entry(hook, name)
            .and_then(|e| e.set_password(value))
            .map_err(|e| match e {
                keyring::Error::NoDefaultStore => {
                    "Aucun trousseau système n'est disponible : le secret n'est pas enregistré."
                        .to_string()
                }
                other => format!("Le trousseau a refusé le secret : {other}"),
            })
    }

    fn delete(&self, hook: &str, name: &str) {
        match entry(hook, name).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(e) => tracing::warn!(hook, name, "le secret n'a pas pu être effacé : {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Absent: written. Ours and outdated: rewritten. Edited: left alone.
    #[test]
    fn the_skill_is_updated_unless_the_user_edited_it() {
        let dir = tempfile::tempdir().unwrap();
        let skill = dir.path().join("aria-hooks").join("SKILL.md");
        ship_skill(&skill);
        assert_eq!(std::fs::read_to_string(&skill).unwrap(), aria_core::hooks::SKILL);

        // An older version of ours, with its fingerprint beside it.
        let old = "---\nname: aria-hooks\n---\nancienne version\n";
        std::fs::write(&skill, old).unwrap();
        let hash = aria_core::hooks::sha256_hex(old.as_bytes());
        std::fs::write(skill.with_file_name(".shipped"), hash).unwrap();
        ship_skill(&skill);
        assert_eq!(std::fs::read_to_string(&skill).unwrap(), aria_core::hooks::SKILL);

        // Edited by the user: theirs.
        let edited = format!("{}\nMa règle à moi.\n", aria_core::hooks::SKILL);
        std::fs::write(&skill, &edited).unwrap();
        ship_skill(&skill);
        assert_eq!(std::fs::read_to_string(&skill).unwrap(), edited);
    }

    /// The real store, with a made-up hook. Ignored: it writes to the user's
    /// Credential Manager, then removes what it wrote.
    #[test]
    #[ignore]
    fn the_system_store_keeps_and_forgets_a_secret() {
        let store = Keyring;
        let (hook, name) = ("essai-aria-test", "jeton");
        store.delete(hook, name);
        assert_eq!(store.get(hook, name), None);
        store.set(hook, name, "valeur-d-essai-é").unwrap();
        assert_eq!(store.get(hook, name).as_deref(), Some("valeur-d-essai-é"));
        store.delete(hook, name);
        assert_eq!(store.get(hook, name), None);
    }
}
