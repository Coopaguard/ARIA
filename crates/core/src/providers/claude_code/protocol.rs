//! The `stream-json` wire format, typed.
//!
//! One JSON object per line, on stdin and stdout. The shapes below come from
//! `docs/cli/claude-code-cli.md` §8, which was written by **running**
//! CLI 2.1.165 rather than from the documentation — so they are what the CLI
//! actually emits, not what it is said to emit.
//!
//! # The `Unknown` variant is the point
//!
//! Epic 01 asks for it by name. The CLI adds event types between versions, and
//! `docs/cli/claude-code-cli.md` opens by saying the CLI moves fast. A parser that fails on an
//! unrecognised line is a bridge that breaks on an update nobody asked for — so
//! every variant below is a best effort, and anything unmatched lands in
//! [`CliEvent::Unknown`] with its raw text, gets logged, and the session carries
//! on.
//!
//! For the same reason every struct is `#[serde(default)]` where it can be:
//! a field that disappears must not take the event with it.

use serde::Deserialize;

/// One line of the CLI's stdout.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum CliEvent {
    /// Lifecycle, with `subtype` saying which.
    #[serde(rename = "system")]
    System(SystemEvent),

    /// A complete assistant message — what arrives without
    /// `--include-partial-messages`.
    #[serde(rename = "assistant")]
    Assistant { message: ApiMessage },

    /// Echoed user input, and tool results, which the CLI sends back as a user
    /// turn.
    #[serde(rename = "user")]
    User { message: ApiMessage },

    /// The envelope around the API's own streaming events.
    #[serde(rename = "stream_event")]
    StreamEvent { event: serde_json::Value },

    /// End of turn, with the accounting.
    #[serde(rename = "result")]
    Result(ResultEvent),

    /// Where the subscription stands, pushed unasked.
    ///
    /// Decision 36 says the stream carries the quota instrument, and this is
    /// it: the CLI has no `usage` subcommand, so there is nowhere else to read
    /// this from. Observed on 2026-10-26 arriving mid-turn, unprompted, once
    /// the seven-day window crossed its warning threshold.
    #[serde(rename = "rate_limit_event")]
    RateLimit {
        #[serde(default)]
        rate_limit_info: RateLimitInfo,
    },
}

/// The subscription window, as the CLI reports it.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct RateLimitInfo {
    /// `allowed`, `allowed_warning`, and whatever else it grows.
    #[serde(default)]
    pub status: String,
    /// Which window: `seven_day`, and others the CLI may add.
    #[serde(default, rename = "rateLimitType")]
    pub window: String,
    /// Fraction of the window used, 0 to 1.
    #[serde(default)]
    pub utilization: f64,
    /// Unix seconds at which the window rolls over.
    #[serde(default, rename = "resetsAt")]
    pub resets_at: Option<i64>,
    #[serde(default, rename = "isUsingOverage")]
    pub using_overage: bool,
    /// The threshold just crossed, when one was.
    #[serde(default, rename = "surpassedThreshold")]
    pub surpassed_threshold: Option<f64>,
}

impl RateLimitInfo {
    /// Worth telling the user about, as opposed to worth logging.
    ///
    /// The main agent spends the **same pot** as every delegated session, and
    /// what exhausts it is the number of turns rather than their size. So the
    /// warning matters long before the refusal does: it is what should push the
    /// local tier forward, and the plan's return condition is written against
    /// exactly this number.
    pub fn is_warning(&self) -> bool {
        self.status != "allowed" || self.utilization >= 0.75 || self.using_overage
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "subtype")]
pub enum SystemEvent {
    /// The session is up. This is where the native id comes from.
    #[serde(rename = "init")]
    Init {
        #[serde(default)]
        session_id: String,
        #[serde(default)]
        tools: Vec<String>,
        /// Shape varies — sometimes names, sometimes objects — so it stays raw
        /// and is flattened to names at the call site.
        #[serde(default)]
        mcp_servers: serde_json::Value,
    },
    /// Anything else the CLI calls a system event.
    #[serde(other)]
    Other,
}

/// An Anthropic API message, cut down to the blocks we act on.
#[derive(Debug, Deserialize)]
pub struct ApiMessage {
    #[serde(default)]
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },

    #[serde(rename = "thinking")]
    Thinking {
        #[serde(default)]
        thinking: String,
    },

    #[serde(rename = "tool_use")]
    ToolUse {
        name: String,
        /// The tool's arguments, raw.
        ///
        /// Kept raw rather than typed per tool: epic 02 needs **the command
        /// itself**, and every tool spells its arguments differently. Narrowing
        /// this to a known set would silently drop the command for any tool we
        /// had not thought of — which is exactly the tool a pattern registry
        /// most needs to see.
        #[serde(default)]
        input: serde_json::Value,
    },

    #[serde(rename = "tool_result")]
    ToolResult {
        #[serde(default)]
        is_error: bool,
    },

    /// A block type we do not model. Not an error.
    #[serde(other)]
    Other,
}

