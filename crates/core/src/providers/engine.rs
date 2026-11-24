//! What an engine *is able to do*, as opposed to what it calls things.
//!
//! The provider trait next door was written for a second engine and holds up:
//! it exposes no list of modes, because a mode is a provider's vocabulary and a
//! profile is ours. What it did **not** express is the harder half, found by
//! looking at four real CLIs on 2026-11-12 rather than by imagining them:
//!
//! > a profile a provider cannot *translate* is one thing. An engine that
//! > cannot **ask the user anything at all** is another, and the trait had no
//! > word for it.
//!
//! # What was measured, and the date
//!
//! Relevé le 2026-11-12, de la documentation de chaque éditeur. To be
//! re-checked rather than trusted after the fact — these move monthly, and
//! `docs/cli/claude-code-cli.md` exists precisely because CLI facts rot:
//!
//! | | non-interactive | JSON stream | MCP per session | **relay** |
//! |---|---|---|---|---|
//! | Claude Code | stdin `stream-json` | `stream-json` | `--mcp-config` | **`--permission-prompt-tool`** |
//! | Gemini CLI | `-p` | `--output-format stream-json` | `--allowed-mcp-server-names` *restricts* | none — `--approval-mode` only |
//! | Copilot CLI | `-p` | `--output-format json` (JSONL) | `--additional-mcp-config` | none — `--allow-all-tools` / `--deny-tool` |
//! | Codex | `codex exec` | `--json` | `config.toml` + `-c` | none — `exec` is `approval_policy = never` |
//!
//! **Claude Code is the only one of the four that can ask.** The others offer
//! static policy: a list fixed at launch, a coarse mode, or nothing. ARIA's
//! whole permission architecture rests on the relay — a request that does not
//! expire, lives under its session, and waits for the user. On three of those
//! four engines there is nobody to ask.
//!
//! # The answer, chosen by the user on 2026-11-12
//!
//! Declared capability, and a product that degrades **visibly**. A provider
//! says `relays_permissions: false`; the core then compiles its pattern
//! registry into a [`StaticPolicy`] at launch, and the settings screen says so
//! in words rather than offering a setting with no effect. Rich with Claude,
//! usable elsewhere, never silent.
//!
//! The alternative — refuse the profiles that need asking, which the trait
//! could already express with `native: None` — was rejected for a concrete
//! reason: Normal is the *default* profile, so those engines would have offered
//! Plan and Autonome and nothing in between.

use serde::{Deserialize, Serialize};

use crate::permissions::{Profile, registry::PatternRegistry};

/// What kind of thing answers.
///
/// The three the settings screen groups by, and the reason the enum exists
/// before its second variant does: the user asked for one selector per post —
/// "l'agent principal sur quelle CLI, quel moteur local, quelle API" — so the
/// *kinds* are part of the interface even while only one is populated. Tier B
/// of the voice post arrives as [`EngineKind::Local`], and it was always going
/// to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineKind {
    /// A command-line agent, driven as a subprocess. Everything today.
    Cli,
    /// A model running on this machine, behind a local sidecar.
    Local,
    /// A remote API, billed per token.
    Api,
}

impl EngineKind {
    /// What the settings screen calls it, in French.
    pub fn label(&self) -> &'static str {
        match self {
            EngineKind::Cli => "CLI",
            EngineKind::Local => "moteur local",
            EngineKind::Api => "API",
        }
    }
}

/// Which post an engine can hold.
///
/// Two, and they are not interchangeable. The voice post is a dispatcher whose
/// every word is spoken, under a byte budget, never billed per token; a
/// delegated session is the one doing the work. An engine good at one may be
/// wrong for the other, and the settings screen asks twice for that reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Post {
    /// The agent that holds the conversation.
    Voice,
    /// The sessions it delegates to.
    Delegation,
}

/// How an engine spells the name of a tool served over MCP.
///
/// A leak sealed on 2026-11-12. `mcp__aria__delegate_task` is **Claude Code's**
/// convention, and the core had it written into three places that build a name
/// and three that match one — so a second engine would have broken the relay
/// routing and the auto-approval in silence, which is the worst way for a
/// permission mechanism to break.
///
/// # Why only one variant
///
/// Because only one is verified. The project's standing rule is that CLI facts
/// are referenced or re-measured and re-dated, never restated from memory, and
/// a variant invented for an engine nobody has run would be exactly that. The
/// seam is what matters here: the core no longer hard-codes the convention.
///
/// **Adding a variant means running that CLI and writing down what it printed**,
/// not reading a blog about it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpToolNaming {
    /// `mcp__server__tool` — Claude Code, verified on 2.1.312.
    #[default]
    DoubleUnderscore,
}

