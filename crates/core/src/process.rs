//! Every process ARIA starts goes through here, so that none opens a window.
//!
//! The release build has no console of its own (`windows_subsystem =
//! "windows"`). Windows then gives a **new, visible** console to each console
//! program it starts: a black window titled `claude` that kills the agent when
//! it is closed. In debug the child borrows the terminal's console, so the
//! fault cannot be seen in development — hence the test below, which refuses a
//! process started any other way.

use std::ffi::OsStr;

/// `CREATE_NO_WINDOW`: the child gets a console nobody sees. What it starts in
/// turn (MCP servers, `git`) inherits that console, and stays hidden too.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A command whose console window is never shown.
pub fn command(program: impl AsRef<OsStr>) -> std::process::Command {
    #[allow(unused_mut)]
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// The same, for a process awaited by the runtime.
pub fn tokio_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    command(program).into()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                sources(&path, found);
            } else if path.extension().is_some_and(|e| e == "rs") {
                found.push(path);
            }
        }
    }

    #[test]
    fn no_process_starts_outside_this_module() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        for dir in ["app/src", "crates/core/src", "crates/voice/src", "crates/memory/src"] {
            sources(&root.join(dir), &mut files);
        }
        assert!(files.len() > 20, "the sources were not found: {}", files.len());

        let direct = concat!("Command", "::new(");
        let offenders: Vec<_> = files
            .iter()
            .filter(|file| !file.ends_with("crates/core/src/process.rs"))
            .filter(|file| std::fs::read_to_string(file).unwrap().contains(direct))
            .collect();
        assert!(offenders.is_empty(), "use `process::command`, not {direct}…): {offenders:?}");
    }
}
