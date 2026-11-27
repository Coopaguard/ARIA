//! Hooks: scripts that wake ARIA when something happens.
//!
//! Defined with the user on 2026-11-27, `docs/todos/hooks.md`. A hook is **a
//! script, not a model**: it checks something on its rhythm and, when there is
//! news, prints one JSON line. ARIA then speaks first, or follows the answer
//! she is giving. While nothing changes, nothing is spent: no token, no
//! notification.
//!
//! ```text
//!   ~/.aria/hooks/<id>/
//!     hook.toml     manifest       what, how often, quiet hours, secrets
//!     check.ps1     the script     any language the runner knows
//!     state/        its memory     what it has already seen
//! ```
//!
//! # What the core holds, and the script cannot touch
//!
//! The switch and the **fingerprint** of what the user validated. A hook runs
//! only while its script and declared secrets are the ones validated: a
//! session, or anyone, that changes the script suspends it until the user
//! validates again. Validation follows a trial — two runs in front of the
//! user, whose results the user judges. Only a failure is refused.
//!
//! # Where the halves are
//!
//! - [`manifest`]: `hook.toml`;
//! - [`output`]: the contract on stdout;
//! - [`runner`]: one run, through [`crate::process`];
//! - [`inbox`]: when an event is said.

pub mod inbox;
pub mod live;
pub mod manifest;
pub mod output;
pub mod runner;
pub mod secrets;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::agent::clock::{self, Stamp};
use inbox::{Delivery, Floor, Inbox, Rules};
use manifest::Manifest;
use output::Level;
use runner::{Job, Ran, Verdict};

/// The skill delegated sessions read before writing a hook. Product
/// knowledge, so it lives here; `app/` puts it where the engine finds it.
pub const SKILL: &str = include_str!("skill.md");
/// Where, under the sessions' folder (`~/.aria/subagents`).
pub const SKILL_PATH: &str = ".claude/skills/aria-hooks/SKILL.md";

/// Failures in a row before a hook is switched off.
pub const MAX_FAILURES: u32 = 3;
/// Runs kept per hook for the tab.
const RECENT: usize = 10;

/// Where the secrets live. The system store in `app/`; a map in tests.
///
/// Never a file: a platform without a store refuses, and says so.
pub trait Secrets: Send + Sync {
    fn get(&self, hook: &str, name: &str) -> Option<String>;
    /// The reason in French when the store refuses.
    fn set(&self, hook: &str, name: &str, value: &str) -> Result<(), String>;
    fn delete(&self, hook: &str, name: &str);
}

/// No store: a hook that declares a secret fails, and says which.
pub struct NoSecrets;

impl Secrets for NoSecrets {
    fn get(&self, _: &str, _: &str) -> Option<String> {
        None
    }
    fn set(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
        Err("Aucun trousseau système n'est disponible : le secret n'est pas enregistré.".into())
    }
    fn delete(&self, _: &str, _: &str) {}
}

/// One hook folder, as found on disk.
#[derive(Clone, Debug)]
pub struct Found {
    pub id: String,
    pub dir: PathBuf,
    pub manifest: Result<Manifest, String>,
    /// `None` when the manifest or the script could not be read.
    pub fingerprint: Option<String>,
}

/// What the user validated.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Approval {
    pub fingerprint: String,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Never validated.
    New,
    Active,
    /// Switched off by the user.
    Off,
    /// Switched off after [`MAX_FAILURES`] failures in a row.
    Failing,
    /// Changed since it was validated: suspended until validated again.
    Modified,
    /// `hook.toml` or the script cannot be read.
    Broken,
}