impl McpToolNaming {
    /// The name an engine will use for one of our tools.
    pub fn render(&self, server: &str, tool: &str) -> String {
        match self {
            McpToolNaming::DoubleUnderscore => format!("mcp__{server}__{tool}"),
        }
    }

    /// Whether a name an engine reported is that tool of ours.
    ///
    /// The bare name is accepted too, and deliberately: a tool call arriving
    /// unqualified is the same tool, and refusing it would turn a cosmetic
    /// difference into a refused permission the user cannot explain.
    pub fn matches(&self, candidate: &str, server: &str, tool: &str) -> bool {
        candidate == self.render(server, tool) || candidate == tool
    }
}

/// One MCP server for a session to connect to.
///
/// **Structured, not rendered.** Until 2026-11-12 the core built Claude Code's
/// `{"mcpServers": …}` JSON itself, in `crate::transport` — which is the core
/// knowing one engine's configuration file format. Now it says *what* to serve
/// and each provider renders *its* way: inline JSON for Claude Code, a
/// `--additional-mcp-config` payload for Copilot, `-c mcp_servers.…` for Codex,
/// a settings file for Gemini.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServer {
    /// The name the engine will prefix our tools with.
    pub name: String,
    /// Where it is served, token included.
    pub url: String,
}

/// One tool on one of our MCP servers, before it has a name.
///
/// Carried instead of a string so the provider renders it in its own
/// convention — see [`McpToolNaming`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpTool {
    pub server: String,
    pub tool: String,
}

impl McpTool {
    pub fn new(server: impl Into<String>, tool: impl Into<String>) -> Self {
        Self {
            server: server.into(),
            tool: tool.into(),
        }
    }

    pub fn render(&self, naming: McpToolNaming) -> String {
        naming.render(&self.server, &self.tool)
    }
}

/// What an engine can be asked to do, declared by its provider.
///
/// Read by the core to decide *how* to launch, and by the settings screen to
/// say what the user is giving up. Every field is here because an engine
/// surveyed on 2026-11-12 lacked it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Can ask us, per decision, while it runs.
    ///
    /// The one that changes the product rather than the plumbing. `false` means
    /// every decision is taken **at launch** from a [`StaticPolicy`], the
    /// permission cards stop appearing for that session, and the screen has to
    /// say so.
    pub relays_permissions: bool,
    /// Accepts MCP servers for one session without touching global config.
    ///
    /// `false` means ARIA's own tools — the relay, the tiles, the memory —
    /// would have to be written into the user's own configuration file, which
    /// is not something this product does behind their back. An engine without
    /// it gets no tools of ours, and a delegated session there cannot show a
    /// tile or read the memory.
    pub mcp_per_session: bool,
    /// Takes a path to text appended to its own system prompt.
    pub instructions_flag: bool,
    /// Reads its instruction file out of the working directory by itself.
    ///
    /// A happy consequence of decision 108: a session now runs in
    /// `~/.aria/subagents`, which is where its `AGENTS.md` already is. An
    /// engine that discovers one there gets the standing instructions **for
    /// free**, with no flag at all.
    pub discovers_instructions_in_cwd: bool,
    /// Can be handed a native session id and continue it.
    pub resumes: bool,
    /// How it spells one of our MCP tools.
    pub naming: McpToolNaming,
}

impl Default for Capabilities {
    /// Everything off but the naming.
    ///
    /// Off and not on, because a capability wrongly assumed present fails at
    /// the worst moment: a relay that is not there means a session that asked
    /// and nobody heard, which is the fault measured live on 2026-10-26 and
    /// came out of the loudspeaker.
    fn default() -> Self {
        Self {
            relays_permissions: false,
            mcp_per_session: false,
            instructions_flag: false,
            discovers_instructions_in_cwd: false,
            resumes: false,
            naming: McpToolNaming::DoubleUnderscore,
        }
    }
}

