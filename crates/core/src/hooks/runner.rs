//! Running a hook's script, once, and reading what it said.
//!
//! Through [`crate::process`], like every process ARIA starts, so the script
//! never opens a console window. The language is told from the extension;
//! the script gets its folder as working directory and these variables:
//!
//! | Variable | What |
//! |---|---|
//! | `ARIA_HOOK_ID` | the folder's name |
//! | `ARIA_HOOK_DIR` | the folder itself, also the working directory |
//! | `ARIA_HOOK_STATE` | `state/`, the script's own memory of what it has seen |
//! | `ARIA_SECRET_<NAME>` | each secret the manifest declares |

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use super::output::{self, Outcome};

/// The folder a script keeps its memory in.
pub const STATE_DIR: &str = "state";

/// What one run gave.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    Nothing,
    Event(output::Event),
    /// The script failed, in words for the tab. Secret values are masked.
    Failed(String),
}

/// Everything a run needs, and nothing it may change.
#[derive(Clone, Debug)]
pub struct Job {
    pub id: String,
    pub dir: PathBuf,
    pub script: String,
    pub timeout: Duration,
    /// `(name, value)`, already read from the system store.
    pub secrets: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct Ran {
    pub verdict: Verdict,
    pub elapsed: Duration,
    /// Whether `state/` was empty before the run: news from a script that had
    /// no baseline is old news, and the caller shows it without saying it.
    pub fresh: bool,
}

/// The program and arguments that run a script, by its extension.
///
/// PowerShell gets UTF-8 both ways. Windows PowerShell 5.1 reads a `.ps1`
/// without a byte-order mark as Windows-1252, and that is how sessions write
/// files, so « Jérôme » arrived as « JÃ©rÃ´me » (caught by a test on
/// 2026-11-27). The script is therefore read as UTF-8 here and run as a
/// script block, which loses `$PSScriptRoot`: the working directory and
/// `ARIA_HOOK_DIR` say where the hook is.
pub fn interpreter(script: &Path) -> Option<(OsString, Vec<OsString>)> {
    let path = script.as_os_str().to_owned();
    let ext = script
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let quoted_ps = format!("'{}'", script.to_string_lossy().replace('\'', "''"));
    let ps = |shell: &str| {
        Some((
            OsString::from(shell),
            [
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
            ]
            .into_iter()
            .map(OsString::from)
            .chain([OsString::from(format!(
                "[Console]::OutputEncoding = [Text.Encoding]::UTF8; \
                 & ([ScriptBlock]::Create([IO.File]::ReadAllText({quoted_ps}, [Text.Encoding]::UTF8))); \
                 exit $LASTEXITCODE"
            ))])
            .collect(),
        ))
    };
    match ext.as_str() {
        "ps1" if cfg!(windows) => ps("powershell.exe"),
        "ps1" => ps("pwsh"),
        "sh" => Some(("bash".into(), vec![path])),
        "py" if cfg!(windows) => Some(("python".into(), vec![path])),
        "py" => Some(("python3".into(), vec![path])),
        "js" | "mjs" => Some(("node".into(), vec![path])),
        "cmd" | "bat" if cfg!(windows) => Some(("cmd.exe".into(), vec!["/D".into(), "/C".into(), path])),
        "exe" if cfg!(windows) => Some((path, Vec::new())),
        "" if !cfg!(windows) => Some((path, Vec::new())),
        _ => None,
    }
}

pub async fn run(job: &Job) -> Ran {
    let started = Instant::now();
    let state = job.dir.join(STATE_DIR);
    let fresh = is_empty_dir(&state);
    let verdict = match attempt(job, &state).await {
        Ok(verdict) => verdict,
        Err(e) => Verdict::Failed(e),
    };
    let verdict = match verdict {
        Verdict::Failed(message) => Verdict::Failed(mask(&message, &job.secrets)),
        Verdict::Event(mut event) => {
            // The test run reaches a model; so may `data` later.
            event.why = mask(&event.why, &job.secrets);
            event.say = event.say.map(|s| mask(&s, &job.secrets));
            event.data = event.data.map(|d| {
                let text = mask(&d.to_string(), &job.secrets);
                serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text))
            });
            Verdict::Event(event)
        }
        nothing => nothing,
    };
    Ran { verdict, elapsed: started.elapsed(), fresh }
}

async fn attempt(job: &Job, state: &Path) -> Result<Verdict, String> {
    let script = job.dir.join(&job.script);
    if !script.is_file() {
        return Err(format!("le script {} est introuvable", job.script));
    }
    let (program, args) = interpreter(&script)
        .ok_or_else(|| format!("aucun interpréteur connu pour {}", job.script))?;
    std::fs::create_dir_all(state).map_err(|e| format!("state/ n'a pas pu être créé : {e}"))?;

    let mut command = crate::process::tokio_command(&program);
    command
        .args(&args)
        .current_dir(&job.dir)
        .env("ARIA_HOOK_ID", &job.id)
        .env("ARIA_HOOK_DIR", &job.dir)
        .env("ARIA_HOOK_STATE", state)
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A run past its timeout is dropped, and dropping kills it.
        .kill_on_drop(true);
    for (name, value) in &job.secrets {
        command.env(format!("ARIA_SECRET_{}", name.to_uppercase()), value);
    }
    let child = command
        .spawn()
        .map_err(|e| format!("{} n'a pas pu démarrer : {e}", program.to_string_lossy()))?;
    let out = match tokio::time::timeout(job.timeout, child.wait_with_output()).await {
        Err(_) => return Err(format!("délai dépassé ({} s)", job.timeout.as_secs())),
        Ok(Err(e)) => return Err(format!("le script n'a pas pu être lu : {e}")),
        Ok(Ok(out)) => out,
    };
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let last = stderr.lines().map(str::trim).filter(|l| !l.is_empty()).last();
        let code = out
            .status
            .code()
            .map_or_else(|| "interrompu".to_string(), |c| format!("code {c}"));
        return Err(match last {
            Some(line) => format!("échec ({code}) : {}", truncate(line, 300)),
            None => format!("échec ({code})"),
        });
    }
    match output::parse(&String::from_utf8_lossy(&out.stdout)) {
        Ok(Outcome::Nothing) => Ok(Verdict::Nothing),
        Ok(Outcome::Event(event)) => Ok(Verdict::Event(event)),
        Err(e) => Err(e.to_string()),
    }
}