/// One run, as the tab shows it.
#[derive(Clone, Debug, Serialize)]
pub struct Record {
    pub at: Stamp,
    pub elapsed_ms: u64,
    #[serde(flatten)]
    pub result: RecordResult,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum RecordResult {
    Nothing,
    Event { why: String, say: Option<String>, level: Level },
    Failed { message: String },
}

/// A row of the tab, and what ARIA reads out when asked.
#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub status: Status,
    /// Why it is broken, when it is.
    pub problem: Option<String>,
    pub every_secs: Option<u64>,
    pub last_run: Option<Stamp>,
    pub next_run: Option<Stamp>,
    pub recent: Vec<Record>,
    /// Events waiting for their moment.
    pub waiting: usize,
    /// The secrets it declares: names, and whether one is stored. Never a value.
    pub secrets: Vec<SecretKey>,
    /// A trial run waiting for the user's verdict.
    pub awaiting: Option<Awaiting>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SecretKey {
    pub name: String,
    pub stored: bool,
}

/// Runs a trial takes, each judged by the user before the next one starts.
pub const TRIAL_RUNS: usize = 2;

/// A trial in progress.
///
/// Decided with the user on 2026-11-28: **one run, then the user's verdict,
/// then the next run.** For a mailbox: « tu as reçu ce mail » — right; « pas de
/// nouveau mail » — right. Both verdicts are the validation. Nothing about
/// what a run says is refused by the core: only the user knows what was
/// expected.
#[derive(Clone, Debug)]
pub struct Trial {
    /// What is validated if the user accepts every run.
    pub fingerprint: String,
    pub runs: Vec<Ran>,
    /// How many of `runs` the user has judged right.
    pub judged: usize,
}

/// One run of a trial, waiting for the user's verdict.
#[derive(Clone, Debug)]
pub struct Step {
    /// 1 or 2.
    pub number: usize,
    pub ran: Ran,
    /// Set when the run is not a result to judge — it failed, or the script
    /// changed under it. The trial is then over.
    pub refused: Option<String>,
}

impl Step {
    /// The run, in words, for a card or a sentence.
    pub fn said(&self) -> String {
        describe(&self.ran)
    }
}

/// What a run said, in French.
pub fn describe(ran: &Ran) -> String {
    match &ran.verdict {
        Verdict::Nothing => "rien : aucune nouvelle".to_string(),
        Verdict::Event(e) => match &e.say {
            Some(say) => format!("« {say} »"),
            None => format!("{} (affiché, pas dit)", e.why),
        },
        Verdict::Failed(m) => format!("échec : {m}"),
    }
}

/// What follows the user's verdict on a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Next {
    /// Right, and a run is left: run it.
    RunAgain,
    /// Right, and it was the last one: the hook is validated and on.
    Validated,
    /// Not what was expected: the trial is over, the script to be fixed.
    Discarded,
}

/// The run waiting for the user's verdict, as the tab shows it.
#[derive(Clone, Debug, Serialize)]
pub struct Awaiting {
    pub number: usize,
    #[serde(flatten)]
    pub result: RecordResult,
}