impl Capabilities {
    /// What the settings screen says is missing, in French, one line each.
    ///
    /// Empty when nothing is. Said rather than merely logged: decision 5's rule
    /// applied one level up — a capability the user cannot see missing is a
    /// trap, exactly as a profile silently absent was.
    pub fn shortcomings(&self) -> Vec<&'static str> {
        let mut said = Vec::new();
        if !self.relays_permissions {
            said.push(
                "ne sait pas demander une autorisation en cours de route : \
                 les décisions sont prises au lancement",
            );
        }
        if !self.mcp_per_session {
            said.push(
                "n'accepte pas les outils d'ARIA pour une seule session : \
                 ni tuiles, ni lecture de la mémoire",
            );
        }
        if !self.instructions_flag && !self.discovers_instructions_in_cwd {
            said.push("ne lit pas les consignes de ~/.aria/subagents/AGENTS.md");
        }
        if !self.resumes {
            said.push("ne sait pas reprendre une session interrompue");
        }
        said
    }
}

/// Decisions taken **before** a session starts, for an engine that cannot ask.
///
/// Compiled from the pattern registry by [`StaticPolicy::compile`]. It is a
/// worse mechanism than the relay and it is meant to be: what it cannot do is
/// judge something nobody anticipated, which is the one thing a human in front
/// of a card does well.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StaticPolicy {
    /// Command patterns the registry pre-approves.
    pub allow: Vec<String>,
    /// Command patterns the registry refuses outright.
    pub deny: Vec<String>,
    /// Whether anything *unanticipated* may run.
    ///
    /// False for every profile but Autonome. It is the whole difference between
    /// a static policy and a blank cheque, and it is why this is not
    /// `bypassPermissions` by another name — that stays excluded.
    pub allow_unknown: bool,
}

