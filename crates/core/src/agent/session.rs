//! The persistent CLI session that holds the conversation.
//!
//! One process, many turns. That is what makes tier A viable at all: **the MCP
//! handshake is paid once, not per turn**, and it was the thing that collapsed
//! when a process was launched per sentence. T0 measured 29 warm turns through
//! a single process, first text at 1113 ms median.
//!
//! Every flag below was surveyed by running CLI 2.1.165, not read from
//! documentation — `docs/cli/claude-code-cli.md` §8 — and re-checked
//! against `claude --help` on 2026-10-26 before being written here.
//!
//! # The four that are not obvious
//!
//! **`--model` takes the full name, never the alias.** Measured: `--model
//! haiku` resolved to `claude-haiku-4-5-20251001`, a generation behind what was
//! asked for, silently. An alias is a promise the CLI makes to itself.
//!
//! **`--effort low` does not switch reasoning off.** It reduced it; it did not
//! remove it, and Haiku still spent 1.5 s thinking before its first word.
//! `MAX_THINKING_TOKENS=0` is the lever that works — output dropped from 220
//! tokens to 55 — and it still does not eliminate reasoning entirely.
//!
//! **`--strict-mcp-config` is not tidiness.** Without it, five claude.ai
//! connectors load at start-up: 916 ms, and a set of tools a voice dispatcher
//! must not have.
//!
//! **`--bare` is deliberately absent.** It is the minimal mode and would suit a
//! light dispatcher exactly — except that its authentication is *strictly*
//! `ANTHROPIC_API_KEY`: OAuth and the keychain are never read. The lightest
//! mode is the billed mode, and the rule of 2026-10-24 is that this post is
//! never billed per token. The weight comes off with the flags above instead.
//!
//! # And one that is about the working directory
//!
//! The session runs in a directory of its own, **with no `CLAUDE.md` in it or
//! above it**. A voice assistant that discovers a project's instructions starts
//! answering as that project's coding agent; and the discovery is dynamic, so
//! it also moves the cached prefix from one turn to the next.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, mpsc};

use super::tools::SYSTEM_PROMPT;
use crate::providers::Usage;
use crate::providers::claude_code::protocol::{self, CliEvent, ContentBlock, Parsed, SystemEvent};

/// The model, in full.
///
/// A constant rather than a default buried in a struct, because the measured
/// fault — an alias quietly resolving a generation behind — is one nobody goes
/// looking for.
pub const MODEL: &str = "claude-haiku-5-5";

/// How to launch the main agent.
#[derive(Clone, Debug)]
pub struct Config {
    /// The binary. `ARIA_CLAUDE_BIN` overrides it, which is how a test points
    /// at a stub.
    pub command: String,
    pub model: String,
    /// Where the post runs. `~/.aria` in the product (decision 108), and it
    /// must have no `CLAUDE.md` in it **or above it** — see [`claude_md_above`].
    pub cwd: PathBuf,
    /// Our MCP server, as inline JSON, when there is one to offer.
    pub mcp_config: Option<String>,
    /// The tool names the session may call. Empty means **words only**.
    pub allowed_tools: Vec<String>,
    /// The relay the CLI asks before running one of them.
    ///
    /// Without it a tool exists and cannot be run, which is a distinction the
    /// flag names hide: see [`arguments`].
    pub permission_tool: Option<String>,

    /// The user's own standing instructions, as a file for the CLI to read.
    ///
    /// `~/.aria/AGENTS.md`, decision 106. A path and not text, and that is
    /// what makes it safe: the whole system prompt already travels as one
    /// `argv` element, and Windows bounds a command line at 32 767
    /// characters — appending a file of unknown size to it would fail at a
    /// length nobody would think to look for.
    ///
    /// Verified on CLI 2.1.312 on 2026-11-12: `--append-system-prompt-file`
    /// combines with `--system-prompt`, and the post starts.
    pub instructions_file: Option<PathBuf>,

