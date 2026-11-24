//! One session, one process.
//!
//! No multiplexing — epic 01 is explicit, and the reason shows up immediately
//! in epic 02: a permission request has to be routed back to *its* session, and
//! two sessions sharing a process would share a stdout.
//!
//! ```text
//!   claude -p --input-format stream-json --output-format stream-json --verbose
//! ```
//!
//! `--verbose` is **mandatory** with `--output-format stream-json`. One JSON
//! object per line, both ways; closing stdin ends the session cleanly.
//!
//! # Three things here are not obvious and each one is a bug avoided
//!
//! **stderr is drained.** A pipe nobody reads fills, and a process whose stderr
//! pipe is full blocks forever — which would look exactly like a session gone
//! quiet, and decision 8 would then mark it "sans réponse" rather than report
//! the real fault.
//!
//! **The reader task owns stdout and nothing else touches it.** Lines are
//! parsed one at a time; a line that will not parse becomes
//! [`ProviderEvent::Unknown`] and the loop continues.
//!
//! **`stop()` closes stdin before killing.** Closing is how the CLI finishes
//! what it was doing and writes its session state; killing is for a process
//! that ignores that.

use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::mpsc;

use super::protocol::{self, CliEvent, ContentBlock, Parsed, SystemEvent};
use crate::providers::{AgentSession, LaunchSpec, McpToolNaming, ProviderEvent, Usage};

/// How many events may queue before the producer waits.
///
/// Bounded deliberately: an unbounded channel in front of a CLI that can emit
/// thousands of partial-message events turns a slow consumer into unbounded
/// memory. Back-pressure onto the reader is the right failure.
const CHANNEL_CAPACITY: usize = 256;

pub struct ClaudeSession {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    native_id: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

#[async_trait::async_trait]
impl AgentSession for ClaudeSession {
    async fn send(&mut self, text: &str) -> anyhow::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("la session est fermée"))?;
        let line = protocol::user_message(text);
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        // Dropping stdin closes it, which is the documented clean shutdown: the
        // CLI finishes the turn and writes its session state, so the session
        // stays resumable.
        self.stdin = None;

        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        // Give it a moment to go on its own before insisting.
        let graceful = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
        match graceful {
            Ok(Ok(_)) => Ok(()),
            // It ignored the closed stdin. Decision 8 is about never killing a
            // session *automatically on silence*; this is the user having asked
            // for it to stop, which is a different thing.
            _ => {
                child.kill().await?;
                Ok(())
            }
        }
    }

    fn native_id(&self) -> Option<String> {
        self.native_id.lock().unwrap().clone()
    }
}

/// Build the argument list.
///
/// Separated from spawning so it can be asserted on without running anything —
/// the flags are the security surface, and `bypassPermissions` never appearing
/// is worth a test rather than a reading.
pub fn arguments(spec: &LaunchSpec) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-p".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        // Mandatory with stream-json output. Without it the CLI refuses.
        "--verbose".into(),
    ];

    args.push("--permission-mode".into());
    args.push(super::permission_mode(spec.profile).into());

    if let Some(id) = &spec.resume {
        // Epic 05: never deduce resumability from a date — try it and handle
        // the failure. The user's own `cleanupPeriodDays` can be one day.
        args.push("--resume".into());
        args.push(id.clone());
    }

    if let Some(model) = &spec.model {
        // In full, never an alias: `docs/cli/claude-code-cli.md` §8 measured `--model haiku`
        // resolving a generation behind what was asked for, silently.
        args.push("--model".into());
        args.push(model.clone());
    }

    if let Some(dir) = &spec.cwd {
        // Grants file access to that directory, which is why it is explicit
        // rather than inherited from wherever ARIA happens to be running.
        args.push("--add-dir".into());
        args.push(dir.display().to_string());
    }

    // Hard ceilings, applied by the CLI. Flags, not code — and the reason
    // decision 8 can afford never to kill a quiet session.
    if let Some(turns) = spec.max_turns {
        args.push("--max-turns".into());
        args.push(turns.to_string());
    }
    if let Some(budget) = spec.max_budget_usd {
        args.push("--max-budget-usd".into());
        args.push(budget.to_string());
    }

    // Epic 02. While this is absent the session runs unrelayed, which means the
    // CLI refuses whatever would have asked — safe, not permissive.
    if let Some(tool) = &spec.relay {
        args.push("--permission-prompt-tool".into());
        // Rendered here and not upstream: `mcp__server__tool` is **this
        // engine's** convention, and a core that spelled it would be a core
        // that knows one engine. See `providers::McpToolNaming`.
        args.push(tool.render(McpToolNaming::DoubleUnderscore));
    }
    if !spec.mcp_servers.is_empty() {
        args.push("--mcp-config".into());
        args.push(mcp_config(&spec.mcp_servers));
        // **No `--strict-mcp-config` here, and that is decision 105.**
        //
        // It used to be pushed alongside, so a delegated session saw ARIA's
        // servers and nothing else. The user overturned it on 2026-11-12 with
        // an argument the flag cannot express: a sub-agent is allowed
        // capabilities the voice post is not. The voice post is a dispatcher
        // under a byte budget whose every word is spoken; a delegated session
        // is the one doing the work, and the connectors the user configured —
        // `docs/cli/claude-code-cli.md` §8 measured five, Gmail and Drive among them — are the
        // reach they configured them for.
        //
        // What does **not** change: every call still goes through
        // `--permission-prompt-tool`, so the user decides. Two consequences
        // worth knowing rather than rediscovering — an MCP tool marked
        // "requires user interaction" cannot be approved by a relay at all
        // (`docs/cli/claude-code-cli.md` §4), and a server of the user's own named `aria` would
        // now collide with ours, which is why the memory post is called
        // `aria_memory` (see `crate::mcp::subagent::SERVER_NAME`).
    }

    // Decision 106: the standing instructions every delegated session gets,
    // read by the CLI rather than passed as text. Appended to the CLI's own
    // coding-agent prompt — which is right here and wrong on the voice post,
    // where the prompt is replaced instead.
    if let Some(file) = &spec.instructions_file {
        args.push("--append-system-prompt-file".into());
        args.push(file.display().to_string());
    }

    args
}