impl StaticPolicy {
    /// Flatten the registry into two lists and a default.
    ///
    /// The registry holds command-line patterns with a verdict and a risk, and
    /// that is exactly the shape a static allow/deny list wants — so nothing is
    /// invented here, it is the same knowledge spent earlier.
    ///
    /// `Profile::Autonomous` is the only one that lets an unrecognised command
    /// through, because that is what the profile *means*. On every other
    /// profile an unrecognised command is refused by the engine, which is safe
    /// rather than permissive: the alternative, with nobody to ask, is a
    /// session that runs something nobody approved.
    pub fn compile(registry: &PatternRegistry, profile: Profile) -> Self {
        let mut allow = Vec::new();
        let mut deny = Vec::new();
        for rule in registry.rules() {
            if rule.auto_approve() && profile != Profile::Plan {
                allow.push(rule.pattern());
            } else {
                deny.push(rule.pattern());
            }
        }
        allow.sort_unstable();
        deny.sort_unstable();
        Self {
            allow,
            deny,
            allow_unknown: profile == Profile::Autonomous,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_is_named_the_way_the_engine_will_name_it() {
        let naming = McpToolNaming::DoubleUnderscore;
        assert_eq!(
            naming.render("aria", "delegate_task"),
            "mcp__aria__delegate_task"
        );
        assert!(naming.matches("mcp__aria__delegate_task", "aria", "delegate_task"));
        // The bare name too: unqualified is the same tool, and refusing it
        // would turn a spelling into a refused permission.
        assert!(naming.matches("delegate_task", "aria", "delegate_task"));
        // And nothing else, which is the half that matters: a tool from
        // another server must not inherit our verdicts.
        assert!(!naming.matches("mcp__other__delegate_task", "aria", "delegate_task"));
        assert!(!naming.matches("Bash", "aria", "delegate_task"));
    }

    /// Off by default, and the comment on `Default` says why in one line: a
    /// relay wrongly assumed present is a session that asked with nobody
    /// listening.
    #[test]
    fn a_capability_is_absent_until_a_provider_claims_it() {
        let capabilities = Capabilities::default();
        assert!(!capabilities.relays_permissions);
        assert!(!capabilities.mcp_per_session);
        assert_eq!(capabilities.shortcomings().len(), 4);
    }

    #[test]
    fn an_engine_that_can_do_everything_has_nothing_to_confess() {
        let capabilities = Capabilities {
            relays_permissions: true,
            mcp_per_session: true,
            instructions_flag: true,
            discovers_instructions_in_cwd: false,
            resumes: true,
            naming: McpToolNaming::DoubleUnderscore,
        };
        assert!(capabilities.shortcomings().is_empty());
    }

    /// Reading the instructions out of the working directory counts, and that
    /// is decision 108 paying off rather than a loophole: the session runs in
    /// `~/.aria/subagents`, where the file already is.
    #[test]
    fn discovering_the_instructions_in_the_folder_counts_as_reading_them() {
        let capabilities = Capabilities {
            instructions_flag: false,
            discovers_instructions_in_cwd: true,
            ..Capabilities::default()
        };
        assert!(
            !capabilities
                .shortcomings()
                .iter()
                .any(|said| said.contains("consignes"))
        );
    }

    #[test]
    fn a_static_policy_is_the_registry_flattened() {
        let registry = PatternRegistry::with_defaults();
        let normal = StaticPolicy::compile(&registry, Profile::Normal);

        assert!(!normal.allow.is_empty(), "le registre a des motifs sûrs");
        assert!(!normal.allow_unknown, "seul Autonome laisse passer l'inconnu");

        // Plan approves nothing: it is the profile where the agent reads and
        // proposes, so a static policy for it allows nothing at all.
        let plan = StaticPolicy::compile(&registry, Profile::Plan);
        assert!(plan.allow.is_empty(), "{:?}", plan.allow);
        assert!(!plan.allow_unknown);

        // And Autonome is the only blank in the whole table — which is what
        // the profile means, and is still not `bypassPermissions`: the lists
        // are there, and what they refuse stays refused.
        let autonome = StaticPolicy::compile(&registry, Profile::Autonomous);
        assert!(autonome.allow_unknown);
        assert_eq!(autonome.deny, normal.deny);
    }
}

/// One entry in a settings dropdown.
///
/// Serialised to the window, which is why it carries the display name and the
/// kind rather than only an id: the screen groups by kind — "CLI", "moteur
/// local", "API" — and the user asked for exactly that vocabulary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineChoice {
    pub id: String,
    pub display_name: String,
    pub kind: EngineKind,
}

/// Every engine ARIA can be pointed at, for one post.
///
/// # Why the two lists differ, and will keep differing
///
/// The delegation post is behind [`super::AgentProvider`], so adding an engine
/// there is declaring a mapping. The **voice** post is not behind that trait at
/// all: `crate::agent::session` drives Claude Code directly, because what that
/// post needs is narrower and stranger — a replaceable system prompt, text
/// streamed sentence by sentence, a tool catalogue bounded in bytes, and never
/// a per-token bill. Its second engine was always going to be tier B, a local
/// model behind a sidecar, which is an [`EngineKind::Local`] and not another
/// CLI.
///
/// So the voice list has one entry today and the honest thing is to show it as
/// one entry, not to imply a choice that does not exist.
pub fn choices(post: Post) -> Vec<EngineChoice> {
    let claude = EngineChoice {
        id: crate::providers::claude_code::ID.to_string(),
        display_name: "Claude Code".to_string(),
        kind: EngineKind::Cli,
    };
    match post {
        // Claude Code first: the default again since 2026-11-19 (decision
        // 127), and an unknown id falls back to the first entry. The local
        // models that hear (epic 10) follow, chosen by hand.
        Post::Voice => std::iter::once(claude)
            .chain(
                crate::agent::local::models()
                    .into_iter()
                    .map(|model| EngineChoice {
                        id: model.id.to_string(),
                        display_name: model.display_name.to_string(),
                        kind: EngineKind::Local,
                    }),
            )
            .collect(),
        Post::Delegation => {
            let mut all = vec![claude];
            // Epic 03's exit condition, selectable rather than merely
            // compiled — and debug-only, because an engine that repeats your
            // words is a proof and not a feature. See `super::echo`.
            #[cfg(debug_assertions)]
            all.push(EngineChoice {
                id: crate::providers::echo::ID.to_string(),
                display_name: "Écho (démonstration)".to_string(),
                kind: EngineKind::Cli,
            });
            all
        }
    }
}

/// Whether an id is one this build can actually be pointed at.
///
/// Checked rather than trusted: the id comes out of a hand-editable TOML file,
/// and a typo there must fall back to the default engine with a line in the
/// log, not leave the product with no engine at all.
pub fn is_known(post: Post, id: &str) -> bool {
    choices(post).iter().any(|choice| choice.id == id)
}

#[cfg(test)]
mod catalogue_tests {
    use super::*;