    /// What the user adds to [`SYSTEM_PROMPT`], appended to it.
    ///
    /// **Appended, never substituted.** The shipped lines are the mechanism —
    /// repeat a payload verbatim, nothing that cannot be heard, say nothing
    /// after delegating — and each of them is there because its absence
    /// produced a specific fault out loud. A persona goes after them; it does
    /// not get to overrule them, and the order says so to the model as much as
    /// to the reader.
    pub persona: String,

    /// What the delegated sessions can reach, as one line — decision 117.
    ///
    /// Built by [`super::skills::prompt_line`] from the engine's connectors,
    /// read once before launch: the prompt is frozen at the first request, so
    /// this is the only moment it can be said.
    pub skills: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            command: std::env::var("ARIA_CLAUDE_BIN").unwrap_or_else(|_| "claude".into()),
            model: MODEL.into(),
            cwd: Config::fallback_cwd(),
            mcp_config: None,
            allowed_tools: Vec::new(),
            permission_tool: None,
            instructions_file: None,
            persona: String::new(),
            skills: None,
        }
    }
}

/// The command line, built apart from the spawning so it can be asserted on.
pub fn arguments(config: &Config) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        // The TTS is fed as the words arrive; waiting for a complete message
        // would add the whole generation to the first syllable.
        "--include-partial-messages",
        // Nothing to resume: this process *is* the session, and what it said is
        // in our own journal. Not writing transcripts to disk is also the
        // cheaper answer for a microphone that is always on.
        "--no-session-persistence",
        "--strict-mcp-config",
        // Keeps `cwd`, the environment and git state out of the system prompt.
        // The prefix has to be identical from one turn to the next or the cache
        // is missed every time, and the cache is most of why a turn is fast.
        "--exclude-dynamic-system-prompt-sections",
        "--effort",
        "low",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    args.push("--model".into());
    args.push(config.model.clone());

    args.push("--tools".into());
    if config.allowed_tools.is_empty() {
        // Measured: an empty value yields `"tools":[]`. A dispatcher with no
        // tools can still hold a conversation, which is what the tiers before
        // the MCP catalogue need.
        args.push(String::new());
    } else {
        args.extend(config.allowed_tools.iter().cloned());
    }

    if let Some(tool) = &config.permission_tool {
        // Measured live on 2026-10-26: `--tools` makes a tool **exist**, it
        // does not make it runnable. The model called `delegate_task` of its
        // own accord, the CLI asked for permission, there was nobody to ask,
        // and the refusal came out of the loudspeaker as "l'autorisation n'a
        // pas été accordée" — the voice telling the user about a dialog that
        // has no business existing.
        //
        // The answer is the relay, which is the mechanism this project already
        // chose for exactly this question (decision 6). `--allowed-tools` was
        // tried first and is the wrong lever: it is a blanket pre-approval
        // flag, and on the main agent's post the policy is narrower than a
        // list — see `crate::mcp::voice::approve`.
        args.push("--permission-prompt-tool".into());
        args.push(tool.clone());
    }

    if let Some(mcp) = &config.mcp_config {
        args.push("--mcp-config".into());
        args.push(mcp.clone());
    }

    // Last, because it is long: a command line read in a process list stays
    // legible up to here.
    args.push("--system-prompt".into());
    args.push(prompt_with(&config.persona, config.skills.as_deref()));

    // After `--system-prompt`, which is the order it is applied in: the file
    // is appended to the prompt above, never substituted for it. Decision 106,
    // and the reason it is a file rather than more text in the argument above
    // is on [`Config::instructions_file`].
    if let Some(file) = &config.instructions_file {
        args.push("--append-system-prompt-file".into());
        args.push(file.display().to_string());
    }
    args
}

/// The shipped prompt, with the user's persona after it.
///
/// After, and that is not cosmetic: the shipped lines are the mechanism, the
/// persona is a preference, and a model reads an order of precedence in the
/// order it is given them. The heading is there so neither the model nor
/// anybody reading a process list has to guess where one ends.
pub fn prompt(persona: &str) -> String {
    prompt_with(persona, None)
}