/// Replace every secret value by its name, wherever it appears.
///
/// The model never sees a secret: not in a failure, not in the test run, not
/// in a `say` written by a careless script.
pub fn mask(text: &str, secrets: &[(String, String)]) -> String {
    let mut out = text.to_string();
    // Longest first, so a secret that contains another is masked whole.
    let mut sorted: Vec<&(String, String)> = secrets.iter().filter(|(_, v)| v.len() >= 4).collect();
    sorted.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    for (name, value) in sorted {
        out = out.replace(value.as_str(), &format!("[secret {name}]"));
    }
    out
}

fn is_empty_dir(dir: &Path) -> bool {
    std::fs::read_dir(dir).map_or(true, |mut entries| entries.next().is_none())
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    format!("{}…", text.chars().take(max).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_never_reach_a_model() {
        let secrets = vec![("imap".to_string(), "hunter2-long".to_string())];
        assert_eq!(
            mask("auth failed for hunter2-long@imap", &secrets),
            "auth failed for [secret imap]@imap"
        );
        // Too short to mask without masking ordinary words.
        assert_eq!(mask("abc", &[("x".into(), "abc".into())]), "abc");
    }

    #[test]
    fn the_language_is_told_from_the_extension() {
        let (program, args) = interpreter(Path::new("C:/h/check.ps1")).unwrap();
        assert!(program.to_string_lossy().contains("pwsh") || program == "powershell.exe");
        let last = args.last().unwrap().to_string_lossy().to_string();
        assert!(last.contains("UTF8") && last.contains("'C:/h/check.ps1'"), "{last}");
        assert_eq!(interpreter(Path::new("x.sh")).unwrap().0, "bash");
        assert!(interpreter(Path::new("x.docx")).is_none());
    }

    #[test]
    fn a_quote_in_the_path_does_not_break_powershell() {
        let (_, args) = interpreter(Path::new("C:/Users/O'Brien/check.ps1")).unwrap();
        assert!(args.last().unwrap().to_string_lossy().contains("'C:/Users/O''Brien/check.ps1'"));
    }

    /// A real script, the shell this platform has.
    fn job(dir: &Path, body: &str) -> Job {
        let script = if cfg!(windows) { "check.ps1" } else { "check.sh" };
        std::fs::write(dir.join(script), body).unwrap();
        Job {
            id: "essai".into(),
            dir: dir.to_path_buf(),
            script: script.into(),
            timeout: Duration::from_secs(30),
            secrets: vec![("cle".into(), "s3cr3t-value".into())],
        }
    }

    /// The contract end to end: a script that compares with its state
    /// speaks once, then is silent.
    #[tokio::test]
    async fn a_script_that_compares_speaks_once() {
        let dir = tempfile::tempdir().unwrap();
        let body = if cfg!(windows) {
            r#"$seen = Join-Path $env:ARIA_HOOK_STATE 'seen'
if (Test-Path $seen) { exit 0 }
Set-Content $seen '1'
Write-Output '{"why":"nouvelle version","say":"Jérôme a publié la 1.9."}'"#
        } else {
            r#"seen="$ARIA_HOOK_STATE/seen"
[ -f "$seen" ] && exit 0
echo 1 > "$seen"
echo '{"why":"nouvelle version","say":"Jérôme a publié la 1.9."}'"#
        };
        let job = job(dir.path(), body);
        let first = run(&job).await;
        assert!(first.fresh);
        let Verdict::Event(event) = first.verdict else { panic!("{:?}", first.verdict) };
        assert_eq!(event.say.as_deref(), Some("Jérôme a publié la 1.9."), "UTF-8 survives");
        let second = run(&job).await;
        assert!(!second.fresh);
        assert_eq!(second.verdict, Verdict::Nothing);
    }

    #[tokio::test]
    async fn a_failure_is_a_failure_and_its_secret_is_masked() {
        let dir = tempfile::tempdir().unwrap();
        let body = if cfg!(windows) {
            "[Console]::Error.WriteLine(\"login refused: $env:ARIA_SECRET_CLE\"); exit 3"
        } else {
            "echo \"login refused: $ARIA_SECRET_CLE\" >&2; exit 3"
        };
        let ran = run(&job(dir.path(), body)).await;
        let Verdict::Failed(message) = ran.verdict else { panic!("{:?}", ran.verdict) };
        assert!(message.contains("code 3"), "{message}");
        assert!(message.contains("[secret cle]") && !message.contains("s3cr3t"), "{message}");
    }

    #[tokio::test]
    async fn a_script_past_its_timeout_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let body = if cfg!(windows) { "Start-Sleep -Seconds 20" } else { "sleep 20" };
        let mut job = job(dir.path(), body);
        job.timeout = Duration::from_secs(2);
        let ran = run(&job).await;
        assert!(matches!(&ran.verdict, Verdict::Failed(m) if m.contains("délai")), "{:?}", ran.verdict);
        assert!(ran.elapsed < Duration::from_secs(10));
    }
}