    /// Claude Code first — the default again since 2026-11-19 (decision 127),
    /// and an unknown id falls back to the first entry — then the local models.
    #[test]
    fn the_voice_post_offers_the_cli_then_the_local_models() {
        let voice = choices(Post::Voice);
        assert_eq!(voice.len(), 3);
        assert_eq!(voice[0].id, crate::providers::claude_code::ID);
        assert_eq!(voice[0].kind, EngineKind::Cli);
        for choice in &voice[1..] {
            assert_eq!(choice.kind, EngineKind::Local);
            assert!(crate::agent::local::find(&choice.id).is_some());
        }
    }

    /// Epic 03 again: a second engine appears in the **interface** without the
    /// interface being touched. In a release build there is one.
    #[test]
    fn delegation_offers_the_second_engine_in_a_debug_build() {
        let delegation = choices(Post::Delegation);
        if cfg!(debug_assertions) {
            assert_eq!(delegation.len(), 2, "{delegation:?}");
            assert!(is_known(Post::Delegation, "echo"));
        } else {
            assert_eq!(delegation.len(), 1, "{delegation:?}");
            assert!(!is_known(Post::Delegation, "echo"));
        }
    }

    /// A typo in the TOML must not cost the engine.
    #[test]
    fn an_unknown_id_is_not_known() {
        assert!(!is_known(Post::Delegation, "gemini-cli"));
        assert!(!is_known(Post::Voice, ""));
        assert!(is_known(Post::Voice, crate::providers::claude_code::ID));
    }

    /// The kinds the dropdown groups by, in French, because they are read.
    #[test]
    fn every_kind_has_a_label() {
        assert_eq!(EngineKind::Cli.label(), "CLI");
        assert_eq!(EngineKind::Local.label(), "moteur local");
        assert_eq!(EngineKind::Api.label(), "API");
    }
}

// ------------------------------------------------------------------- models

/// What a model is **for**, in the only vocabulary a dispatcher can choose by.
///
/// Asked for by the user on 2026-11-12: with Haiku holding the conversation,
/// Haiku again for a simple lookup, Sonnet for ordinary work, Opus for code.
///
/// # Why a tier and never a model id
///
/// Three reasons, and the first decides it:
///
/// - **it survives the abstraction.** `claude-opus-5-5` means nothing to Gemini
///   or to Codex. A tier maps onto every engine surveyed — Haiku/Sonnet/Opus,
///   Flash/Pro, mini/full — so the voice post asks for *the strong one* and the
///   provider knows which that is;
/// - **it costs three short words.** The tool schema ships on every turn of
///   speech, and an enum of model identifiers would be both longer and wrong
///   next month;
/// - **models are renamed, deprecated and superseded constantly.** A dispatcher
///   that named one would eventually name something that no longer exists, and
///   the failure arrives as a refused launch in the middle of a sentence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    /// A fact to fetch, a lookup, something short. The weather.
    Fast,
    /// Ordinary work, and the default when nothing says otherwise.
    #[default]
    Standard,
    /// Code, or a long analysis. Slow, and worth it.
    Expert,
}

impl ModelTier {
    /// The word the **model** writes, which is also what the schema lists.
    ///
    /// French, because the whole catalogue is: this post reads French and
    /// speaks French, and a schema in two languages is one a 4B-class model
    /// gets wrong occasionally.
    pub fn word(&self) -> &'static str {
        match self {
            ModelTier::Fast => "rapide",
            ModelTier::Standard => "standard",
            ModelTier::Expert => "expert",
        }
    }

    /// Every tier, in increasing cost. The order the schema lists them in.
    pub const ALL: [ModelTier; 3] = [ModelTier::Fast, ModelTier::Standard, ModelTier::Expert];

    /// Back from the word, for a tool argument.
    ///
    /// Unknown falls back to [`ModelTier::Standard`] rather than failing: the
    /// value comes from a language model, and refusing a whole delegation over
    /// a misspelled tier would cost the user their answer to save a token.
    pub fn from_word(word: &str) -> Self {
        ModelTier::ALL
            .into_iter()
            .find(|tier| tier.word() == word.trim())
            .unwrap_or_default()
    }
}