/// The shipped prompt, what the delegations can reach, then the persona.
///
/// The skills line sits with the shipped rules because it is ours, not the
/// user's — and before the persona for the same reason the rules do.
pub fn prompt_with(persona: &str, skills: Option<&str>) -> String {
    let mut out = SYSTEM_PROMPT.to_string();
    if let Some(skills) = skills.map(str::trim).filter(|s| !s.is_empty()) {
        out.push_str("\n- ");
        out.push_str(skills);
    }
    let persona = persona.trim();
    if !persona.is_empty() {
        out.push_str(
            "\n\nConsignes de ton utilisateur (elles ne lèvent aucune règle ci-dessus) :\n",
        );
        out.push_str(persona);
    }
    out
}

/// What the agent is doing, normalised.
#[derive(Clone, Debug)]
pub enum Speech {
    /// The session is up, with the id the CLI gave it.
    Started {
        native_id: String,
        /// The tools the session carries, as the CLI lists them.
        ///
        /// Carried rather than dropped because `--tools` is the only thing
        /// standing between a voice dispatcher and a coding agent, and a
        /// mistake there is silent: nothing looks wrong about a session that
        /// also happens to hold `Bash`. Judging the list is the caller's —
        /// [`super::pilot`] knows which tools were asked for; this module does
        /// not.
        ///
        /// **It is empty on the first turn, and that is not a defect of ours.**
        /// Measured on 2026-10-26: `init` is emitted while the MCP servers are
        /// still `status: "pending"`, so the handshake has not happened and
        /// there is nothing to list yet. From the second turn on — `init` comes
        /// back every turn — the MCP tools appear. Which is one more reason the
        /// warm-up of [`MainAgent::warm_up`] is not optional.
        tools: Vec<String>,
    },
    /// A fragment to say. Already in speaking order.
    Delta(String),
    /// It reached for a tool. The arguments are raw: `delegate_task` is typed
    /// at the call site, and an unknown tool must still be loggable.
    ToolUse {
        name: String,
        input: serde_json::Value,
    },
    /// The turn is over.
    Finished {
        text: String,
        usage: Usage,
    },
    Failed(String),
    /// The turn was cut short at our request — see [`Speaker::interrupt`].
    ///
    /// Carries what had been written by then, for the journal. Not a failure:
    /// reading it as one is how ARIA would answer being interrupted with « Je
    /// n'ai pas pu répondre ».
    Interrupted(String),
    /// Where the subscription stands. It arrives unasked, mid-turn, and it is
    /// the only place the CLI reports it — there is no `usage` subcommand.
    Quota {
        utilization: f64,
        window: String,
        warning: bool,
    },
    /// A line we do not model. Logged, never fatal — the CLI adds event types
    /// between versions and a bridge that dies on one breaks on an update
    /// nobody asked for.
    Unknown(String),
}

/// A running conversation.
pub struct MainAgent {
    child: Child,
    stdin: ChildStdin,
    events: mpsc::Receiver<Speech>,
    stderr: Arc<Mutex<String>>,
    native_id: Option<String>,
    /// Set when an interrupt is sent, read by the stream reader at the next
    /// `result`. See [`Speaker::interrupt`].
    interrupting: Arc<AtomicBool>,
}

impl Config {
    /// Where the post runs when `~` could not be resolved.
    ///
    /// A throwaway directory under `%TEMP%`, which is what the product used
    /// until decision 108 and was chosen for exactly one property: nothing
    /// puts a `CLAUDE.md` there. It stays as the fallback for that property,
    /// and for nothing else.
    pub fn fallback_cwd() -> PathBuf {
        std::env::temp_dir().join("aria-agent")
    }
}

/// The nearest `CLAUDE.md` at `dir` or above it, if there is one.
///
/// Walking **up** and not merely looking in `dir`: the half the first version
/// missed, and the one decision 108 made load-bearing. The old working
/// directory was under `%TEMP%`, whose parents hold nothing of ours; `~/.aria`
/// has the user's home for a parent, where a `CLAUDE.md` is an ordinary thing
/// to find — and the CLI walks up to it.
///
/// What this cannot see, and nothing can: `~/.claude/CLAUDE.md`, which the CLI
/// loads whatever the working directory is. That one is the user's own global
/// memory, out of our reach; this check is about the tree we chose.
pub fn claude_md_above(dir: &Path) -> Option<PathBuf> {
    dir.ancestors()
        .map(|parent| parent.join("CLAUDE.md"))
        .find(|candidate| candidate.is_file())
}

