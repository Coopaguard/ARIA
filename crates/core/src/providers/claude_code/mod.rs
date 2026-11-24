//! Claude Code, driven as a subprocess over `stream-json`.
//!
//! The Claude Agent SDK exists only in Python and TypeScript, so the CLI is
//! the contract. Its verified behaviour — permission modes, rule matching,
//! the three hard limits of the relay, session flags, retention — is written
//! down in `docs/cli/claude-code-cli.md`, with sources and a survey
//! date. Do not restate those facts from memory.
//!
//! # Written rather than taken from a crate, and why
//!
//! Epic 01 asks for the community crates to be tried first. Checked on
//! 2026-10-25: `rusty_claw` is 0.1.0 with 32 downloads, last touched in
//! February; `cc-agent-sdk` is 0.1.7, last touched in March. Seven to eight
//! months behind a CLI that `docs/cli/claude-code-cli.md` opens by calling fast-moving, in 0.x,
//! with almost no adoption to shake bugs out. Depending on either would put a
//! stale wrapper between us and the one interface everything else rests on.
//! So: written here, against a CLI version we have run.
//!
//! # What this module does NOT do yet
//!
//! It does not relay permission requests — that is epic 02, and it needs an
//! MCP server. Until it lands, sessions launch with no `--permission-prompt-tool`.
//! That is **safe rather than permissive**: with no terminal and no relay, the
//! CLI refuses anything that would have asked. Useful work in `Plan` and read
//! paths, refusals elsewhere, and nothing silently allowed.

pub mod protocol;
pub mod session;

use crate::permissions::Profile;
use crate::providers::{
    AgentProvider, AgentSession, Availability, Capabilities, Connector, LaunchSpec, McpToolNaming,
    ModelChoice, ModelTier, NativeMode, ProfileSupport, ProviderEvent,
};

/// The identifier, in configuration and in the engine dropdown.
///
/// A constant since 2026-11-12 rather than a literal in `id()`, because the
/// catalogue and the configuration file both name it and a third spelling of
/// the same string is how a selector silently selects nothing.
pub const ID: &str = "claude-code";

/// The default command. Overridable for a CLI that is not on `PATH`.
pub const DEFAULT_COMMAND: &str = "claude";

/// Claude Code as an [`AgentProvider`].
///
/// No minimum version since 2026-11-19, by the user's choice: the CLI is
/// called, and a launch that fails reports why. `--version` is still asked,
/// but only to be shown.
pub struct ClaudeCode {
    command: String,
}

impl Default for ClaudeCode {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeCode {
    pub fn new() -> Self {
        Self {
            command: DEFAULT_COMMAND.to_string(),
        }
    }

    /// Point at a CLI somewhere other than `PATH`.
    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = command.into();
        self
    }

    pub fn command(&self) -> &str {
        &self.command
    }
}

/// The profile table, as epic 03 fixes it.
///
/// The explanations are French and are **shown**, because a profile whose
/// translation you cannot see is a trap. They describe what the CLI does, from
/// `docs/cli/claude-code-cli.md` §2 — not what we wish it did.
pub fn mapping() -> Vec<ProfileSupport> {
    let row = |profile, name: &str, explanation: &str| ProfileSupport {
        profile,
        native: Some(NativeMode {
            name: name.to_string(),
            explanation: explanation.to_string(),
        }),
    };
    vec![
        row(
            Profile::Plan,
            "plan",
            "lit et explore, n'édite rien avant qu'un plan soit approuvé",
        ),
        row(
            Profile::Strict,
            "default",
            "les lectures passent ; éditions, commandes shell et réseau sont demandées",
        ),
        row(
            Profile::Normal,
            "acceptEdits",
            "lectures, éditions et commandes de fichiers courantes dans le répertoire \
             de travail ; les autres commandes shell sont demandées",
        ),
        row(
            Profile::Autonomous,
            "auto",
            "presque tout passe, mais un classifieur relit chaque action avant exécution, \
             et les chemins protégés restent bloqués",
        ),
    ]
}

/// The native mode name for a profile.
///
/// `bypassPermissions` is reachable from none of them, by construction: there
/// is no profile that maps to it and no code path that can produce it.
pub fn permission_mode(profile: Profile) -> &'static str {
    match profile {
        Profile::Plan => "plan",
        Profile::Strict => "default",
        Profile::Normal => "acceptEdits",
        Profile::Autonomous => "auto",
    }
}

