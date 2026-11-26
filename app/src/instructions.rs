//! What `~/.aria` holds for the agents: the two `AGENTS.md`, and the two
//! folders the agents run in.
//!
//! The same split as [`crate::memory`], and for the same reason in one line of
//! decision 106: **the core receives the paths, it does not discover them.**
//! The path API that knows where `~` is belongs to Tauri, and Tauri lives
//! here. What the core owns is the text a missing file is created with
//! ([`aria_core::agent::instructions`]), because that text is product
//! knowledge and this module is plumbing.
//!
//! # Created, not merely read
//!
//! A setting file that does not exist is a setting nobody knows they have. So
//! a missing file is written with the shipped default, which is what makes
//! "edit `~/.aria/AGENTS.md`" an instruction the user can follow without being
//! told the format first.
//!
//! An **existing** file is never touched — not read, not rewritten, not
//! migrated. The CLI reads it directly, which also means a user who empties it
//! gets an empty append rather than the default back.

use std::path::{Path, PathBuf};

use aria_core::Core;
use aria_core::agent::instructions::{
    DEFAULT_MAIN, DEFAULT_SUBAGENTS, DIR, Home, MAIN_FILE, PREVIOUS_SUBAGENTS, SUBAGENTS_DIR,
};
use tauri::{AppHandle, Manager};

/// Resolve the four paths, creating what is missing, and hand them to the core.
///
/// Failure is per file and never fatal. A home directory that cannot be
/// written to costs the instructions, not the assistant: the core keeps
/// `None`, no flag is pushed, and both posts launch exactly as they did before
/// this existed.
pub fn attach(app: &AppHandle, core: &Core) {
    let home = match app.path().home_dir() {
        Ok(home) => home.join(DIR),
        Err(e) => {
            tracing::error!("dossier personnel introuvable, consignes ignorées : {e}");
            return;
        }
    };

    let subagents = home.join(SUBAGENTS_DIR);
    let found = Home {
        main: ensure(&home.join(MAIN_FILE), DEFAULT_MAIN),
        subagents: ensure(&subagents.join(MAIN_FILE), DEFAULT_SUBAGENTS).inspect(|path| {
            refresh(path, DEFAULT_SUBAGENTS, PREVIOUS_SUBAGENTS);
        }),
        // Decision 108: where each agent runs. Resolved in the same call as
        // the files because it is the same folder and the same reading of
        // `~` — and offered only once it is a directory that exists, since a
        // working directory that cannot be entered fails the launch rather
        // than the path.
        dir: usable(&home),
        subagents_dir: usable(&subagents),
    };
    tracing::info!(
        poste = ?found.main,
        taches = ?found.subagents,
        dossier = ?found.dir,
        dossier_taches = ?found.subagents_dir,
        "consignes et dossiers attachés"
    );
    core.set_home(found);
}

/// The directory's path if an agent can be started in it, after creating it.
///
/// `None` rather than a path that will fail later: a working directory handed
/// to the CLI that is not a directory fails the spawn with an OS error, and
/// the core's fallback is a working assistant. Decision 108.
fn usable(dir: &Path) -> Option<PathBuf> {
    if let Err(e) = std::fs::create_dir_all(dir) {
        tracing::error!("{} inutilisable comme dossier de travail : {e}", dir.display());
        return None;
    }
    if !dir.is_dir() {
        tracing::error!("{} n'est pas un dossier", dir.display());
        return None;
    }
    Some(dir.to_path_buf())
}

/// The file's path if it is usable, after creating it from `default` if it is
/// absent.
///
/// Returns `None` rather than propagating, because every failure here has the
/// same consequence and it is not an error the user can act on at start-up.
/// Replace a file that is still exactly a former shipped text. One the user
/// touched, even by a character, is theirs and stays.
fn refresh(path: &Path, default: &str, previous: &[&str]) {
    let Ok(current) = std::fs::read_to_string(path) else {
        return;
    };
    if !previous.iter().any(|old| current.replace("\r\n", "\n") == *old) {
        return;
    }
    match std::fs::write(path, default) {
        Ok(()) => tracing::info!("consignes mises à jour : {}", path.display()),
        Err(e) => tracing::warn!("{} non mis à jour : {e}", path.display()),
    }
}