impl MainAgent {
    /// Start the process and the reader behind it.
    pub async fn start(config: Config) -> Result<Self> {
        std::fs::create_dir_all(&config.cwd)
            .with_context(|| format!("cannot create {}", config.cwd.display()))?;
        if let Some(found) = claude_md_above(&config.cwd) {
            // Said plainly rather than tolerated: the symptom is an assistant
            // that answers about a repository, and nothing points at the cause.
            bail!(
                "{} is read from {} — the voice agent must run where no CLAUDE.md is found",
                found.display(),
                config.cwd.display()
            );
        }

        let mut child = crate::process::tokio_command(&config.command)
            .args(arguments(&config))
            // `--effort low` was not enough; this is the lever that works.
            .env("MAX_THINKING_TOKENS", "0")
            .current_dir(&config.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("cannot start {} — is the CLI on PATH?", config.command))?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr_pipe = child.stderr.take().expect("stderr was piped");

        // Drained in the background, and that is not optional: a full stderr
        // pipe deadlocks the child, and its contents are what explain a failure.
        let stderr = Arc::new(Mutex::new(String::new()));
        {
            let sink = Arc::clone(&stderr);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr_pipe).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    sink.lock().await.push_str(&line);
                    sink.lock().await.push('\n');
                }
            });
        }

        let (tx, events) = mpsc::channel(256);
        let interrupting = Arc::new(AtomicBool::new(false));
        tokio::spawn(read_stream(stdout, tx, Arc::clone(&interrupting)));

        Ok(Self {
            child,
            stdin,
            events,
            stderr,
            native_id: None,
            interrupting,
        })
    }

    /// Spend one turn getting the MCP servers connected, before anyone speaks.
    ///
    /// **Measured on 2026-10-26, and it is not optional.** The CLI reports
    /// every MCP server as `status: "pending"` in its `init` line and starts
    /// the turn without waiting for the handshake — so the first thing said to
    /// a fresh process is answered by a model that has no tools. The symptom
    /// is an assistant explaining, fluently and at length, that it cannot look
    /// up the weather.
    ///
    /// It cannot be fixed by waiting before speaking, because the CLI does
    /// nothing at all until it has input: the connection only begins once the
    /// first message arrives. So the warm-up has to be a real turn, and the
    /// answer is thrown away.
    ///
    /// A persistent session pays this once and holds it all day, which is the
    /// whole argument for tier A — the same argument as the MCP handshake
    /// itself, one floor up.
    pub async fn warm_up(&mut self) -> Result<()> {
        self.say("bonjour").await?;
        while let Some(event) = self.next().await {
            match event {
                Speech::Finished { .. } => return Ok(()),
                Speech::Failed(message) => bail!("le tour de chauffe a échoué : {message}"),
                _ => {}
            }
        }
        bail!("le processus s'est arrêté pendant le tour de chauffe")
    }

    /// Say something to it. One process holds every turn.
    pub async fn say(&mut self, text: &str) -> Result<()> {
        let line = protocol::user_message(text);
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// The next thing it does, or `None` when the stream has ended.
    pub async fn next(&mut self) -> Option<Speech> {
        let event = self.events.recv().await?;
        if let Speech::Started { native_id, .. } = &event {
            self.native_id = Some(native_id.clone());
        }
        Some(event)
    }

    pub fn native_id(&self) -> Option<&str> {
        self.native_id.as_deref()
    }

    /// Hand the event stream out, keeping the half that speaks.
    ///
    /// Two tasks have to share this process: one reads what the agent says,
    /// one tells it what it heard. Both `next` and `say` take `&mut self`, so
    /// they cannot be held by two tasks at once — and a `select!` over the two
    /// would not do either, since the handler of one branch needs the borrow
    /// the other branch's future is holding.
    ///
    /// Splitting is also the shape the rest of the crate already uses:
    /// [`crate::providers::AgentProvider::launch`] returns a handle and a
    /// receiver for exactly this reason.
    ///
    /// Called **after** [`Self::warm_up`], which needs both halves.
    pub fn split(self) -> (Speaker, mpsc::Receiver<Speech>) {
        (
            Speaker {
                child: self.child,
                stdin: self.stdin,
                stderr: self.stderr,
                interrupting: self.interrupting,
                requests: AtomicU64::new(0),
            },
            self.events,
        )
    }

    /// Whatever the process wrote to stderr, which is where a start-up failure
    /// explains itself.
    pub async fn stderr(&self) -> String {
        self.stderr.lock().await.clone()
    }

    /// Close stdin and let it finish; kill it if it will not.
    pub async fn stop(&mut self) -> Result<()> {
        self.stdin.shutdown().await.ok();
        match tokio::time::timeout(std::time::Duration::from_secs(5), self.child.wait()).await {
            Ok(status) => {
                status?;
            }
            Err(_) => {
                tracing::warn!("the main agent did not exit on its own; killing it");
                self.child.kill().await.ok();
            }
        }
        Ok(())
    }
}