#[async_trait::async_trait]
impl AgentProvider for ClaudeCode {
    fn id(&self) -> &'static str {
        ID
    }

    fn display_name(&self) -> &'static str {
        "Claude Code"
    }

    fn profile_mapping(&self) -> Vec<ProfileSupport> {
        mapping()
    }

    /// Everything on, and every line of it verified on CLI 2.1.312.
    ///
    /// This is the only engine surveyed on 2026-11-12 that can **ask** —
    /// `--permission-prompt-tool` — which is what ARIA's whole permission
    /// architecture rests on. Gemini, Codex and Copilot offer static policy
    /// only, so a provider for any of them declares `relays_permissions:
    /// false` and gets a [`crate::providers::StaticPolicy`] instead.
    ///
    /// `discovers_instructions_in_cwd` is **false** and that is not an
    /// omission: the CLI's help names `CLAUDE.md` for discovery and never
    /// `AGENTS.md`, so discovery here is *unverified* and the file travels by
    /// flag. Decision 106 chose the flag for exactly that reason.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            relays_permissions: true,
            mcp_per_session: true,
            instructions_flag: true,
            discovers_instructions_in_cwd: false,
            resumes: true,
            naming: McpToolNaming::DoubleUnderscore,
        }
    }

    /// The three tiers, cheapest first, with their identifiers in full.
    ///
    /// **Full names, never aliases.** `docs/cli/claude-code-cli.md` §8 measured `--model haiku`
    /// resolving to `claude-haiku-4-5-20251001` — a generation behind what was
    /// asked for, and silently, which is why `agent::session` already carried
    /// the full name as a constant.
    ///
    /// Four models exist in this family; three are listed, because this list is
    /// not a catalogue of what exists but of what is **worth offering** for the
    /// two jobs ARIA has: hold a conversation, or do a piece of work. A fourth
    /// entry in a dropdown nobody can choose between is a worse dropdown.
    ///
    /// The tiers come straight from how the user put it on 2026-11-12: Haiku
    /// for a simple request such as the weather, Sonnet for the rest, Opus for
    /// code.
    fn models(&self) -> Vec<ModelChoice> {
        vec![
            ModelChoice {
                id: "claude-haiku-5-5".into(),
                display_name: "Haiku 5.5".into(),
                tier: ModelTier::Fast,
                note: "Le plus rapide. Ce que le poste vocal utilise, et ce qui suffit \
                       pour aller chercher un fait."
                    .into(),
            },
            ModelChoice {
                id: "claude-sonnet-5-5".into(),
                display_name: "Sonnet 5.5".into(),
                tier: ModelTier::Standard,
                note: "Le choix par défaut d'une tâche déléguée ordinaire.".into(),
            },
            ModelChoice {
                id: "claude-opus-5-5".into(),
                display_name: "Opus 5.5".into(),
                tier: ModelTier::Expert,
                note: "Pour du code et les analyses longues. Lent, et il le vaut.".into(),
            },
        ]
    }

    async fn availability(&self) -> Availability {
        match run_version(&self.command).await {
            Ok(found) => Availability::Ready { version: found },
            Err(e) => Availability::Missing {
                reason: e.to_string(),
            },
        }
    }

    /// What `claude mcp list` reports, parsed.
    ///
    /// The only source that costs no quota: it calls no model, it only
    /// health-checks each server — which is also why it takes seconds, measured
    /// on 2026-11-13. It lists the servers a session started without
    /// `--strict-mcp-config` loads, which is every delegated session since
    /// decision 105. Our own servers are not in it: they travel by
    /// `--mcp-config`.
    ///
    /// A failure is an empty list, logged. Not knowing the connectors costs a
    /// line of the voice prompt, never the voice post.
    async fn connectors(&self) -> Vec<Connector> {
        match run_mcp_list(&self.command).await {
            Ok(text) => parse_mcp_list(&text),
            Err(e) => {
                tracing::warn!("connecteurs illisibles : {e:#}");
                Vec::new()
            }
        }
    }

    async fn launch(
        &self,
        spec: LaunchSpec,
    ) -> anyhow::Result<(
        Box<dyn AgentSession>,
        tokio::sync::mpsc::Receiver<ProviderEvent>,
    )> {
        session::launch(&self.command, spec).await
    }
}

/// Ask the CLI what it is.
async fn run_version(command: &str) -> anyhow::Result<String> {
    let output = tokio::process::Command::new(command)
        .arg("--version")
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("`{command} --version` n'a pas pu être lancé : {e}"))?;

    if !output.status.success() {
        anyhow::bail!("`{command} --version` a échoué : {}", output.status);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_version(&text)
        .ok_or_else(|| anyhow::anyhow!("version illisible dans « {} »", text.trim()))
}