/// The `--mcp-config` payload, in this engine's shape.
///
/// `type: "http"` — read off the installed CLI on 2026-10-25 rather than taken
/// from a guide: `claude mcp add --transport http` wrote exactly this shape and
/// `claude mcp get` printed it back as `Type: http`. Third-party guides
/// disagree about `type` versus `transport`.
///
/// Here since 2026-11-12, having lived in `crate::transport` — which is to say
/// one engine's configuration format inside engine-agnostic code. The core now
/// hands over `(name, url)` pairs and each provider writes its own file.
fn mcp_config(servers: &[crate::providers::McpServer]) -> String {
    let mut table = serde_json::Map::new();
    for server in servers {
        table.insert(
            server.name.clone(),
            serde_json::json!({ "type": "http", "url": server.url }),
        );
    }
    serde_json::json!({ "mcpServers": table }).to_string()
}

/// Spawn the process and start pumping its output.
pub async fn launch(
    command: &str,
    spec: LaunchSpec,
) -> anyhow::Result<(Box<dyn AgentSession>, mpsc::Receiver<ProviderEvent>)> {
    let args = arguments(&spec);
    tracing::info!("launching {command} {}", args.join(" "));

    let mut builder = tokio::process::Command::new(command);
    builder.args(&args);
    // Run the process IN the directory, not merely with access to it.
    // `--add-dir` grants access; it does not move the session, so without this
    // a relative path in a prompt resolves wherever ARIA happens to be running
    // — which, for a background service, is nowhere anybody meant.
    if let Some(dir) = &spec.cwd {
        builder.current_dir(dir);
    }
    let mut child = builder
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("`{command}` n'a pas pu être lancé : {e}"))?;

    let stdin = child.stdin.take().expect("stdin was piped");
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    let native_id = std::sync::Arc::new(std::sync::Mutex::new(None));

    // stderr is drained and logged. An unread pipe fills, and a process blocked
    // on a full stderr looks exactly like a session that stopped answering.
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::warn!("claude stderr: {line}");
        }
    });

    let reader_id = std::sync::Arc::clone(&native_id);
    let reader_tx = tx.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }
            for event in translate(&line, &reader_id) {
                if reader_tx.send(event).await.is_err() {
                    // Nobody is listening any more: the session was dropped.
                    return;
                }
            }
        }
    });

    let mut session = ClaudeSession {
        child: Some(child),
        stdin: Some(stdin),
        native_id,
    };

    // A session opened with nothing to say is legitimate — the main agent will
    // speak to it later — so an empty prompt sends nothing rather than an empty
    // turn, which the CLI would bill for and answer with a shrug.
    if !spec.prompt.trim().is_empty() {
        session.send(&spec.prompt).await?;
    }

    Ok((Box::new(session), rx))
}