fn ensure(path: &Path, default: &str) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    // `is_file` and not `exists`: a directory at that path would otherwise be
    // reported as present and then handed to the CLI, which would fail at
    // launch with something unhelpful.
    if path.exists() {
        tracing::warn!("{} n'est pas un fichier, consignes ignorées", path.display());
        return None;
    }

    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::warn!("{} non créé : {e}", parent.display());
        return None;
    }
    match std::fs::write(path, default) {
        Ok(()) => {
            tracing::info!("consignes créées : {}", path.display());
            Some(path.to_path_buf())
        }
        Err(e) => {
            tracing::warn!("{} non écrit : {e}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing file is created with the shipped text, and the path comes
    /// back — which is the whole mechanism.
    #[test]
    fn a_missing_file_is_created_from_the_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("subagents").join(MAIN_FILE);

        let found = ensure(&path, DEFAULT_SUBAGENTS).expect("créé");

        assert_eq!(found, path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_SUBAGENTS);
    }

    /// A file still exactly as a former version wrote it is brought up to date;
    /// one the user changed is not.
    #[test]
    fn only_an_untouched_former_text_is_refreshed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(MAIN_FILE);

        std::fs::write(&path, PREVIOUS_SUBAGENTS[0]).unwrap();
        refresh(&path, DEFAULT_SUBAGENTS, PREVIOUS_SUBAGENTS);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), DEFAULT_SUBAGENTS);

        let mine = format!("{}\nMa propre consigne.\n", PREVIOUS_SUBAGENTS[0]);
        std::fs::write(&path, &mine).unwrap();
        refresh(&path, DEFAULT_SUBAGENTS, PREVIOUS_SUBAGENTS);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
    }

    /// And an existing one is left exactly as the user wrote it. This is the
    /// test that would catch a "repair the file" idea, which would silently
    /// overwrite instructions the user is relying on.
    #[test]
    fn an_existing_file_is_never_rewritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(MAIN_FILE);
        std::fs::write(&path, "Appelle-moi capitaine.").expect("write");

        let found = ensure(&path, DEFAULT_MAIN).expect("trouvé");

        assert_eq!(found, path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "Appelle-moi capitaine.",
            "le fichier de l'utilisateur a été réécrit"
        );
    }

    /// An emptied file is honoured as empty. The user's way of turning this
    /// off is to clear it, and getting the default back would be ARIA arguing.
    #[test]
    fn an_emptied_file_stays_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(MAIN_FILE);
        std::fs::write(&path, "").expect("write");

        assert_eq!(ensure(&path, DEFAULT_MAIN), Some(path.clone()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
    }

    /// A directory where a file is expected would be handed to the CLI and
    /// fail at launch with something nobody can diagnose.
    #[test]
    fn a_directory_in_the_way_costs_the_file_and_not_the_launch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(MAIN_FILE);
        std::fs::create_dir(&path).expect("mkdir");

        assert_eq!(ensure(&path, DEFAULT_MAIN), None);
    }

    /// A working directory is created rather than merely looked for: on a first
    /// run `~/.aria/subagents` does not exist, and a session handed a path that
    /// is not there fails the spawn with an OS error. Decision 108.
    #[test]
    fn a_working_directory_is_created_when_it_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let subagents = dir.path().join(".aria").join("subagents");
        assert!(!subagents.exists());

        assert_eq!(usable(&subagents), Some(subagents.clone()));
        assert!(subagents.is_dir());
    }

    /// And a file sitting where the folder should be costs the folder, not the
    /// launch: the core falls back, and the agent still starts.
    #[test]
    fn a_file_in_the_way_costs_the_folder_and_not_the_launch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("subagents");
        std::fs::write(&path, "pas un dossier").expect("write");

        assert_eq!(usable(&path), None);
    }
}