fn result_of(ran: &Ran) -> RecordResult {
    match &ran.verdict {
        Verdict::Nothing => RecordResult::Nothing,
        Verdict::Event(e) => RecordResult::Event {
            why: e.why.clone(),
            say: e.say.clone(),
            level: e.level,
        },
        Verdict::Failed(m) => RecordResult::Failed { message: m.clone() },
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum HookError {
    #[error("aucun hook « {0} »")]
    Unknown(String),
    #[error("le hook « {0} » est illisible : {1}")]
    Broken(String, String),
    #[error("le hook « {0} » a changé depuis son essai : il faut le refaire")]
    Changed(String),
    #[error("le hook « {0} » n'a pas de passage à juger : il faut d'abord l'essayer")]
    NoTrial(String),
    #[error("le hook « {0} » attend l'avis de l'utilisateur sur son passage {1}")]
    Awaiting(String, usize),
    #[error("le hook « {0} » n'a jamais été validé")]
    NotValidated(String),
    #[error("le hook « {0} » est déjà en cours")]
    Running(String),
    #[error("{0}")]
    Secret(String),
    #[error("{0}")]
    Io(String),
}

#[derive(Debug, Default)]
struct Book {
    last_run: Option<(Instant, Stamp)>,
    failures: u32,
    running: bool,
    recent: VecDeque<Record>,
}

#[derive(Default)]
struct State {
    approvals: HashMap<String, Approval>,
    /// The last trial of each hook: what « valider » validates.
    trials: HashMap<String, Trial>,
    books: HashMap<String, Book>,
    inbox: Inbox,
}

/// Every hook of `~/.aria/hooks`, and what the core knows about each.
///
/// The folder is read again on each call: the user and the sessions edit it,
/// and a cache would be one more way to run a script nobody validated.
pub struct Hooks {
    root: PathBuf,
    state: Mutex<State>,
}

impl Hooks {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), state: Mutex::new(State::default()) }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Restore what was validated, from wherever the caller keeps it.
    pub fn restore(&self, approvals: HashMap<String, Approval>) {
        self.state.lock().unwrap().approvals = approvals;
    }

    /// What to keep. Losing it suspends every hook, which is the safe loss.
    pub fn approvals(&self) -> HashMap<String, Approval> {
        self.state.lock().unwrap().approvals.clone()
    }

    pub fn list(&self) -> Vec<Row> {
        let found = discover(&self.root);
        let state = self.state.lock().unwrap();
        let waiting = state.inbox.waiting();
        found
            .iter()
            .map(|hook| {
                let book = state.books.get(&hook.id);
                let status = status(hook, state.approvals.get(&hook.id), book);
                let every = hook.manifest.as_ref().ok().map(|m| m.every);
                let last_run = book.and_then(|b| b.last_run.map(|(_, at)| at));
                let next_run = match (status, every) {
                    (Status::Active, Some(every)) => Some(
                        last_run.map_or_else(clock::now, |at| {
                            at + chrono::Duration::from_std(every).unwrap_or_default()
                        }),
                    ),
                    _ => None,
                };
                Row {
                    id: hook.id.clone(),
                    name: hook
                        .manifest
                        .as_ref()
                        .map_or_else(|_| hook.id.clone(), |m| m.name.clone()),
                    status,
                    problem: hook.manifest.as_ref().err().cloned().or_else(|| {
                        hook.fingerprint
                            .is_none()
                            .then(|| "le script est introuvable".to_string())
                    }),
                    every_secs: every.map(|d| d.as_secs()),
                    last_run,
                    next_run,
                    recent: book.map(|b| b.recent.iter().cloned().collect()).unwrap_or_default(),
                    waiting: waiting.iter().filter(|(id, ..)| *id == hook.id).map(|w| w.2).sum(),
                    // Whether each is stored is the core's to say: it holds the store.
                    secrets: hook
                        .manifest
                        .as_ref()
                        .map(|m| {
                            m.secrets
                                .iter()
                                .map(|name| SecretKey { name: name.clone(), stored: false })
                                .collect()
                        })
                        .unwrap_or_default(),
                    awaiting: state
                        .trials
                        .get(&hook.id)
                        .filter(|t| t.runs.len() > t.judged)
                        .and_then(|t| {
                            Some(Awaiting { number: t.runs.len(), result: result_of(t.runs.last()?) })
                        }),
                }
            })
            .collect()
    }

    /// Run the next run of a hook's trial, and wait for the user's verdict.
    ///
    /// The first call starts the trial. A call while a run is waiting for its
    /// verdict is refused: the user judges each run before the next.
    pub async fn trial_step(&self, id: &str, secrets: &dyn Secrets) -> Result<Step, HookError> {
        let hook = self.find(id)?;
        let manifest = hook
            .manifest
            .clone()
            .map_err(|e| HookError::Broken(id.into(), e))?;
        let fingerprint = hook
            .fingerprint
            .clone()
            .ok_or_else(|| HookError::Broken(id.into(), "le script est introuvable".into()))?;
        {
            let mut state = self.state.lock().unwrap();
            match state.trials.get(id) {
                // A script changed since its last run starts over.
                Some(t) if t.fingerprint != fingerprint => {
                    state.trials.remove(id);
                }
                Some(t) if t.runs.len() > t.judged => {
                    return Err(HookError::Awaiting(id.into(), t.runs.len()));
                }
                _ => {}
            }
        }
        self.claim(id)?;
        let ran = match job(&hook, &manifest, secrets) {
            Ok(job) => runner::run(&job).await,
            Err(message) => Ran {
                verdict: Verdict::Failed(message),
                elapsed: Default::default(),
                fresh: false,
            },
        };
        let after = self.find(id)?.fingerprint;
        let mut state = self.state.lock().unwrap();
        state.books.entry(id.into()).or_default().running = false;
        let trial = state.trials.entry(id.into()).or_insert_with(|| Trial {
            fingerprint: fingerprint.clone(),
            runs: Vec::new(),
            judged: 0,
        });
        let number = trial.runs.len() + 1;
        // Only what is not a result ends the trial: a failure, or a script
        // that changed under it. What a run says is the user's to judge.
        let refused = if after.as_deref() != Some(fingerprint.as_str()) {
            Some("le script a changé pendant son essai".to_string())
        } else if let Verdict::Failed(message) = &ran.verdict {
            Some(format!("le passage {number} a échoué : {message}"))
        } else {
            None
        };
        if refused.is_some() {
            state.trials.remove(id);
        } else {
            trial.runs.push(ran.clone());
        }
        Ok(Step { number, ran, refused })
    }

    /// The user's verdict on the run waiting for one.
    pub fn trial_answer(&self, id: &str, right: bool) -> Result<Next, HookError> {
        let current = self.find(id)?.fingerprint;
        let mut state = self.state.lock().unwrap();
        let trial = state
            .trials
            .get_mut(id)
            .filter(|t| t.runs.len() > t.judged)
            .ok_or_else(|| HookError::NoTrial(id.into()))?;
        if !right {
            state.trials.remove(id);
            return Ok(Next::Discarded);
        }
        trial.judged += 1;
        if trial.judged < TRIAL_RUNS {
            return Ok(Next::RunAgain);
        }
        let fingerprint = trial.fingerprint.clone();
        state.trials.remove(id);
        if current.as_deref() != Some(fingerprint.as_str()) {
            return Err(HookError::Changed(id.into()));
        }
        state
            .approvals
            .insert(id.into(), Approval { fingerprint, enabled: true });
        state.books.entry(id.into()).or_default().failures = 0;
        Ok(Next::Validated)
    }

    /// The run waiting for a verdict, if any.
    pub fn awaiting(&self, id: &str) -> Option<Awaiting> {
        let state = self.state.lock().unwrap();
        let trial = state.trials.get(id).filter(|t| t.runs.len() > t.judged)?;
        Some(Awaiting {
            number: trial.runs.len(),
            result: result_of(trial.runs.last()?),
        })
    }

    /// The switch. Switching on also forgives past failures.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), HookError> {
        self.find(id)?;
        let mut state = self.state.lock().unwrap();
        let approval = state
            .approvals
            .get_mut(id)
            .ok_or_else(|| HookError::NotValidated(id.into()))?;
        approval.enabled = enabled;
        if enabled {
            state.books.entry(id.into()).or_default().failures = 0;
        } else {
            state.inbox.forget(id);
        }
        Ok(())
    }

    /// Remove the folder and everything the core knew. Secrets are the
    /// caller's, since it holds the store.
    pub fn delete(&self, id: &str) -> Result<Vec<String>, HookError> {
        let hook = self.find(id)?;
        let secrets = hook.manifest.as_ref().map(|m| m.secrets.clone()).unwrap_or_default();
        std::fs::remove_dir_all(&hook.dir).map_err(|e| HookError::Io(e.to_string()))?;
        let mut state = self.state.lock().unwrap();
        state.approvals.remove(id);
        state.trials.remove(id);
        state.books.remove(id);
        state.inbox.forget(id);
        Ok(secrets)
    }

    /// The runs that are due, each claimed so a slow one is not started twice.
    pub fn due(&self, secrets: &dyn Secrets) -> Vec<Job> {
        let now = Instant::now();
        let found = discover(&self.root);
        let mut state = self.state.lock().unwrap();
        let mut jobs = Vec::new();
        for hook in &found {
            let book = state.books.get(&hook.id);
            if status(hook, state.approvals.get(&hook.id), book) != Status::Active {
                continue;
            }
            let Ok(manifest) = &hook.manifest else { continue };
            let book = state.books.entry(hook.id.clone()).or_default();
            let due = !book.running
                && book
                    .last_run
                    .is_none_or(|(at, _)| now.duration_since(at) >= manifest.every);
            if !due {
                continue;
            }
            match job(hook, manifest, secrets) {
                Ok(job) => {
                    book.running = true;
                    jobs.push(job);
                }
                Err(message) => {
                    // A missing secret is a failure like any other, counted.
                    book.last_run = Some((now, clock::now()));
                    book.failures += 1;
                    push_recent(book, Record {
                        at: clock::now(),
                        elapsed_ms: 0,
                        result: RecordResult::Failed { message },
                    });
                }
            }
        }
        jobs
    }

    /// Book a run's result. An event goes to the inbox; news from a script
    /// that had no baseline is shown and not said.
    pub fn record(&self, id: &str, ran: Ran) {
        let at = clock::now();
        let mut state = self.state.lock().unwrap();
        let book = state.books.entry(id.into()).or_default();
        book.running = false;
        book.last_run = Some((Instant::now(), at));
        let elapsed_ms = ran.elapsed.as_millis() as u64;
        let result = match &ran.verdict {
            Verdict::Nothing => RecordResult::Nothing,
            Verdict::Event(e) => RecordResult::Event {
                why: e.why.clone(),
                say: e.say.clone(),
                level: e.level,
            },
            Verdict::Failed(m) => RecordResult::Failed { message: m.clone() },
        };
        match &ran.verdict {
            Verdict::Failed(message) => {
                book.failures += 1;
                if book.failures == MAX_FAILURES {
                    tracing::warn!(hook = id, "hook coupé après {MAX_FAILURES} échecs : {message}");
                }
            }
            _ => book.failures = 0,
        }
        push_recent(book, Record { at, elapsed_ms, result });
        if let Verdict::Event(mut event) = ran.verdict {
            // Without a sentence to say it is shown, not said: having the
            // main agent word it needs a message whose answer is spoken, and
            // `Told` has none for something the user did not say. Yet.
            if ran.fresh || event.say.is_none() {
                event.level = Level::Info;
            }
            state.inbox.push(id, event, at.naive_local());
        }
    }

    /// What may be delivered now, given what the floor is doing.
    pub fn take(&self, floor: Floor) -> Vec<Delivery> {
        let rules: HashMap<String, Rules> = discover(&self.root)
            .into_iter()
            .filter_map(|h| {
                let m = h.manifest.ok()?;
                Some((h.id, Rules { quiet: m.quiet, max_per_day: m.max_per_day }))
            })
            .collect();
        self.state.lock().unwrap().inbox.take(clock::now().naive_local(), floor, |id| {
            rules.get(id).copied().unwrap_or_default()
        })
    }


    pub fn has_waiting(&self) -> bool {
        !self.state.lock().unwrap().inbox.waiting().is_empty()
    }

    /// The name the user gave it, or its folder's.
    pub fn name(&self, id: &str) -> String {
        self.find(id)
            .ok()
            .and_then(|h| h.manifest.ok())
            .map_or_else(|| id.to_string(), |m| m.name)
    }

    fn find(&self, id: &str) -> Result<Found, HookError> {
        discover(&self.root)
            .into_iter()
            .find(|h| h.id == id)
            .ok_or_else(|| HookError::Unknown(id.into()))
    }

    fn claim(&self, id: &str) -> Result<(), HookError> {
        let mut state = self.state.lock().unwrap();
        let book = state.books.entry(id.into()).or_default();
        if book.running {
            return Err(HookError::Running(id.into()));
        }
        book.running = true;
        Ok(())
    }
}

