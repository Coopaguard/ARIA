//! The root, and the two file operations everything above is built on.
//!
//! # Why the root is given and not discovered
//!
//! Decision 94 puts the memory under `~/.aria/memories`, and the path API that
//! knows where `~` is belongs to Tauri, which lives in `app/`. The core
//! therefore **receives** its root, exactly as it receives the configuration.
//!
//! That is not tidiness. It makes the confinement rule of decision 90 testable:
//! `..` refused, symlink refused, every write inside one directory — asserted
//! against a throwaway folder instead of a real home directory.
//!
//! # What the single writer does and does not cover
//!
//! Decision 91 asks for one writer so a voice correction and a delegated
//! ingestion cannot collide. Our writes go through [`write_atomic`], so they
//! are ordered and never half-visible. But a delegated session writes with its
//! **own** `Write` tool, outside anything we can lock — which is precisely why
//! decision 92 keys the index on the *state of the files* rather than on our
//! own write calls. The lock orders us; the fingerprint catches everyone.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many previous versions of a page are kept (decision 98).
///
/// Bounded because decision 91 lets a model rewrite at will, and generous
/// because text weighs nothing. Never zero: the version kept is the whole point
/// of not using git (decision 94).
pub const KEEP_VERSIONS: usize = 10;

/// The directory that holds the previous versions, inside the memory folder.
///
/// Inside, and deliberately: decision 94's property is that **one folder is the
/// whole memory**, so copying it backs up the history too.
pub const HISTORY_DIR: &str = ".history";

/// Why a path or a write was refused.
#[derive(Debug, thiserror::Error)]
pub enum PathRefusal {
    /// The path left the root, by `..` or by being absolute.
    #[error("{0:?} is not a relative path inside the memory folder")]
    Escapes(String),
    /// A component resolved outside the root — a symlink, in practice.
    #[error("{0:?} resolves outside the memory folder")]
    ResolvesOutside(String),
    /// Nothing addressable.
    #[error("an empty path addresses no page")]
    Empty,
    /// The filesystem said no.
    #[error("{what}: {source}")]
    Io {
        what: String,
        #[source]
        source: std::io::Error,
    },
}

impl PathRefusal {
    fn io(what: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io { what: what.into(), source }
    }
}

/// A directory nothing may write outside of.
#[derive(Clone, Debug)]
pub struct Root {
    path: PathBuf,
}

impl Root {
    /// Take a directory as the root, creating it if it is not there yet.
    ///
    /// The path is canonicalised once, here, so every later comparison is
    /// against a real path rather than against whatever the caller typed.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PathRefusal> {
        let path = path.as_ref();
        std::fs::create_dir_all(path)
            .map_err(|e| PathRefusal::io(format!("creating {}", path.display()), e))?;
        let path = dunce_canonicalize(path)
            .map_err(|e| PathRefusal::io(format!("resolving {}", path.display()), e))?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Turn a relative path into an absolute one inside the root, or refuse.
    ///
    /// Two checks, and they catch different things. The first reads the path as
    /// written and rejects `..`, an absolute path and a drive prefix. The second
    /// resolves the deepest ancestor that exists and checks it is still inside
    /// — that is the one a symlink cannot slip past, because a symlink only
    /// shows itself once the filesystem has resolved it.
    pub fn resolve(&self, relative: &str) -> Result<PathBuf, PathRefusal> {
        if relative.trim().is_empty() {
            return Err(PathRefusal::Empty);
        }
        let candidate = Path::new(relative);
        for component in candidate.components() {
            match component {
                Component::Normal(part) if !part.is_empty() => {}
                _ => return Err(PathRefusal::Escapes(relative.to_owned())),
            }
        }

        let joined = self.path.join(candidate);
        let mut existing = joined.as_path();
        let resolved = loop {
            match dunce_canonicalize(existing) {
                Ok(real) => break real,
                Err(_) => match existing.parent() {
                    Some(parent) => existing = parent,
                    // We canonicalised the root in `open`, so an ancestor
                    // always resolves; reaching here means the root itself is
                    // gone, and that is worth saying plainly.
                    None => return Err(PathRefusal::Escapes(relative.to_owned())),
                },
            }
        };
        if !resolved.starts_with(&self.path) {
            return Err(PathRefusal::ResolvesOutside(relative.to_owned()));
        }
        Ok(joined)
    }
}