/// Run `claude mcp list` and hand back what it printed.
async fn run_mcp_list(command: &str) -> anyhow::Result<String> {
    let output = tokio::process::Command::new(command)
        .args(["mcp", "list"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("`{command} mcp list` n'a pas pu être lancé : {e}"))?;
    if !output.status.success() {
        anyhow::bail!("`{command} mcp list` a échoué : {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Read the servers out of `claude mcp list`.
///
/// There is no JSON form (`claude mcp list --help`, 2.1.323), so this reads
/// the text it prints, one server per line:
///
/// ```text
/// claude.ai Gmail: https://gmailmcp.googleapis.com/mcp/v1 - ✓ Connected
/// ```
///
/// The name is everything before the **first** `": "` — a URL holds a colon
/// but never followed by a space — and the state is what follows the **last**
/// `" - "`. Only a state that says `Connected` and nothing else counts: the
/// help names `⏸ Pending approval`, and anything we have not seen must read as
/// unusable rather than as a promise. The banner line has no `" - "` and
/// falls out by itself.
pub fn parse_mcp_list(text: &str) -> Vec<Connector> {
    text.lines()
        .filter_map(|line| {
            let (name, rest) = line.trim().split_once(": ")?;
            let (_, state) = rest.rsplit_once(" - ")?;
            let state = state.trim_start_matches(|c: char| !c.is_alphanumeric());
            Some(Connector {
                name: name.trim().to_string(),
                connected: state.trim().eq_ignore_ascii_case("connected"),
            })
        })
        .filter(|connector| !connector.name.is_empty())
        .collect()
}

/// Pull the version out of `2.1.165 (Claude Code)`.
pub fn parse_version(text: &str) -> Option<String> {
    let token = text.split_whitespace().next()?;
    // Must look like a version, so a change of output format produces "illisible"
    // rather than a nonsense string shown as a version.
    if token.split('.').count() < 2 || !token.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    Some(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_pulled_out_of_what_the_cli_actually_prints() {
        // Measured on the installed CLI, 2026-10-25.
        assert_eq!(
            parse_version("2.1.165 (Claude Code)\n").as_deref(),
            Some("2.1.165")
        );
    }

    #[test]
    fn something_that_is_not_a_version_is_reported_as_unreadable() {
        assert_eq!(parse_version("command not found"), None);
        assert_eq!(parse_version(""), None);
    }

    /// What the CLI printed on 2026-11-13, 2.1.323, banner included.
    #[test]
    fn the_connectors_are_read_off_what_the_cli_actually_prints() {
        let printed = "Checking MCP server health…\n\n\
             claude.ai Claude Docs: https://api.anthropic.com/v1/pages/mcp - ✓ Connected\n\
             claude.ai Gmail: https://gmailmcp.googleapis.com/mcp/v1 - √ Connected\n\
             projet: npx -y some-server --port 3000 - ⏸ Pending approval\n\
             claude.ai Google Calendar: https://calendarmcp.googleapis.com/mcp/v1 - ✗ Failed to connect\n";
        let found = parse_mcp_list(printed);
        let names: Vec<&str> = found.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "claude.ai Claude Docs",
                "claude.ai Gmail",
                "projet",
                "claude.ai Google Calendar"
            ]
        );
        // The console renders ✓ as √ on Windows; both read as connected.
        assert!(found[0].connected && found[1].connected);
        // Anything else is not a promise a session can keep.
        assert!(!found[2].connected, "pending approval");
        assert!(!found[3].connected, "failed");
    }

    #[test]
    fn a_line_that_is_not_a_server_is_skipped() {
        assert!(parse_mcp_list("No MCP servers configured.\n").is_empty());
        assert!(parse_mcp_list("").is_empty());
    }

    #[test]
    fn no_profile_reaches_bypass_permissions() {
        // Not a style check: decision 3 excludes it definitively, and a voice
        // transcription error would otherwise become an unguarded action.
        for profile in Profile::ALL {
            assert_ne!(permission_mode(profile), "bypassPermissions");
            assert_ne!(permission_mode(profile), "dontAsk");
        }
    }

    #[test]
    fn every_profile_has_a_translation_and_it_is_in_french() {
        let mapping = mapping();
        assert_eq!(mapping.len(), Profile::ALL.len());
        for row in &mapping {
            let native = row.native.as_ref().expect("Claude Code honours all four");
            assert_eq!(native.name, permission_mode(row.profile));
            assert!(
                !native.explanation.is_empty(),
                "the settings screen shows this: a profile whose translation \
                 you cannot see is a trap"
            );
        }
    }
}