fn push_recent(book: &mut Book, record: Record) {
    book.recent.push_front(record);
    book.recent.truncate(RECENT);
}

fn job(hook: &Found, manifest: &Manifest, secrets: &dyn Secrets) -> Result<Job, String> {
    let mut values = Vec::new();
    for name in &manifest.secrets {
        let value = secrets
            .get(&hook.id, name)
            .ok_or_else(|| format!("le secret « {name} » n'est pas enregistré"))?;
        values.push((name.clone(), value));
    }
    Ok(Job {
        id: hook.id.clone(),
        dir: hook.dir.clone(),
        script: manifest.script.clone(),
        timeout: manifest.timeout,
        secrets: values,
    })
}

fn status(hook: &Found, approval: Option<&Approval>, book: Option<&Book>) -> Status {
    let Some(fingerprint) = hook.fingerprint.as_deref().filter(|_| hook.manifest.is_ok()) else {
        return Status::Broken;
    };
    match approval {
        None => Status::New,
        Some(a) if a.fingerprint != fingerprint => Status::Modified,
        Some(a) if !a.enabled => Status::Off,
        Some(_) if book.is_some_and(|b| b.failures >= MAX_FAILURES) => Status::Failing,
        Some(_) => Status::Active,
    }
}

/// A folder name the tab, the voice and an environment variable can carry.
pub fn is_hook_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Every folder under `root` that holds a `hook.toml`, by name.
pub fn discover(root: &Path) -> Vec<Found> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<Found> = entries
        .flatten()
        .filter(|e| e.path().join(manifest::FILE).is_file())
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().to_string();
            if !is_hook_id(&id) {
                tracing::warn!("dossier de hook ignoré, nom invalide : {id}");
                return None;
            }
            let dir = e.path();
            let manifest = std::fs::read_to_string(dir.join(manifest::FILE))
                .map_err(|e| e.to_string())
                .and_then(|text| manifest::parse(&text).map_err(|e| e.to_string()));
            let fingerprint = manifest
                .as_ref()
                .ok()
                .and_then(|m| fingerprint(&dir, m).ok());
            Some(Found { id, dir, manifest, fingerprint })
        })
        .collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}

