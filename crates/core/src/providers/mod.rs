//! The agent provider trait: what ARIA needs from an agent that *acts*.
//!
//! Launching and resuming a session, the profile-to-native-mode table the
//! provider declares for itself, relaying permission requests (carrying the
//! command, not just the tool name), and a normalised event stream.
//!
//! The trait exposes no list of modes. A mode is a provider's vocabulary; a
//! profile is ours, and it survives a change of engine. See
//! epic 03.
//!
//! # Why this is a trait with one implementation
//!
//! Laying it down afterwards costs far more than laying it down now, and epic
//! 03's exit condition is explicit about what "useful" means here: a second,
//! fake provider — even one that only echoes, even one that can honour two
//! profiles out of four — plugs in **without touching the interface code**, and
//! the interface says which profiles are missing. That is why
//! [`AgentProvider::profile_mapping`] returns a table with holes in it rather
//! than a list of modes: a profile the provider cannot honour has to be
//! *sayable*, not silently absent.

pub mod claude_code;
#[cfg(debug_assertions)]
pub mod echo;
pub mod engine;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use engine::{
    fastest, model_for, Capabilities, EngineChoice, EngineKind, McpServer, McpTool, McpToolNaming, ModelChoice,
    ModelTier, Post, StaticPolicy,
};

use crate::permissions::Profile;

/// What a provider calls the thing it does for one of our profiles.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeMode {
    /// The provider's own name for it — `acceptEdits`, and so on.
    pub name: String,
    /// What it actually does, in French, for the settings screen.
    ///
    /// Displayed, and decision 5 says why: a profile whose translation you
    /// cannot see is a trap. "Autonome" on Claude Code is a classifier that
    /// re-reads every action; elsewhere the same word could cover far less.
    pub explanation: String,
}

/// One row of the profile table.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProfileSupport {
    pub profile: Profile,
    /// `None` when the provider has no equivalent.
    ///
    /// The interface says so instead of offering a setting with no effect.
    pub native: Option<NativeMode>,
}

/// Whether the provider can actually be used right now.
///
/// Epic 01: checked **at every start-up**, not once and for all, because the
/// CLI updates itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Availability {
    Ready {
        version: String,
    },
    /// Not installed, or not runnable.
    Missing {
        reason: String,
    },
}

impl Availability {
    pub fn can_launch(&self) -> bool {
        matches!(self, Availability::Ready { .. })
    }

    /// What the settings screen shows, in French.
    pub fn message(&self) -> String {
        match self {
            Availability::Ready { version } => format!("CLI {version} — prête"),
            Availability::Missing { reason } => {
                format!("CLI introuvable ({reason})")
            }
        }
    }
}

/// A server of the user's own that a delegated session will load.
///
/// Decision 105 lets the user's connectors into delegated sessions; decision
/// 117 is the voice post being **told** they are there, so that "envoie ça par
/// mail à Louis" is delegated rather than declined.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connector {
    /// The engine's own name for it — `claude.ai Gmail` on Claude Code.
    pub name: String,
    /// Whether the engine could reach it when asked.
    ///
    /// A connector that needs authentication or failed its health check is
    /// listed by the engine and useless to a session; telling the voice post
    /// it can send mail through one would be a promise the session breaks.
    pub connected: bool,
}

