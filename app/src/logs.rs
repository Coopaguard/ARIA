//! The log, written to `~/.aria/logs` and kept seven days.
//!
//! # Why a file at all
//!
//! The release build runs without a console (`windows_subsystem = "windows"`
//! in `main.rs`), so until 2026-11-23 an installed ARIA logged to nowhere: a
//! fault on someone else's machine left nothing to read. One file per day,
//! `aria.YYYY-MM-DD.log`, and the console as well in a debug build, where
//! `npm run dev` is still how it is read.
//!
//! # Why `~/.aria` and not `%LOCALAPPDATA%`
//!
//! Asked for by the user. The log is something **someone opens**, like the two
//! `AGENTS.md`, which is what `~/.aria` holds; `%LOCALAPPDATA%/aria` is what one
//! empties to get 3.7 GB back.
//!
//! # Seven days, swept at start
//!
//! By the file's modification time, at every launch, before the first line is
//! written. Not left to the appender's own count of files: a count is not a
//! duration, and ARIA can stay up for days, so a sweep only at rotation would
//! leave the rule to whenever the date happens to change. A file that cannot
//! be removed is skipped — it may be open in an editor — and said in the log.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// How long a day's file is kept.
pub const KEPT: Duration = Duration::from_secs(7 * 24 * 60 * 60);

const PREFIX: &str = "aria";
const SUFFIX: &str = "log";

/// `~/.aria/logs`, or `None` when there is no home to put it in.
pub fn dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".aria").join("logs"))
}

/// Start logging. The returned guard flushes the file when dropped, so it is
/// held for the life of the process.
pub fn init() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let filter = || {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "info,aria=debug,aria_core=debug".into())
    };
    let console = tracing_subscriber::fmt::layer().with_filter(filter());

    let mut swept = Vec::new();
    let file = dir().and_then(|dir| {
        std::fs::create_dir_all(&dir).ok()?;
        swept = sweep(&dir, SystemTime::now());
        tracing_appender::rolling::Builder::new()
            .rotation(tracing_appender::rolling::Rotation::DAILY)
            .filename_prefix(PREFIX)
            .filename_suffix(SUFFIX)
            .build(&dir)
            .ok()
    });

    let Some(appender) = file else {
        tracing_subscriber::registry().with(console).init();
        tracing::warn!("le journal n'a pas de dossier : il ne part qu'à la console");
        return None;
    };
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let file = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_filter(filter());
    tracing_subscriber::registry().with(console).with(file).init();

    // A panic goes to the file too: without a console, it is otherwise the
    // one failure that leaves no trace at all.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panique : {info}");
        previous(info);
    }));

    for (path, error) in swept {
        tracing::warn!("ancien journal non supprimé, {} : {error}", path.display());
    }
    Some(guard)
}

/// Remove this log's files older than [`KEPT`]. Returns those that resisted.
fn sweep(dir: &Path, now: SystemTime) -> Vec<(PathBuf, std::io::Error)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut resisted = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !is_ours(&path) {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if now.duration_since(modified).unwrap_or_default() > KEPT
            && let Err(e) = std::fs::remove_file(&path)
        {
            resisted.push((path, e));
        }
    }
    resisted
}

/// Only `aria.….log`: the sweep never touches a file someone else put there.
fn is_ours(path: &Path) -> bool {
    path.is_file()
        && path.file_name().and_then(|n| n.to_str()).is_some_and(|name| {
            name.starts_with(&format!("{PREFIX}.")) && name.ends_with(&format!(".{SUFFIX}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_our_files_older_than_a_week_are_swept() {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in ["aria.2026-11-01.log", "aria.2026-11-22.log", "notes.txt"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        // Seen from eight days in the future, every file is old.
        let later = SystemTime::now() + Duration::from_secs(8 * 24 * 60 * 60);
        assert!(sweep(dir.path(), later).is_empty());
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(left, vec!["notes.txt"], "seul ce qui n'est pas un journal reste");
    }

    #[test]
    fn a_recent_file_is_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("aria.2026-11-23.log"), "x").unwrap();
        sweep(dir.path(), SystemTime::now() + Duration::from_secs(6 * 24 * 60 * 60));
        assert!(dir.path().join("aria.2026-11-23.log").exists());
    }
}