/// One line in, zero or more normalised events out.
fn translate(line: &str, native_id: &std::sync::Mutex<Option<String>>) -> Vec<ProviderEvent> {
    let parsed = protocol::parse_line(line);
    let event = match parsed {
        Parsed::Unknown(raw) => {
            tracing::debug!("unmodelled line from the CLI: {raw}");
            return vec![ProviderEvent::Unknown { raw }];
        }
        Parsed::Known(event) => event,
    };

    match event {
        CliEvent::System(SystemEvent::Init {
            session_id,
            tools,
            mcp_servers,
        }) => {
            *native_id.lock().unwrap() = Some(session_id.clone());
            vec![ProviderEvent::Started {
                native_id: session_id,
                tools,
                mcp_servers: server_names(&mcp_servers),
            }]
        }
        CliEvent::System(SystemEvent::Other) => vec![],

        CliEvent::Assistant { message } => message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } if !text.is_empty() => {
                    Some(ProviderEvent::TextDelta(text.clone()))
                }
                ContentBlock::Thinking { .. } => Some(ProviderEvent::Thinking),
                ContentBlock::ToolUse { name, input } => Some(ProviderEvent::ToolUse {
                    name: name.clone(),
                    command: ContentBlock::command(input),
                }),
                _ => None,
            })
            .collect(),

        CliEvent::User { message } => message
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult { is_error } => Some(ProviderEvent::ToolResult {
                    // The CLI reports tool results on a user turn, which is why
                    // this arm is here and not under `Assistant`.
                    name: String::new(),
                    ok: !is_error,
                }),
                _ => None,
            })
            .collect(),

        // Partial streaming. Already covered by the complete `assistant`
        // message, so it is dropped rather than doubled — turning it into text
        // deltas as well would print every answer twice.
        CliEvent::StreamEvent { .. } => vec![],

        CliEvent::RateLimit { rate_limit_info } => vec![ProviderEvent::Quota {
            utilization: rate_limit_info.utilization,
            warning: rate_limit_info.is_warning(),
            window: rate_limit_info.window,
        }],

        CliEvent::Result(result) => {
            if !result.session_id.is_empty() {
                *native_id.lock().unwrap() = Some(result.session_id.clone());
            }
            if !result.permission_denials.is_empty() {
                tracing::info!(
                    "the CLI refused {} request(s) on its own",
                    result.permission_denials.len()
                );
            }
            if result.is_error {
                vec![ProviderEvent::Failed {
                    message: if result.result.is_empty() {
                        result.subtype.clone()
                    } else {
                        result.result.clone()
                    },
                }]
            } else {
                vec![ProviderEvent::Finished {
                    summary: result.result.clone(),
                    usage: Usage {
                        input_tokens: result.usage.input_tokens,
                        output_tokens: result.usage.output_tokens,
                        cache_read_input_tokens: result.usage.cache_read_input_tokens,
                        cache_creation_input_tokens: result.usage.cache_creation_input_tokens,
                        total_cost_usd: result.total_cost_usd,
                    },
                }]
            }
        }
    }
}