/// Everything needed to open a session.
#[derive(Clone, Debug, Default)]
pub struct LaunchSpec {
    /// The first thing to ask. May be empty for a session opened idle.
    pub prompt: String,
    /// The profile this session runs under — the global default, or an override.
    pub profile: Profile,
    /// Working directory. The provider grants file access to it.
    pub cwd: Option<PathBuf>,
    /// A native session id to resume.
    ///
    /// Epic 05: never deduce resumability from a date. Try the resume and
    /// handle the failure — the user's own `cleanupPeriodDays` can be as low as
    /// one day, and we do not control it.
    pub resume: Option<String>,
    /// Hard ceilings, applied by the provider itself.
    ///
    /// Flags, not code: `--max-turns` and `--max-budget-usd` already exist, and
    /// they are also why decision 8 never kills a quiet session.
    pub max_turns: Option<u32>,
    pub max_budget_usd: Option<f64>,
    /// The model this session runs on, in the engine's own full spelling.
    ///
    /// `None` leaves the engine on its own default, which is where every
    /// delegated session was until 2026-11-12 — a weather lookup and a day of
    /// coding both got whatever `claude` happened to prefer. The voice agent
    /// now picks a [`ModelTier`] and `Conversation::delegate` resolves it
    /// against [`AgentProvider::models`], because that is the one place that
    /// holds both the tier and the provider.
    pub model: Option<String>,
    /// The MCP tool the provider should relay permission requests to.
    ///
    /// Epic 02. While this is `None` the session runs **unrelayed**, which is
    /// safe rather than permissive: with no terminal and no relay, anything
    /// that would have asked is refused by the CLI itself.
    ///
    /// An [`McpTool`] and no longer a string since 2026-11-12: the *name* is
    /// the engine's business — see [`McpToolNaming`] — and a core that spelled
    /// it would be a core that knows one engine's convention.
    pub relay: Option<McpTool>,
    /// The MCP servers to hand the session.
    ///
    /// **Structured, not rendered.** The core used to build Claude Code's
    /// `{"mcpServers": …}` JSON in `crate::transport`, which is one engine's
    /// configuration format living in engine-agnostic code. Each provider now
    /// renders these its own way, and a provider whose
    /// [`Capabilities::mcp_per_session`] is false ignores them — a session
    /// there gets no tools of ours, which the settings screen says out loud.
    pub mcp_servers: Vec<McpServer>,
    /// Decisions taken in advance, for an engine that cannot be asked.
    ///
    /// `Some` exactly when [`Capabilities::relays_permissions`] is false. It is
    /// the pattern registry flattened at launch — a worse mechanism than the
    /// relay, deliberately, and the one the user chose on 2026-11-12 over
    /// refusing those engines the profiles that need asking.
    pub policy: Option<StaticPolicy>,
    /// Standing instructions to append to the provider's own system prompt.
    ///
    /// A **path**, not the text: the CLI reads the file itself
    /// (`--append-system-prompt-file`, verified on 2.1.312 on 2026-11-12),
    /// which keeps an arbitrarily long file off a command line that Windows
    /// bounds at 32 767 characters. Decision 106.
    ///
    /// `None` while `app/` has not resolved it, which costs the file and not
    /// the session.
    pub instructions_file: Option<PathBuf>,
}

/// What a session reports, normalised away from any provider's wire format.
#[derive(Clone, Debug)]
pub enum ProviderEvent {
    /// The session is up. Carries the provider's own id, which is what a
    /// resume needs later.
    Started {
        native_id: String,
        tools: Vec<String>,
        mcp_servers: Vec<String>,
    },
    /// A fragment of the answer.
    TextDelta(String),
    /// The agent is reasoning, with nothing to show yet.
    Thinking,
    /// The agent reached for a tool.
    ///
    /// `command` is carried from the V0 and epic 02 says why it cannot wait: a
    /// pattern registry that only ever sees tool names can express no pattern.
    ToolUse {
        name: String,
        command: Option<String>,
    },
    ToolResult {
        name: String,
        ok: bool,
    },
    /// The turn finished.
    Finished {
        summary: String,
        usage: Usage,
    },
    /// The session failed.
    Failed {
        message: String,
    },
    /// Where the account's quota stands, volunteered by the provider.
    ///
    /// Decision 36: the stream **is** the instrument. The CLI has no `usage`
    /// subcommand, so there is nowhere else to read this, and it arrives
    /// unasked — observed mid-turn on 2026-10-26 once the seven-day window
    /// crossed its warning threshold.
    ///
    /// It matters most from **delegated** sessions: they and the voice agent
    /// spend the same pot, and what exhausts it is the number of turns rather
    /// than their size.
    Quota {
        /// Fraction of the window used, 0 to 1.
        utilization: f64,
        /// Which window — `seven_day` on Claude Code.
        window: String,
        /// The provider thinks this is worth showing, not merely logging.
        warning: bool,
    },
    /// Something the provider sent that we do not model.
    ///
    /// Epic 01 asks for this by name: it is logged, and it **does not interrupt
    /// the session**. The CLI adds event types, and a bridge that dies on an
    /// unknown one is a bridge that breaks on an update nobody asked for.
    Unknown {
        raw: String,
    },
}