/// `canonicalize` without the `\\?\` prefix Windows adds.
///
/// The prefix is harmless to the filesystem and poisonous to `starts_with`:
/// a canonicalised child carries it, a root built from a plain path does not,
/// and the comparison silently fails. We strip it on both sides instead.
fn dunce_canonicalize(path: &Path) -> std::io::Result<PathBuf> {
    let real = path.canonicalize()?;
    let text = real.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(stripped) => Ok(PathBuf::from(stripped)),
        None => Ok(real),
    }
}

/// Write a file so a reader never sees it half-written.
///
/// Temporary file beside the target, then `rename`. Decision 94 removed git,
/// which made this the **only** protection during a write — and decision 91
/// put writes inside a speaking turn, where decision 80 can cut the turn at any
/// moment. A partially written page is the one outcome that must be impossible.
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), PathRefusal> {
    let parent = path.parent().ok_or(PathRefusal::Empty)?;
    std::fs::create_dir_all(parent)
        .map_err(|e| PathRefusal::io(format!("creating {}", parent.display()), e))?;

    // The name has to be unique per attempt, or two writes racing on the same
    // page would use the same temporary and one would read the other's bytes.
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    let temporary = path.with_file_name(format!(
        "{}.{stamp}.tmp",
        path.file_name().unwrap_or(OsStr::new("page")).to_string_lossy()
    ));

    std::fs::write(&temporary, contents)
        .map_err(|e| PathRefusal::io(format!("writing {}", temporary.display()), e))?;
    // `rename` replaces an existing file on Windows as well as on Unix, which
    // is what makes this one step rather than remove-then-rename.
    std::fs::rename(&temporary, path).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        PathRefusal::io(format!("replacing {}", path.display()), e)
    })
}

/// Keep the current contents of a page as a previous version, then prune.
///
/// Called **before** the new contents are written, so the version kept is the
/// one about to be lost.
pub fn archive(root: &Root, id: &str, previous: &str) -> Result<(), PathRefusal> {
    let dir = root.resolve(&format!("{HISTORY_DIR}/{id}"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| PathRefusal::io(format!("creating {}", dir.display()), e))?;
    write_atomic(&dir.join(format!("{}.md", stamp_now())), previous)?;
    prune(&dir, KEEP_VERSIONS);
    Ok(())
}

/// Move a page's history when the page is renamed (decision 98).
///
/// Without this, ten previous versions stay under a name nothing carries any
/// more — which is exactly the loss the history exists to prevent.
pub fn move_history(root: &Root, from: &str, to: &str) -> Result<(), PathRefusal> {
    let old = root.resolve(&format!("{HISTORY_DIR}/{from}"))?;
    if !old.exists() {
        return Ok(());
    }
    let new = root.resolve(&format!("{HISTORY_DIR}/{to}"))?;
    if let Some(parent) = new.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| PathRefusal::io(format!("creating {}", parent.display()), e))?;
    }
    std::fs::rename(&old, &new)
        .map_err(|e| PathRefusal::io(format!("moving {}", old.display()), e))
}

/// Drop the oldest versions beyond `keep`.
///
/// The stamp sorts lexicographically because it is written biggest unit first,
/// so sorting names is sorting by time — no metadata read, and no dependence on
/// a modification time a backup tool may have rewritten.
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut versions: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    if versions.len() <= keep {
        return;
    }
    versions.sort();
    for stale in &versions[..versions.len() - keep] {
        let _ = std::fs::remove_file(stale);
    }
}