/// One model an engine offers, as the provider declares it.
///
/// **Asked of the provider, never written in the interface** — the same rule as
/// the profile table, and for the same reason: a second engine is a list it
/// declares, not a table elsewhere to keep in step by hand.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelChoice {
    /// The engine's own identifier, **in full**.
    ///
    /// Never an alias. `docs/cli/claude-code-cli.md` §8 measured `--model haiku` resolving to
    /// `claude-haiku-4-5-20251001` — a generation behind what was asked for,
    /// silently, which is the worst way for a model choice to fail.
    pub id: String,
    /// What the dropdown shows.
    pub display_name: String,
    /// Which tier this model answers for.
    pub tier: ModelTier,
    /// One line for the settings screen, in French.
    pub note: String,
}

/// The model for a tier, or the nearest thing the engine has.
///
/// Falls back to standard and then to anything, deliberately: asked for
/// `Expert` on an engine with no expert model, a standard answer beats none,
/// and an engine with an empty list gets `None` — the session then runs on
/// whatever that engine defaults to, which is what every delegated session did
/// until 2026-11-12.
pub fn model_for(models: &[ModelChoice], tier: ModelTier) -> Option<&ModelChoice> {
    models
        .iter()
        .find(|model| model.tier == tier)
        .or_else(|| models.iter().find(|model| model.tier == ModelTier::Standard))
        .or_else(|| models.first())
}

/// The cheapest, fastest model an engine offers.
///
/// What the voice post is pointed at by default, and the user said so in one
/// line: "le plus rapide par défaut". It holds the conversation, answers every
/// sentence, and is never billed per token — so fastest is not a compromise
/// there, it is the right answer.
pub fn fastest(models: &[ModelChoice]) -> Option<&ModelChoice> {
    ModelTier::ALL
        .into_iter()
        .find_map(|tier| models.iter().find(|model| model.tier == tier))
}

#[cfg(test)]
mod model_tests {
    use super::*;

    fn three() -> Vec<ModelChoice> {
        ["fast-1", "std-1", "exp-1"]
            .into_iter()
            .zip(ModelTier::ALL)
            .map(|(id, tier)| ModelChoice {
                id: id.into(),
                display_name: id.into(),
                tier,
                note: String::new(),
            })
            .collect()
    }

    #[test]
    fn a_tier_round_trips_through_the_word_the_model_writes() {
        for tier in ModelTier::ALL {
            assert_eq!(ModelTier::from_word(tier.word()), tier);
        }
    }

    /// The value comes from a language model. Refusing a whole delegation over
    /// a misspelled tier would cost the user their answer to save a token.
    #[test]
    fn an_unknown_tier_is_standard_and_not_a_failure() {
        assert_eq!(ModelTier::from_word("costaud"), ModelTier::Standard);
        assert_eq!(ModelTier::from_word(""), ModelTier::Standard);
        assert_eq!(ModelTier::from_word(" expert "), ModelTier::Expert);
    }

    #[test]
    fn each_tier_gets_its_own_model_when_the_engine_has_one() {
        let models = three();
        for tier in ModelTier::ALL {
            assert_eq!(model_for(&models, tier).unwrap().tier, tier);
        }
    }

    /// An engine with fewer models still answers. A delegation refused because
    /// the engine has no "expert" would be the abstraction costing the user an
    /// answer.
    #[test]
    fn a_missing_tier_falls_back_rather_than_refusing() {
        let only_standard = vec![three()[1].clone()];
        assert_eq!(
            model_for(&only_standard, ModelTier::Expert).unwrap().id,
            "std-1"
        );
        assert_eq!(
            model_for(&only_standard, ModelTier::Fast).unwrap().id,
            "std-1"
        );
        // An engine that offers nothing names nothing: the session runs on that
        // engine's own default, which is what they all did before.
        assert!(model_for(&[], ModelTier::Fast).is_none());
    }

    /// The voice post's default.
    #[test]
    fn the_fastest_is_what_the_voice_post_gets_by_default() {
        assert_eq!(fastest(&three()).unwrap().id, "fast-1");
        // Even when the list is not in order.
        let mut shuffled = three();
        shuffled.reverse();
        assert_eq!(fastest(&shuffled).unwrap().id, "fast-1");
        assert!(fastest(&[]).is_none());
    }
}