impl ContentBlock {
    /// The command inside a `tool_use`, when the tool has one.
    ///
    /// Tools spell it differently and new ones will spell it differently again,
    /// so this tries the known keys and gives up gracefully. Giving up means a
    /// request the registry cannot match on, which is **asked at medium risk** —
    /// the safe direction.
    pub fn command(input: &serde_json::Value) -> Option<String> {
        for key in ["command", "cmd", "script"] {
            if let Some(value) = input.get(key).and_then(|v| v.as_str()) {
                return Some(value.to_string());
            }
        }
        // File tools: the path is the closest thing to a command, and it is
        // what a pattern would be written against.
        for key in ["file_path", "path", "notebook_path"] {
            if let Some(value) = input.get(key).and_then(|v| v.as_str()) {
                return Some(value.to_string());
            }
        }
        None
    }
}

#[derive(Debug, Deserialize)]
pub struct ResultEvent {
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub usage: UsageFields,
    #[serde(default)]
    pub total_cost_usd: Option<f64>,
    /// Requests the CLI refused on its own. Worth surfacing: with no relay
    /// wired, this is where "it did nothing and said nothing" becomes visible.
    #[serde(default)]
    pub permission_denials: Vec<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UsageFields {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
}

/// Parse one line, or say it is unknown.
///
/// Never returns an error. A line we cannot read is a line to log and move
/// past — see the module docs.
pub fn parse_line(line: &str) -> Parsed {
    match serde_json::from_str::<CliEvent>(line) {
        Ok(event) => Parsed::Known(event),
        Err(_) => Parsed::Unknown(line.to_string()),
    }
}

pub enum Parsed {
    Known(CliEvent),
    Unknown(String),
}

/// The one input shape the CLI accepts, built for us.
///
/// Measured, not guessed: `docs/cli/claude-code-cli.md` §8 records this exact envelope working,
/// one per line.
pub fn user_message(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": text }]
        }
    })
    .to_string()
}

/// Stop the turn in progress, keeping the process and its context.
///
/// Measured on 2026-11-23 against CLI 2.1.323 (`docs/cli/claude-code-cli.md` §8): the CLI answers
/// `control_response` / `success` within milliseconds, keeps the partial
/// answer in its context followed by a synthetic `[Request interrupted by
/// user]` user turn, and ends the turn with a `result` of subtype
/// `error_during_execution`. A user message sent right after starts a fresh
/// turn at once instead of queueing behind the old one — which is the point.
pub fn interrupt_request(request_id: &str) -> String {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": { "subtype": "interrupt" }
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The envelope measured on 2026-11-23. A typo here is silent: the CLI
    /// ignores what it does not recognise and the turn simply runs on.
    #[test]
    fn the_interrupt_is_the_measured_envelope() {
        let line: serde_json::Value =
            serde_json::from_str(&interrupt_request("aria-1")).expect("json");
        assert_eq!(line["type"], "control_request");
        assert_eq!(line["request_id"], "aria-1");
        assert_eq!(line["request"]["subtype"], "interrupt");
    }

    #[test]
    fn an_unknown_event_type_does_not_fail_the_parse() {
        // The case this exists for: the CLI ships a new event type and the
        // bridge has to survive it.
        match parse_line(r#"{"type":"telemetry_v2","whatever":1}"#) {
            Parsed::Unknown(raw) => assert!(raw.contains("telemetry_v2")),
            Parsed::Known(_) => panic!("should not have matched a known variant"),
        }
    }

    #[test]
    fn a_line_that_is_not_json_does_not_fail_the_parse_either() {
        match parse_line("Warning: something on stdout that is not JSON") {
            Parsed::Unknown(_) => {}
            Parsed::Known(_) => panic!("not JSON"),
        }
    }

    #[test]
    fn init_carries_the_session_id_a_resume_will_need() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc-123","tools":["Bash"],"mcp_servers":[]}"#;
        match parse_line(line) {
            Parsed::Known(CliEvent::System(SystemEvent::Init {
                session_id, tools, ..
            })) => {
                assert_eq!(session_id, "abc-123");
                assert_eq!(tools, vec!["Bash"]);
            }
            _ => panic!("init should parse"),
        }
    }

    #[test]
    fn an_unknown_system_subtype_still_parses_as_a_system_event() {
        // `docs/cli/claude-code-cli.md` §8 lists `status` and `thinking_tokens` among the subtypes,
        // and there will be more.
        match parse_line(r#"{"type":"system","subtype":"thinking_tokens","n":12}"#) {
            Parsed::Known(CliEvent::System(SystemEvent::Other)) => {}
            _ => panic!("an unmodelled subtype is still a system event"),
        }
    }

    #[test]
    fn the_command_is_pulled_out_of_a_tool_use_for_the_pattern_registry() {
        let input = serde_json::json!({ "command": "git status" });
        assert_eq!(ContentBlock::command(&input).as_deref(), Some("git status"));
    }

    #[test]
    fn a_file_tool_yields_its_path_rather_than_nothing() {
        let input = serde_json::json!({ "file_path": "D:/Code/ARIA/README.md" });
        assert_eq!(
            ContentBlock::command(&input).as_deref(),
            Some("D:/Code/ARIA/README.md")
        );
    }

    #[test]
    fn a_tool_whose_arguments_we_do_not_recognise_yields_no_command() {
        // Which means "asked, at medium risk" — the safe direction.
        let input = serde_json::json!({ "some_new_shape": { "nested": true } });
        assert_eq!(ContentBlock::command(&input), None);
    }

    #[test]
    fn the_input_envelope_is_the_one_the_cli_accepts() {
        let line = user_message("bonjour");
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["type"], "user");
        assert_eq!(value["message"]["role"], "user");
        assert_eq!(value["message"]["content"][0]["text"], "bonjour");
    }
}