/// `mcp_servers` comes back as names, or as objects carrying a status.
///
/// The status is kept, and it earned its place: a server listed by name alone
/// reads as connected, and on 2026-10-25 that misreading had us conclude the
/// relay was wired while the CLI was in fact reporting it as `pending`. A name
/// without its status is a log line that lies.
fn server_names(value: &serde_json::Value) -> Vec<String> {
    match value {
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| match item {
                serde_json::Value::String(s) => s.clone(),
                other => {
                    let name = other.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                    match other.get("status").and_then(|s| s.as_str()) {
                        Some(status) => format!("{name} ({status})"),
                        None => name.to_string(),
                    }
                }
            })
            .collect(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::Profile;

    fn spec(profile: Profile) -> LaunchSpec {
        LaunchSpec {
            profile,
            ..Default::default()
        }
    }

    #[test]
    fn stream_json_needs_verbose_and_it_is_always_there() {
        // Not optional: the CLI refuses `--output-format stream-json` without
        // it, so a session would fail to start at all.
        let args = arguments(&spec(Profile::Normal));
        assert!(args.contains(&"--verbose".to_string()));
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"stream-json".to_string()));
    }

    #[test]
    fn no_profile_can_produce_a_bypass_flag() {
        // Decision 3: excluded definitively. Asserted on the actual argument
        // list rather than on the mapping, because the argument list is what
        // reaches the process.
        for profile in Profile::ALL {
            let args = arguments(&spec(profile)).join(" ");
            assert!(!args.contains("bypassPermissions"), "{args}");
            assert!(!args.contains("dontAsk"), "{args}");
        }
    }

    #[test]
    fn each_profile_sends_its_own_native_mode() {
        let args = arguments(&spec(Profile::Plan));
        let i = args.iter().position(|a| a == "--permission-mode").unwrap();
        assert_eq!(args[i + 1], "plan");

        let args = arguments(&spec(Profile::Autonomous));
        let i = args.iter().position(|a| a == "--permission-mode").unwrap();
        assert_eq!(args[i + 1], "auto");
    }

    /// The inverse of what this test asserted until 2026-11-12.
    ///
    /// It used to require `--strict-mcp-config` alongside every
    /// `--mcp-config`, so a delegated session saw ARIA's servers and nothing
    /// else. Decision 105 overturned it: a sub-agent is allowed capabilities
    /// the voice post is not, and the connectors the user configured are the
    /// reach they configured them for.
    ///
    /// Kept as a test rather than deleted, because the flag is a security
    /// surface and its **absence** is now the deliberate state. A future
    /// "obvious" tightening would be reverting a decision, and this is what
    /// says so.
    #[test]
    fn a_delegated_session_is_not_restricted_to_our_own_mcp_servers() {
        let mut s = spec(Profile::Normal);
        s.mcp_servers = crate::transport::session_servers("jeton-de-test");
        let args = arguments(&s);
        assert!(args.contains(&"--mcp-config".to_string()));
        assert!(
            !args.contains(&"--strict-mcp-config".to_string()),
            "décision 105 : les connecteurs de l'utilisateur restent chargés"
        );
    }

    /// The voice post is the other half of the same decision, and the contrast
    /// is the point: it **is** restricted, because its prompt is replaced, its
    /// tools are a list and every word it writes is spoken.
    #[test]
    fn the_voice_post_on_the_other_hand_is_restricted() {
        let args = crate::agent::session::arguments(&crate::agent::session::Config::default());
        assert!(args.contains(&"--strict-mcp-config".to_string()));
    }

    /// Decision 106: the file is pushed only when there is one, and as a path.
    #[test]
    fn the_standing_instructions_travel_as_a_file_and_only_when_there_is_one() {
        let args = arguments(&spec(Profile::Normal));
        assert!(!args.contains(&"--append-system-prompt-file".to_string()));

        let mut s = spec(Profile::Normal);
        s.instructions_file = Some(std::path::PathBuf::from("/tmp/AGENTS.md"));
        let args = arguments(&s);
        let i = args
            .iter()
            .position(|a| a == "--append-system-prompt-file")
            .expect("le drapeau est poussé");
        // The path, not the text. Verified on CLI 2.1.312 on 2026-11-12: the
        // CLI reads the file, which is what keeps an arbitrarily long one off
        // a command line Windows bounds at 32 767 characters.
        assert!(args[i + 1].ends_with("AGENTS.md"), "{}", args[i + 1]);
    }

    /// The core **declares ARIA's MCP server to the CLI**, rather than hoping
    /// the user has configured it. That is what makes `mcp__aria__show_tile`
    /// and `mcp__aria__approve` exist in a delegated session at all: the server
    /// name in this payload is the first half of every tool name.
    ///
    /// Asserted on the payload and not only on the flag, because a config that
    /// named the server anything else would still pass the test above and
    /// would publish tools under names nothing in the core recognises.
    #[test]
    fn the_session_is_handed_arias_own_server_under_the_name_the_tools_use() {
        let mut s = spec(Profile::Normal);
        s.mcp_servers = crate::transport::session_servers("jeton-de-test");
        let args = arguments(&s);

        let at = args
            .iter()
            .position(|a| a == "--mcp-config")
            .expect("le serveur MCP d'ARIA est déclaré à la CLI");
        let config = &args[at + 1];
        assert!(
            config.contains(&format!("\"{}\"", crate::mcp::SERVER_NAME)),
            "{config}"
        );
        assert!(
            config.contains(&crate::transport::relay_url("jeton-de-test")),
            "{config}"
        );
        // `type: "http"`, read off the installed CLI on 2026-10-25 rather than
        // taken from a guide. It lives here since 2026-11-12: it is this
        // engine's configuration format, and the core no longer writes it.
        assert!(config.contains("\"type\":\"http\""), "{config}");
    }

    /// The relay is handed over **unnamed**, and this engine spells it.
    ///
    /// The leak sealed on 2026-11-12: `mcp__server__tool` is Claude Code's
    /// convention, and a core that wrote it would be a core that knows one
    /// engine. The failure would have been silent on a second engine — a relay
    /// flag naming a tool that does not exist, so every permission refused.
    #[test]
    fn this_engine_spells_the_relay_tool_itself() {
        let mut s = spec(Profile::Normal);
        s.relay = Some(crate::mcp::relay_tool());
        let args = arguments(&s);

        let at = args
            .iter()
            .position(|a| a == "--permission-prompt-tool")
            .expect("le relais est poussé");
        assert_eq!(args[at + 1], "mcp__aria__approve");
    }

    /// And an engine that relays is never also handed a frozen list.
    ///
    /// Claude Code can ask, so the supervisor gives it a relay and no policy
    /// (decision 109). A session judged twice would be judged by the stricter
    /// of the two, silently.
    #[test]
    fn an_engine_that_can_ask_ignores_a_static_policy() {
        use crate::providers::AgentProvider;
        assert!(
            super::super::ClaudeCode::new()
                .capabilities()
                .relays_permissions
        );
    }

    #[test]
    fn no_mcp_config_means_no_strict_flag_either() {
        // `--strict-mcp-config` alone would load nothing, which is fine, but
        // pairing them keeps the intent readable at the call site.
        let args = arguments(&spec(Profile::Normal));
        assert!(!args.contains(&"--strict-mcp-config".to_string()));
    }

    #[test]
    fn resuming_passes_the_native_id() {
        let mut s = spec(Profile::Normal);
        s.resume = Some("abc-123".into());
        let args = arguments(&s);
        let i = args.iter().position(|a| a == "--resume").unwrap();
        assert_eq!(args[i + 1], "abc-123");
    }

    #[test]
    fn the_hard_ceilings_are_flags_not_code() {
        let mut s = spec(Profile::Normal);
        s.max_turns = Some(12);
        s.max_budget_usd = Some(0.5);
        let args = arguments(&s).join(" ");
        assert!(args.contains("--max-turns 12"));
        assert!(args.contains("--max-budget-usd 0.5"));
    }

    #[test]
    fn init_is_translated_and_remembers_the_id_for_a_resume() {
        let id = std::sync::Mutex::new(None);
        let events = translate(
            r#"{"type":"system","subtype":"init","session_id":"s-1","tools":["Bash"],"mcp_servers":[]}"#,
            &id,
        );
        assert!(matches!(events[0], ProviderEvent::Started { .. }));
        assert_eq!(id.lock().unwrap().as_deref(), Some("s-1"));
    }

    #[test]
    fn a_tool_use_carries_the_command_the_registry_will_need() {
        let id = std::sync::Mutex::new(None);
        let line = r#"{"type":"assistant","message":{"content":[
            {"type":"tool_use","name":"Bash","input":{"command":"git status"}}]}}"#;
        match &translate(line, &id)[0] {
            ProviderEvent::ToolUse { name, command } => {
                assert_eq!(name, "Bash");
                assert_eq!(command.as_deref(), Some("git status"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn partial_streaming_is_dropped_so_answers_are_not_printed_twice() {
        let id = std::sync::Mutex::new(None);
        let line = r#"{"type":"stream_event","event":{"type":"content_block_delta"}}"#;
        assert!(translate(line, &id).is_empty());
    }

    #[test]
    fn a_failed_result_becomes_a_failure_not_a_completion() {
        let id = std::sync::Mutex::new(None);
        let line = r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":""}"#;
        match &translate(line, &id)[0] {
            ProviderEvent::Failed { message } => assert_eq!(message, "error_max_turns"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_result_carries_the_usage_counters() {
        let id = std::sync::Mutex::new(None);
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"fait",
            "session_id":"s-2","usage":{"input_tokens":18,"output_tokens":53,
            "cache_read_input_tokens":41850,"cache_creation_input_tokens":2494},
            "total_cost_usd":0.001}"#;
        match &translate(line, &id)[0] {
            ProviderEvent::Finished { summary, usage } => {
                assert_eq!(summary, "fait");
                assert_eq!(usage.output_tokens, 53);
                assert_eq!(usage.cache_read_input_tokens, 41850);
            }
            other => panic!("{other:?}"),
        }
        // The result also carries the id, which matters when a resume produced
        // a new one.
        assert_eq!(id.lock().unwrap().as_deref(), Some("s-2"));
    }

    #[test]
    fn an_unknown_line_is_reported_and_does_not_stop_anything() {
        let id = std::sync::Mutex::new(None);
        match &translate("{\"type\":\"something_new\"}", &id)[0] {
            ProviderEvent::Unknown { raw } => assert!(raw.contains("something_new")),
            other => panic!("{other:?}"),
        }
    }
}