/// The half of a running agent that speaks to it.
pub struct Speaker {
    child: Child,
    stdin: ChildStdin,
    stderr: Arc<Mutex<String>>,
    interrupting: Arc<AtomicBool>,
    /// For the interrupt's `request_id`. The CLI echoes it back; we do not
    /// wait on the answer, but a unique id keeps a log line attributable.
    requests: AtomicU64,
}

impl Speaker {
    /// Say something to it. One process holds every turn.
    pub async fn say(&mut self, text: &str) -> Result<()> {
        let line = protocol::user_message(text);
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    /// Cut the turn in progress short. The process, its cache and its context
    /// stay; only the answer being written stops.
    ///
    /// The flag is raised **before** the line goes out, so the `result` that
    /// ends the turn — `error_during_execution`, measured on 2026-11-23 — is
    /// read as [`Speech::Interrupted`] and not as a failure.
    pub async fn interrupt(&mut self) -> Result<()> {
        let n = self.requests.fetch_add(1, Ordering::SeqCst) + 1;
        self.interrupting.store(true, Ordering::SeqCst);
        let line = protocol::interrupt_request(&format!("aria-interrupt-{n}"));
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    pub async fn stderr(&self) -> String {
        self.stderr.lock().await.clone()
    }

    /// Close stdin and let it finish; kill it if it will not.
    pub async fn stop(&mut self) -> Result<()> {
        self.stdin.shutdown().await.ok();
        match tokio::time::timeout(std::time::Duration::from_secs(5), self.child.wait()).await {
            Ok(status) => {
                status?;
            }
            Err(_) => {
                tracing::warn!("the main agent did not exit on its own; killing it");
                self.child.kill().await.ok();
            }
        }
        Ok(())
    }
}

/// Turn the CLI's stdout into [`Speech`].
async fn read_stream(
    stdout: tokio::process::ChildStdout,
    tx: mpsc::Sender<Speech>,
    interrupting: Arc<AtomicBool>,
) {
    let mut lines = BufReader::new(stdout).lines();
    // A turn's text is accumulated here so `Finished` can carry the whole of
    // it: the deltas went to the TTS as they arrived, but the journal wants one
    // string.
    let mut said = String::new();
    // Partial events and complete messages both carry the text. Taking both
    // would say everything twice.
    let mut saw_delta = false;

    while let Ok(Some(line)) = lines.next_line().await {
        // At `trace`, because the lines this does not model are exactly the
        // lines that explain a session behaving oddly — and the CLI's own
        // start-up diagnostics travel on stdout, not stderr.
        tracing::trace!("{line}");
        let event = match protocol::parse_line(&line) {
            Parsed::Known(event) => event,
            Parsed::Unknown(raw) => {
                let _ = tx.send(Speech::Unknown(raw)).await;
                continue;
            }
        };

        let out = match event {
            CliEvent::System(SystemEvent::Init {
                session_id, tools, ..
            }) => {
                tracing::debug!("outils de la session : {}", tools.join(", "));
                Some(Speech::Started {
                    native_id: session_id,
                    tools,
                })
            }
            CliEvent::System(SystemEvent::Other) => None,

            CliEvent::StreamEvent { event } => {
                // The paths are measured, not guessed, and both spellings have
                // been seen in the wild.
                match event
                    .pointer("/delta/text")
                    .or_else(|| event.pointer("/event/delta/text"))
                    .and_then(serde_json::Value::as_str)
                {
                    Some(text) if !text.is_empty() => {
                        saw_delta = true;
                        said.push_str(text);
                        Some(Speech::Delta(text.to_string()))
                    }
                    _ => None,
                }
            }

            CliEvent::Assistant { message } => {
                let mut emitted = None;
                for block in message.content {
                    match block {
                        ContentBlock::Text { text } if !saw_delta && !text.is_empty() => {
                            said.push_str(&text);
                            emitted = Some(Speech::Delta(text));
                        }
                        ContentBlock::ToolUse { name, input } => {
                            let _ = tx.send(Speech::ToolUse { name, input }).await;
                        }
                        _ => {}
                    }
                }
                emitted
            }

            CliEvent::User { .. } => None,

            CliEvent::RateLimit { rate_limit_info } => Some(Speech::Quota {
                utilization: rate_limit_info.utilization,
                warning: rate_limit_info.is_warning(),
                window: rate_limit_info.window,
            }),

            CliEvent::Result(result) => {
                let text = std::mem::take(&mut said);
                saw_delta = false;
                // Cleared at every `result`, ours or not: a flag left standing
                // by an interrupt that landed after the turn had already ended
                // would otherwise misread the *next* real failure.
                let cut = interrupting.swap(false, Ordering::SeqCst);
                if cut && result.subtype != "success" {
                    Some(Speech::Interrupted(text))
                } else if result.is_error {
                    Some(Speech::Failed(if result.result.is_empty() {
                        result.subtype
                    } else {
                        result.result
                    }))
                } else {
                    Some(Speech::Finished {
                        // The CLI's own `result` is the complete answer; the
                        // accumulated deltas are the fallback for a turn that
                        // streamed and reported nothing.
                        text: if result.result.is_empty() {
                            text
                        } else {
                            result.result
                        },
                        usage: Usage {
                            input_tokens: result.usage.input_tokens,
                            output_tokens: result.usage.output_tokens,
                            cache_read_input_tokens: result.usage.cache_read_input_tokens,
                            cache_creation_input_tokens: result.usage.cache_creation_input_tokens,
                            total_cost_usd: result.total_cost_usd,
                        },
                    })
                }
            }
        };

        if let Some(event) = out
            && tx.send(event).await.is_err()
        {
            // Nobody is listening any more. The process is killed on drop.
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args_of(config: &Config) -> String {
        arguments(config).join(" ")
    }

    /// The fault that is invisible: an alias resolving a generation behind.
    #[test]
    fn the_model_is_named_in_full() {
        let args = arguments(&Config::default());
        let i = args.iter().position(|a| a == "--model").unwrap();
        assert_eq!(args[i + 1], "claude-haiku-5-5");
        assert!(!args[i + 1].is_empty() && args[i + 1].contains('-'));
    }

    /// Each of these was measured costing something: 916 ms of connectors, a
    /// cache missed every turn, 1.5 s of reasoning.
    #[test]
    fn the_flags_that_were_paid_for_are_all_there() {
        let args = args_of(&Config::default());
        assert!(args.contains("--strict-mcp-config"));
        assert!(args.contains("--exclude-dynamic-system-prompt-sections"));
        assert!(args.contains("--include-partial-messages"));
        assert!(args.contains("--effort low"));
        assert!(args.contains("--no-session-persistence"));
    }

    /// `--bare` would force API-key authentication, and the rule of 2026-10-24
    /// is that this post is never billed per token.
    #[test]
    fn nothing_forces_api_key_authentication() {
        assert!(!args_of(&Config::default()).contains("--bare"));
    }

    /// Decision 117: the skills line joins the rules, ahead of the persona.
    #[test]
    fn the_skills_line_sits_between_the_rules_and_the_persona() {
        let built = prompt_with("Appelle-moi Louis.", Some("Tes tâches déléguées ont aussi accès à : agenda."));
        let rules = built.find("NE DIS RIEN").unwrap();
        let skills = built.find("accès à : agenda").unwrap();
        let persona = built.find("Appelle-moi").unwrap();
        assert!(rules < skills && skills < persona, "{built}");
        // Absent, it costs nothing at all.
        assert_eq!(prompt_with("", None), SYSTEM_PROMPT);
        assert_eq!(prompt_with("", Some("  ")), SYSTEM_PROMPT);
    }

    #[test]
    fn without_a_catalogue_it_has_words_and_nothing_else() {
        let args = arguments(&Config::default());
        let i = args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(args[i + 1], "");
        assert!(!args.iter().any(|a| a == "--mcp-config"));
    }

    #[test]
    fn with_a_catalogue_it_gets_exactly_those_tools() {
        let config = Config {
            mcp_config: Some("{\"mcpServers\":{}}".into()),
            allowed_tools: vec!["mcp__aria__delegate_task".into()],
            ..Default::default()
        };
        let args = arguments(&config);
        let i = args.iter().position(|a| a == "--tools").unwrap();
        assert_eq!(args[i + 1], "mcp__aria__delegate_task");
        assert!(args.iter().any(|a| a == "--mcp-config"));
    }

    /// Measured live on 2026-10-26: with `--tools` alone the model called
    /// `delegate_task` and the CLI refused it, then said so out loud. Making a
    /// tool exist and making it runnable are two different questions, and the
    /// second one is the relay's.
    #[test]
    fn a_tool_that_exists_is_also_one_it_may_run() {
        let config = Config {
            allowed_tools: vec!["mcp__aria__delegate_task".into()],
            permission_tool: Some("mcp__aria__approve".into()),
            ..Default::default()
        };
        let args = arguments(&config);
        let i = args
            .iter()
            .position(|a| a == "--permission-prompt-tool")
            .unwrap();
        assert_eq!(args[i + 1], "mcp__aria__approve");
        // And never the blunt instruments, whatever the symptom. The first was
        // tried on the way here and is the wrong lever; the second is excluded
        // for good.
        let line = args.join(" ");
        assert!(!line.contains("--allowed-tools"), "{line}");
        assert!(!line.contains("dangerously"), "{line}");
        assert!(!line.contains("bypassPermissions"), "{line}");
    }

    /// No relay named, no flag: a `--permission-prompt-tool` pointing at
    /// nothing would make every tool call fail, which is worse than having no
    /// tools at all.
    #[test]
    fn without_a_relay_the_flag_is_absent() {
        let args = arguments(&Config::default());
        assert!(!args.iter().any(|a| a == "--permission-prompt-tool"));
    }

    /// A persona is added to the rules, never put in their place.
    #[test]
    fn a_persona_never_displaces_the_rules_that_make_it_work() {
        let built = prompt("Tutoie-moi. Je m'appelle Louis.");
        // Everything the mechanism depends on survives, and comes first.
        assert!(built.starts_with(SYSTEM_PROMPT));
        assert!(built.contains("répète-la telle quelle"));
        assert!(built.contains("NE DIS RIEN"));
        // And the user's lines are there, after, marked as subordinate.
        assert!(built.contains("Tutoie-moi"));
        assert!(built.contains("ne lèvent aucune règle"));
        // An empty persona costs nothing at all — it is sent every turn.
        assert_eq!(prompt("   "), SYSTEM_PROMPT);
    }

    /// Everything the agent says is spoken, so the prompt travels with it.
    #[test]
    fn the_voice_prompt_replaces_the_coding_one() {
        let args = arguments(&Config::default());
        let i = args.iter().position(|a| a == "--system-prompt").unwrap();
        assert!(args[i + 1].contains("assistant vocal"));
        assert!(!args.iter().any(|a| a == "--append-system-prompt"));
    }
}