/// `YYYYMMDD-HHMMSS`, in UTC, sortable and readable by a person.
///
/// Readable matters: decision 88's whole argument is that someone opens this
/// folder, and `1794406481.md` tells them nothing. Written out here rather than
/// taken from a date crate, because this is the only place the product needs a
/// calendar.
fn stamp_now() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rest = secs % 86_400;
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// `YYYY-MM-DD`, in UTC.
///
/// The original specification asks for absolute dates, never "last week", and this is the one
/// place that supplies them — to the log, and to the id of a dated page.
pub fn today() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since 1970-01-01 to a civil date. Hinnant's algorithm.
///
/// Short, and tested below against dates taken from elsewhere — a calendar
/// written from memory is the kind of thing that is wrong only in February.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> (tempfile::TempDir, Root) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = Root::open(dir.path()).expect("root");
        (dir, root)
    }

    #[test]
    fn the_calendar_is_right_on_the_dates_that_catch_calendars() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // A leap day, and the day after.
        assert_eq!(civil_from_days(19_782), (2024, 2, 29));
        assert_eq!(civil_from_days(19_783), (2024, 3, 1));
        // 2000 was a leap year, 1900 was not — the rule most implementations
        // get wrong.
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_768), (2026, 11, 11));
    }

    #[test]
    fn a_relative_path_inside_the_root_resolves() {
        let (_dir, root) = root();
        let path = root.resolve("profil/utilisateur.md").expect("resolve");
        assert!(path.starts_with(root.path()));
    }

    #[test]
    fn dot_dot_is_refused() {
        let (_dir, root) = root();
        for attempt in ["../escape.md", "profil/../../escape.md", ".."] {
            assert!(
                matches!(root.resolve(attempt), Err(PathRefusal::Escapes(_))),
                "{attempt} should have been refused"
            );
        }
    }

    #[test]
    fn an_absolute_path_is_refused() {
        let (_dir, root) = root();
        let absolute = if cfg!(windows) { r"C:\windows\system32" } else { "/etc/passwd" };
        assert!(matches!(root.resolve(absolute), Err(PathRefusal::Escapes(_))));
    }

    #[test]
    fn an_empty_path_is_refused() {
        let (_dir, root) = root();
        assert!(matches!(root.resolve("   "), Err(PathRefusal::Empty)));
    }

    /// The check the written path cannot make: a name that looks ordinary and
    /// resolves elsewhere. Skipped where the OS will not let us make one —
    /// Windows needs a privilege for directory symlinks — because a test that
    /// quietly passes by not running is worse than one that says it did not.
    #[test]
    fn a_symlink_out_of_the_root_is_refused() {
        let (_dir, root) = root();
        let outside = tempfile::tempdir().expect("tempdir");
        let link = root.path().join("ailleurs");

        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(outside.path(), &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_dir(outside.path(), &link).is_ok();

        if !made {
            eprintln!("symlink non autorisé par le système : contrôle non joué");
            return;
        }
        assert!(matches!(
            root.resolve("ailleurs/page.md"),
            Err(PathRefusal::ResolvesOutside(_))
        ));
    }

    #[test]
    fn a_write_leaves_no_temporary_behind() {
        let (_dir, root) = root();
        let path = root.resolve("profil/utilisateur.md").expect("resolve");
        write_atomic(&path, "premier").expect("write");
        write_atomic(&path, "second").expect("rewrite");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "second");

        let leftovers: Vec<_> = std::fs::read_dir(path.parent().expect("parent"))
            .expect("read_dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporaires restants : {leftovers:?}");
    }

    #[test]
    fn the_history_keeps_ten_versions_and_drops_the_oldest() {
        let (_dir, root) = root();
        let dir = root.resolve(&format!("{HISTORY_DIR}/profil/utilisateur")).expect("resolve");
        std::fs::create_dir_all(&dir).expect("mkdir");
        // Written directly rather than through `archive`, because `archive`
        // stamps to the second and this needs fifteen distinguishable versions.
        for i in 0..15 {
            std::fs::write(dir.join(format!("2026111{i:02}-000000.md")), format!("v{i}"))
                .expect("write");
        }
        prune(&dir, KEEP_VERSIONS);

        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .expect("read_dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left.len(), KEEP_VERSIONS);
        // The five oldest went, not five at random.
        assert_eq!(left.first().map(String::as_str), Some("202611105-000000.md"));
    }

    #[test]
    fn a_renamed_page_takes_its_history_with_it() {
        let (_dir, root) = root();
        archive(&root, "profil/utilisateur", "ancienne version").expect("archive");
        move_history(&root, "profil/utilisateur", "profil/louis").expect("move");

        let old = root.resolve(&format!("{HISTORY_DIR}/profil/utilisateur")).expect("resolve");
        let new = root.resolve(&format!("{HISTORY_DIR}/profil/louis")).expect("resolve");
        assert!(!old.exists(), "l'ancien dossier d'historique est resté");
        assert_eq!(std::fs::read_dir(&new).expect("read_dir").count(), 1);
    }
}
