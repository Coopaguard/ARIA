//! The `llama-server` process: started, waited for, killed.
//!
//! **Ready means the model is in memory**, not that the process exists:
//! `llama-server` binds its port before loading and answers `/health` with 503
//! until the weights are read. Waiting on that is what lets the loading bar end
//! when the model can actually answer — the user's rule for this epic, stated
//! on 2026-11-18: no lazy loading.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use super::LocalConfig;
use super::client::Client;

/// How long a model may take to load from disk. Generous: 5 GB from a cold
/// disk on a laptop is tens of seconds, and a ceiling that turns slowness into
/// failure is worse than a long wait under a loading bar.
const LOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// How much of the server's output is kept for an error message.
const TAIL: usize = 4000;

/// A running `llama-server`. Killed when dropped.
pub struct Sidecar {
    child: Child,
    output: Arc<Mutex<String>>,
}

impl Sidecar {
    /// Start it and wait until the model is loaded.
    pub async fn start(config: &LocalConfig) -> Result<Self> {
        let client = Client::new(config.port);
        if reap_orphan(&config.server) {
            // Give the port back before asking who holds it.
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if client.healthy().await {
            // Something already answers on our port. Using it would mean
            // talking to a model nobody here chose — said, rather than tried.
            bail!(
                "un serveur répond déjà sur le port {} : ARIA tourne-t-elle deux fois, ou un \
                 llama-server lancé à la main l'occupe-t-il ?",
                config.port
            );
        }

        let args = arguments(config);
        tracing::info!(
            serveur = %config.server.display(),
            "lancement de llama-server : {}",
            args.join(" ")
        );
        let mut child = Command::new(&config.server)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("impossible de lancer {}", config.server.display()))?;
        if let Some(pid) = child.id() {
            let _ = std::fs::write(pid_file(), pid.to_string());
        }

        let output = Arc::new(Mutex::new(String::new()));
        let log = config
            .log_file
            .as_ref()
            .and_then(|path| std::fs::File::create(path).ok())
            .map(|file| Arc::new(std::sync::Mutex::new(file)));
        drain(child.stdout.take(), Arc::clone(&output), log.clone());
        drain(child.stderr.take(), Arc::clone(&output), log);

        let mut sidecar = Self { child, output };
        sidecar.wait_ready(&client).await?;
        Ok(sidecar)
    }

    async fn wait_ready(&mut self, client: &Client) -> Result<()> {
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait()? {
                bail!(
                    "llama-server s'est arrêté ({status}) pendant le chargement :\n{}",
                    self.tail().await
                );
            }
            if client.healthy().await {
                tracing::info!(
                    ms = started.elapsed().as_millis(),
                    "llama-server : modèle chargé"
                );
                return Ok(());
            }
            if started.elapsed() > LOAD_TIMEOUT {
                bail!(
                    "llama-server n'a pas chargé son modèle en {LOAD_TIMEOUT:?} :\n{}",
                    self.tail().await
                );
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// The end of what the server wrote, for an error message.
    pub async fn tail(&self) -> String {
        self.output.lock().await.clone()
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // `kill_on_drop` does the killing; this only says nothing is left.
        let _ = std::fs::remove_file(pid_file());
    }
}

/// Where the running server's PID is written.
///
/// Because `kill_on_drop` only runs when ARIA closes **cleanly**. Killed from
/// the task manager, or by `tauri dev` restarting it, ARIA leaves its
/// `llama-server` behind — Windows does not end a child with its parent — with
/// five gigabytes and the port still held. Measured on 2026-11-18: the next
/// start then refused the local model, « un serveur répond déjà ».
fn pid_file() -> PathBuf {
    std::env::temp_dir().join("aria-llama-server.pid")
}

/// End the server a previous ARIA left running, if there is one.
///
/// Only a process with that PID **and** that executable name: a PID is reused
/// by the system, and killing whatever now carries it would be the one mistake
/// worse than the orphan. Returns whether something was ended.
fn reap_orphan(server: &Path) -> bool {
    let Ok(pid) = std::fs::read_to_string(pid_file()) else {
        return false;
    };
    let _ = std::fs::remove_file(pid_file());
    let Ok(pid) = pid.trim().parse::<u32>() else {
        return false;
    };
    let image = server
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "llama-server.exe".into());
    if !cfg!(windows) {
        return false;
    }
    let ended = std::process::Command::new("taskkill")
        .args(["/F", "/FI", &format!("PID eq {pid}"), "/FI", &format!("IMAGENAME eq {image}")])
        .output()
        .map(|out| out.status.success() && String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()))
        .unwrap_or(false);
    if ended {
        tracing::warn!(pid, "un llama-server laissé par une session précédente a été arrêté");
    }
    ended
}