/// What a turn cost. From the provider's own accounting.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    /// The provider's estimate, when it gives one.
    ///
    /// An estimate, and on Claude Code a **suspect** one: `docs/cli/claude-code-cli.md` §8 records
    /// that `total_cost_usd` does not reconcile with the token counters for
    /// `claude-haiku-5-5`. The token counts are the trustworthy half.
    pub total_cost_usd: Option<f64>,
}

/// A running session.
#[async_trait::async_trait]
pub trait AgentSession: Send {
    /// Say something to it. One process holds several turns.
    async fn send(&mut self, text: &str) -> anyhow::Result<()>;

    /// End it. Closing stdin is the clean way; killing is the fallback.
    async fn stop(&mut self) -> anyhow::Result<()>;

    /// The provider's own id for this session, once it is known.
    ///
    /// `None` until the session has started, because the provider assigns it.
    fn native_id(&self) -> Option<String>;
}

/// An engine ARIA can delegate work to.
#[async_trait::async_trait]
pub trait AgentProvider: Send + Sync {
    /// Stable identifier, for configuration.
    fn id(&self) -> &'static str;

    /// What the settings screen calls it.
    fn display_name(&self) -> &'static str;

    /// Which kind of thing it is — a CLI, a local engine, an API.
    ///
    /// What the settings screen groups the dropdown by. Everything today is
    /// [`EngineKind::Cli`]; the variant exists before its siblings because the
    /// user asked for one selector per post, and a selector has to say what it
    /// is offering.
    fn kind(&self) -> EngineKind {
        EngineKind::Cli
    }

    /// What it is able to do, as opposed to what it calls things.
    ///
    /// Defaulted to nothing but a naming convention, and that default is a
    /// safety property rather than laziness: a provider that forgets to
    /// declare a relay gets a static policy, which refuses what it does not
    /// recognise. The opposite default would hand a session a relay flag no
    /// engine reads, and the refusal would come out of the loudspeaker — which
    /// is the fault measured live on 2026-10-26.
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    /// The models worth offering, cheapest first.
    ///
    /// Asked of the provider for the same reason the profile table is: a second
    /// engine should be a list it declares. Used for two things, both named by
    /// the user on 2026-11-12 — the model the **voice post** runs on (the
    /// fastest by default), and the model a **delegated session** runs on, which
    /// the voice agent chooses by tier rather than by name.
    ///
    /// Empty by default, and that is a working state rather than a gap: a
    /// session then launches with no `--model` and runs on whatever that engine
    /// defaults to, which is what every delegated session did until now.
    fn models(&self) -> Vec<ModelChoice> {
        Vec::new()
    }

    /// The profile table, declared by the provider itself.
    ///
    /// **Asked, never hard-coded in the interface.** That is the whole point of
    /// the trait, and what makes wiring a second engine a matter of declaring
    /// one more mapping.
    fn profile_mapping(&self) -> Vec<ProfileSupport>;

    /// Whether it can be used, checked now rather than remembered.
    async fn availability(&self) -> Availability;

    /// The user's own servers a delegated session would load, as of now.
    ///
    /// Decision 117: what the voice post is told its delegations can reach.
    /// Empty by default, and that is a working state — the post then simply
    /// is not told, which is how it ran until 2026-11-13. A provider whose
    /// sessions get no connectors at all must leave it empty rather than list
    /// what the user configured elsewhere.
    ///
    /// May take seconds: an engine may health-check each server before
    /// answering. Callers bound it.
    async fn connectors(&self) -> Vec<Connector> {
        Vec::new()
    }

    /// Open a session.
    ///
    /// Returns the handle and the stream of what it reports. One process per
    /// session: no multiplexing.
    async fn launch(
        &self,
        spec: LaunchSpec,
    ) -> anyhow::Result<(
        Box<dyn AgentSession>,
        tokio::sync::mpsc::Receiver<ProviderEvent>,
    )>;
}

/// Find a profile's row in a mapping.
pub fn native_for(mapping: &[ProfileSupport], profile: Profile) -> Option<&NativeMode> {
    mapping
        .iter()
        .find(|row| row.profile == profile)
        .and_then(|row| row.native.as_ref())
}