/// SHA-256, in hex. Also what `app/` marks the shipped skill with.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// What validation covers: the script's bytes, its name and the secrets it
/// receives. Not the rhythm or the quiet hours, which the user may tune
/// without a new trial.
pub fn fingerprint(dir: &Path, manifest: &Manifest) -> std::io::Result<String> {
    let script = std::fs::read(dir.join(&manifest.script))?;
    let mut hash = Sha256::new();
    hash.update(b"aria-hook-1\0");
    hash.update(manifest.script.as_bytes());
    hash.update(b"\0");
    hash.update(manifest.secrets.join(",").as_bytes());
    hash.update(b"\0");
    hash.update(&script);
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script_name() -> &'static str {
        if cfg!(windows) { "check.ps1" } else { "check.sh" }
    }

    /// A hook that reports a new version once, then compares.
    fn comparing() -> &'static str {
        if cfg!(windows) {
            r#"$seen = Join-Path $env:ARIA_HOOK_STATE 'seen'
if (Test-Path $seen) { exit 0 }
Set-Content $seen '1'
Write-Output '{"why":"nouvelle version","say":"La 1.9 est sortie."}'"#
        } else {
            r#"[ -f "$ARIA_HOOK_STATE/seen" ] && exit 0
echo 1 > "$ARIA_HOOK_STATE/seen"
echo '{"why":"nouvelle version","say":"La 1.9 est sortie."}'"#
        }
    }

    /// A hook that says what it found every time: the noisy one.
    fn reporting() -> &'static str {
        if cfg!(windows) {
            "Write-Output '{\"why\":\"rien de nouveau\"}'"
        } else {
            "echo '{\"why\":\"rien de nouveau\"}'"
        }
    }

    fn hook(root: &Path, id: &str, body: &str, extra: &str) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(manifest::FILE),
            format!("name = \"Essai\"\nscript = \"{}\"\nevery = \"6h\"\n{extra}", script_name()),
        )
        .unwrap();
        std::fs::write(dir.join(script_name()), body).unwrap();
        dir
    }

    /// The skill states the contract; when a limit moves, it must too.
    #[test]
    fn the_skill_says_what_the_code_enforces() {
        assert!(SKILL.starts_with("---\nname: aria-hooks\n"));
        for said in [
            format!("{} caractères au plus", output::WHY_MAX),
            format!("{} caractères", output::SAY_MAX),
            format!("{} Ko", output::DATA_MAX / 1024),
            format!("{}m au moins", manifest::MIN_EVERY.as_secs() / 60),
            format!("{}m au plus", manifest::MAX_TIMEOUT.as_secs() / 60),
            format!("trois échecs"),
        ] {
            assert!(SKILL.contains(&said), "le skill ne dit pas « {said} »");
        }
        assert_eq!(MAX_FAILURES, 3);
        for tool in ["hook_trial", "ask_secret", "ARIA_HOOK_STATE", "ARIA_SECRET_"] {
            assert!(SKILL.contains(tool), "{tool}");
        }
    }

    #[test]
    fn a_folder_is_a_hook_only_with_its_manifest_and_a_fair_name() {
        let root = tempfile::tempdir().unwrap();
        hook(root.path(), "maj-obsidian", "", "");
        hook(root.path(), "Bad Name", "", "");
        std::fs::create_dir_all(root.path().join("vide")).unwrap();
        let found = discover(root.path());
        assert_eq!(found.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), ["maj-obsidian"]);
        assert!(discover(&root.path().join("absent")).is_empty());
    }

    #[test]
    fn the_fingerprint_covers_the_script_and_its_secrets_not_the_rhythm() {
        let root = tempfile::tempdir().unwrap();
        let dir = hook(root.path(), "h", "echo", "");
        let read = |dir: &Path| {
            let m = manifest::parse(&std::fs::read_to_string(dir.join(manifest::FILE)).unwrap()).unwrap();
            fingerprint(dir, &m).unwrap()
        };
        let before = read(&dir);
        let toml = std::fs::read_to_string(dir.join(manifest::FILE)).unwrap();
        std::fs::write(dir.join(manifest::FILE), toml.replace("6h", "2h")).unwrap();
        assert_eq!(read(&dir), before, "the user may tune the rhythm");
        std::fs::write(dir.join(manifest::FILE), toml.clone() + "secrets = [\"banque\"]\n").unwrap();
        assert_ne!(read(&dir), before, "a new secret is a new hook");
        std::fs::write(dir.join(manifest::FILE), toml).unwrap();
        std::fs::write(dir.join(script_name()), "echo changed").unwrap();
        assert_ne!(read(&dir), before);
    }

    /// The whole life of a hook: new, tried, validated, run, modified.
    #[tokio::test]
    async fn a_hook_runs_only_as_it_was_validated() {
        let root = tempfile::tempdir().unwrap();
        let dir = hook(root.path(), "maj", comparing(), "");
        let hooks = Hooks::new(root.path());
        assert_eq!(hooks.list()[0].status, Status::New);
        assert!(hooks.due(&NoSecrets).is_empty(), "never validated, never run");

        // One run, the user's verdict, the next run, the user's verdict.
        let first = hooks.trial_step("maj", &NoSecrets).await.unwrap();
        assert_eq!((first.number, first.refused.clone()), (1, None));
        assert!(first.said().contains("La 1.9 est sortie."), "{}", first.said());
        assert_eq!(hooks.list()[0].awaiting.as_ref().map(|a| a.number), Some(1));
        assert!(
            matches!(hooks.trial_step("maj", &NoSecrets).await, Err(HookError::Awaiting(_, 1))),
            "no second run before the verdict on the first"
        );
        assert_eq!(hooks.trial_answer("maj", true), Ok(Next::RunAgain));
        assert_eq!(hooks.list()[0].status, Status::New, "not validated half-way");
        let second = hooks.trial_step("maj", &NoSecrets).await.unwrap();
        assert_eq!(second.number, 2);
        assert!(second.said().contains("aucune nouvelle"));
        assert_eq!(hooks.trial_answer("maj", true), Ok(Next::Validated));
        assert_eq!(hooks.list()[0].status, Status::Active);
        assert!(hooks.list()[0].awaiting.is_none());

        // The trial left its baseline: the first live run is silent.
        let jobs = hooks.due(&NoSecrets);
        assert_eq!(jobs.len(), 1);
        assert!(hooks.due(&NoSecrets).is_empty(), "claimed, not started twice");
        let ran = runner::run(&jobs[0]).await;
        assert_eq!(ran.verdict, Verdict::Nothing);
        hooks.record("maj", ran);
        assert!(hooks.due(&NoSecrets).is_empty(), "six hours to wait");

        std::fs::write(dir.join(script_name()), reporting()).unwrap();
        assert_eq!(hooks.list()[0].status, Status::Modified);
        assert!(hooks.due(&NoSecrets).is_empty(), "suspended until validated again");
    }

    /// What a run says is the user's call, decided on 2026-11-28: a hook that
    /// speaks at both runs is validated if that is what they expected.
    #[tokio::test]
    async fn the_user_judges_what_each_run_says() {
        let root = tempfile::tempdir().unwrap();
        hook(root.path(), "bavard", reporting(), "");
        let hooks = Hooks::new(root.path());
        for _ in 0..TRIAL_RUNS {
            let step = hooks.trial_step("bavard", &NoSecrets).await.unwrap();
            assert_eq!(step.refused, None);
            assert!(step.said().contains("rien de nouveau"), "{}", step.said());
            hooks.trial_answer("bavard", true).unwrap();
        }
        assert_eq!(hooks.list()[0].status, Status::Active);
    }

    /// « Ce n'est pas ce que j'attendais » ends the trial; the next one starts
    /// from its first run.
    #[tokio::test]
    async fn a_run_the_user_rejects_ends_the_trial() {
        let root = tempfile::tempdir().unwrap();
        hook(root.path(), "maj", comparing(), "");
        let hooks = Hooks::new(root.path());
        hooks.trial_step("maj", &NoSecrets).await.unwrap();
        assert_eq!(hooks.trial_answer("maj", false), Ok(Next::Discarded));
        assert_eq!(hooks.list()[0].status, Status::New);
        assert!(matches!(hooks.trial_answer("maj", true), Err(HookError::NoTrial(_))));
        assert_eq!(hooks.trial_step("maj", &NoSecrets).await.unwrap().number, 1);
    }

    /// A script changed between two runs starts its trial again.
    #[tokio::test]
    async fn a_script_changed_mid_trial_starts_over() {
        let root = tempfile::tempdir().unwrap();
        let dir = hook(root.path(), "maj", comparing(), "");
        let hooks = Hooks::new(root.path());
        hooks.trial_step("maj", &NoSecrets).await.unwrap();
        hooks.trial_answer("maj", true).unwrap();
        std::fs::write(dir.join(script_name()), reporting()).unwrap();
        assert_eq!(hooks.trial_step("maj", &NoSecrets).await.unwrap().number, 1);
    }

    /// What is not a result is refused: a failure ends the trial.
    #[tokio::test]
    async fn a_run_that_fails_ends_the_trial() {
        let root = tempfile::tempdir().unwrap();
        hook(root.path(), "casse", "exit 4", "");
        let hooks = Hooks::new(root.path());
        let step = hooks.trial_step("casse", &NoSecrets).await.unwrap();
        assert!(step.refused.as_deref().unwrap().contains("passage 1 a échoué"), "{step:?}");
        assert!(hooks.trial_answer("casse", true).is_err(), "nothing to judge");
    }

    #[tokio::test]
    async fn a_missing_secret_fails_the_trial_without_running() {
        let root = tempfile::tempdir().unwrap();
        hook(root.path(), "mails", comparing(), "secrets = [\"imap\"]\n");
        let hooks = Hooks::new(root.path());
        let step = hooks.trial_step("mails", &NoSecrets).await.unwrap();
        assert!(step.refused.as_deref().unwrap().contains("imap"), "{step:?}");
    }

    #[test]
    fn three_failures_switch_it_off_and_switching_on_forgives() {
        let root = tempfile::tempdir().unwrap();
        let dir = hook(root.path(), "h", "", "");
        let hooks = Hooks::new(root.path());
        let found = &discover(root.path())[0];
        hooks.restore(HashMap::from([(
            "h".to_string(),
            Approval { fingerprint: found.fingerprint.clone().unwrap(), enabled: true },
        )]));
        let failed = || Ran {
            verdict: Verdict::Failed("le site ne répond pas".into()),
            elapsed: Default::default(),
            fresh: false,
        };
        for _ in 0..MAX_FAILURES {
            assert_eq!(hooks.list()[0].status, Status::Active);
            hooks.record("h", failed());
        }
        assert_eq!(hooks.list()[0].status, Status::Failing);
        assert_eq!(hooks.list()[0].recent.len(), MAX_FAILURES as usize);
        hooks.set_enabled("h", true).unwrap();
        assert_eq!(hooks.list()[0].status, Status::Active);
        hooks.set_enabled("h", false).unwrap();
        assert_eq!(hooks.list()[0].status, Status::Off);
        assert!(dir.exists());
        hooks.delete("h").unwrap();
        assert!(!dir.exists() && hooks.list().is_empty() && hooks.approvals().is_empty());
    }

    /// News from a run with no baseline is old news: shown, not said.
    #[test]
    fn news_without_a_baseline_is_shown_not_said() {
        let root = tempfile::tempdir().unwrap();
        hook(root.path(), "mails", "", "");
        let hooks = Hooks::new(root.path());
        let event = output::Event {
            why: "200 mails".into(),
            say: Some("Tu as deux cents mails.".into()),
            data: None,
            level: Level::Normal,
        };
        hooks.record("mails", Ran { verdict: Verdict::Event(event.clone()), elapsed: Default::default(), fresh: true });
        let out = hooks.take(Floor::Free);
        assert_eq!(out[0].how, inbox::How::Notify);
        hooks.record("mails", Ran { verdict: Verdict::Event(event), elapsed: Default::default(), fresh: false });
        assert_eq!(hooks.list()[0].waiting, 1);
        assert_eq!(hooks.take(Floor::Free)[0].how, inbox::How::Lead);
    }
}