/// The command line, apart from the spawning so it can be asserted on.
pub fn arguments(config: &LocalConfig) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-m".into(),
        config.model_path.display().to_string(),
        "--mmproj".into(),
        config.mmproj_path.display().to_string(),
        // Without it `llama-server` emits no tool call at all, on any model.
        // The POC's first lesson.
        "--jinja".into(),
        // Gemma 4 thinks by default, invisibly: measured on 2026-11-18 at
        // 2956 ms before the first word of « sept fois huit », against 20 ms
        // for Voxtral, which does not. The same lesson as the CLI's
        // `MAX_THINKING_TOKENS=0` — a voice cannot wait for a monologue.
        "--reasoning".into(),
        "off".into(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        config.port.to_string(),
        // Two slots, and the context is shared between them — hence doubled.
        // Slot 0 holds the conversation, so every turn lands where the last
        // one left its cache and the prompt prefix is reused rather than
        // recomputed; slot 1 takes the transcripts, which would otherwise
        // evict that prefix after every heard turn. See `engine`.
        "-c".into(),
        (config.model.context * 2).to_string(),
        "--parallel".into(),
        "2".into(),
    ];
    args.extend(config.extra_args.iter().cloned());
    args
}

/// Read a pipe line by line, keeping the tail and copying to the log file.
///
/// Drained whatever happens: a full pipe blocks the child, and a blocked
/// `llama-server` looks exactly like a slow one.
fn drain(
    pipe: Option<impl AsyncRead + Unpin + Send + 'static>,
    output: Arc<Mutex<String>>,
    log: Option<Arc<std::sync::Mutex<std::fs::File>>>,
) {
    let Some(pipe) = pipe else {
        return;
    };
    tokio::spawn(async move {
        let mut lines = BufReader::new(pipe).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(log) = &log {
                use std::io::Write;
                let _ = writeln!(log.lock().unwrap(), "{line}");
            }
            let mut tail = output.lock().await;
            tail.push_str(&line);
            tail.push('\n');
            if tail.len() > TAIL * 2 {
                let cut = tail.len() - TAIL;
                let cut = (cut..tail.len())
                    .find(|&i| tail.is_char_boundary(i))
                    .unwrap_or(cut);
                tail.drain(..cut);
            }
        }
    });
}

/// Where `llama-server` is, if it is anywhere we know to look.
///
/// In order: what the settings say, `ARIA_LLAMA_SERVER`, the `PATH`, then
/// winget's package folder — `winget install ggml.llamacpp` does not refresh
/// the `PATH` of processes already running, which is every process the user
/// launched before installing it.
pub fn find(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        return Some(PathBuf::from(configured));
    }
    if let Ok(path) = std::env::var("ARIA_LLAMA_SERVER")
        && !path.trim().is_empty()
    {
        return Some(PathBuf::from(path.trim()));
    }
    let names: &[&str] = if cfg!(windows) {
        &["llama-server.exe"]
    } else {
        &["llama-server"]
    };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in names {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
    winget(&local.join("Microsoft").join("WinGet").join("Packages"))
}

fn winget(packages: &Path) -> Option<PathBuf> {
    std::fs::read_dir(packages)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("llamacpp")
        })
        .map(|entry| entry.path().join("llama-server.exe"))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LocalConfig {
        LocalConfig {
            model: super::super::GEMMA4_E4B,
            model_path: "m.gguf".into(),
            mmproj_path: "p.gguf".into(),
            server: "llama-server".into(),
            port: 7430,
            extra_args: vec!["-ngl".into(), "99".into()],
            persona: String::new(),
            skills: None,
            instructions_file: None,
            transcribe_after: true,
            log_file: None,
        }
    }

    /// The flags without which the post is deaf or toolless, and the user's
    /// own after ours so they can override.
    #[test]
    fn the_server_is_asked_for_tools_audio_and_two_slots() {
        let args = arguments(&config());
        assert!(args.contains(&"--jinja".to_string()));
        let at = |flag: &str| args.iter().position(|a| a == flag).expect(flag);
        assert_eq!(args[at("--reasoning") + 1], "off");
        assert_eq!(args[at("--mmproj") + 1], "p.gguf");
        assert_eq!(args[at("--port") + 1], "7430");
        assert_eq!(args[at("--parallel") + 1], "2");
        assert_eq!(args[at("-c") + 1], "32768", "two slots share the context");
        assert_eq!(args[at("--host") + 1], "127.0.0.1");
        assert_eq!(&args[args.len() - 2..], ["-ngl", "99"]);
    }

    #[test]
    fn a_configured_path_wins() {
        assert_eq!(find(" C:/llama/llama-server.exe "), Some(PathBuf::from("C:/llama/llama-server.exe")));
    }
}
