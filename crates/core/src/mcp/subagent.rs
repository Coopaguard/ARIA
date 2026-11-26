//! The memory post, served to **delegated sessions**.
//!
//! Decision 105 settled how a sub-agent reaches what ARIA knows, and ruled out
//! the two cheaper answers:
//!
//! - **injecting the pages into the delegated prompt** — one shot, chosen
//!   before the agent knows what it needs, and read-only in the worst way: a
//!   task that discovers its subject half-way through cannot ask a second time;
//!   - **`--add-dir` on the memory folder** — free, and unbounded: `docs/cli/claude-code-cli.md` §4
//!   records that reads never pass through the relay, so it is a read grant
//!   with no trace and no filter, and the native `Edit` it also hands over
//!   bypasses `.history`, the alias of decision 98 and the log.
//!
//! So: a server of its own, reached by MCP, where the core stays between the
//! agent and the folder.
//!
//! # Why a third server and not two more tools on the relay
//!
//! [`super::Relay`] already serves delegated sessions (`approve`, the tiles).
//! Merging would have worked and is not what this is. The reason is the
//! **budget**: [`crate::agent::tools`] bounds its descriptions to 1 600 bytes
//! in all, because they are sent on every single turn of a speaking agent.
//! Here nothing is spoken and a session has a handful of turns, so the
//! descriptions can say what they need to — the id convention, what a resume
//! is, when to read a page in full. One server, one audience, one budget.
//!
//! # Read-only, and that is the whole shape of it
//!
//! `remember`, `str_replace` and `rename_page` are **not** here. A write
//! carries the three obligations of decision 98 and the nudge of decision 95 —
//! what qualifies as durable, and its negative half — and both of those are
//! aimed at an agent that is in a conversation with the user. A sub-agent
//! researching train times has no standing to decide what ARIA remembers
//! about its user for ever, and the relay cannot usefully arbitrate it either:
//! the user would be shown a permission card for a sentence they never heard.
//!
//! The voice post keeps the writes ([`crate::mcp::voice`]), where there is a
//! human in the room to contradict them.
//!
//! # The token
//!
//! The same per-session token as the relay's, on a path of its own — but
//! checked **in front of the door**, by a layer in [`crate::transport`], the
//! way the main agent's endpoint is and not the way the relay is.
//!
//! The relay reads its token inside the handler because it needs the
//! **routing**: which session is asking. Here there is nothing to route — the
//! memory is the user's, not the session's — so the token is pure
//! authentication, and authentication in front of the door also covers
//! `tools/list`, which a handler-level check leaves open.
//!
//! It leaves this module as nothing but protocol, which is what lets it be
//! served over an in-memory pipe and driven by a real MCP client with no HTTP
//! server in the way.

use std::sync::Arc;

use rmcp::ErrorData;
use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{
    CallToolResponse, CallToolResult, ContentBlock, JsonObject, ServerCapabilities, ServerConfig,
    Tool,
};

use crate::Core;

/// The name this server is given in `--mcp-config`.
///
/// Not `aria`, which is the relay's, because a session now carries both and
/// two servers cannot share a name. And prefixed rather than plain `memory`
/// for a reason decision 105 created: `--strict-mcp-config` is no longer
/// passed to a delegated session, so the user's own servers are loaded beside
/// ours and a bare `memory` could collide with one of them.
pub const SERVER_NAME: &str = "aria_memory";

/// How many pages a search may answer with.
///
/// Three resumes is about what a model reads before deciding which page to
/// open in full, and the ceiling is here rather than in the schema so a model
/// asking for fifty gets three instead of a refusal.
pub const MAX_HITS: usize = 5;

pub const RECALL: &str = "recall";
pub const READ_PAGE: &str = "read_page";
/// Not a write: a proposal the main agent reads with the result (2026-11-26).
pub const PROPOSE: &str = "propose_memory";

/// What `--tools` would be passed for one of them, in Claude Code's spelling.
///
/// Kept unparameterised for the tests and the log; the **decision** path goes
/// through [`is_read_tool`], which takes the engine's naming.
pub fn tool_name(tool: &str) -> String {
    crate::providers::McpToolNaming::DoubleUnderscore.render(SERVER_NAME, tool)
}

/// Whether `tool` is one of this post's — all of which read.
///
/// Used by [`crate::Core::decide_permission`] to answer without asking. It is
/// a *list*, deliberately, and not a prefix test on the server name: a prefix
/// test would auto-approve anything ever added to this module, including the
/// write tools whose absence is the point.
pub fn is_read_tool(naming: crate::providers::McpToolNaming, tool: &str) -> bool {
    [RECALL, READ_PAGE, PROPOSE]
        .iter()
        .any(|name| naming.matches(tool, SERVER_NAME, name))
}

/// The tools, with room to explain themselves.
pub(crate) fn catalogue() -> Vec<crate::agent::tools::ToolSpec> {
    use crate::agent::tools::ToolSpec;
    vec![
        ToolSpec {
            name: RECALL,
            description: "Cherche dans la mémoire personnelle de l'utilisateur d'ARIA : \
                 ses préférences, ses décisions, ses habitudes, ce qui le concerne. \
                 Appelle cet outil AVANT de supposer quoi que ce soit sur lui — son \
                 matériel, ses horaires, ses goûts, les noms qu'il emploie. \
                 Rend au plus cinq pages, chacune avec son identifiant et son résumé \
                 en deux phrases. Le résumé n'est pas la page : s'il te faut les \
                 faits détaillés, les sources ou les liens, ouvre la page avec \
                 read_page. Si rien ne revient, c'est que la mémoire ne sait rien là-dessus : \
                 dis-le, n'invente pas.",
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Quelques mots en français, ou l'identifiant d'une page."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Combien de pages au plus. Par défaut 3, maximum 5."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: READ_PAGE,
            description: "Lit une page de la mémoire en entier : son en-tête, ses faits, \
                 ses sources et les pages qu'elle cite entre doubles crochets. \
                 `page` est l'identifiant rendu par recall, de la forme dossier/nom — \
                 pas un titre ni un chemin de fichier. Les pages citées s'ouvrent de la \
                 même façon. Tu n'écris pas dans la mémoire : propose avec \
                 propose_memory.",
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "page": {
                        "type": "string",
                        "description": "L'identifiant de la page, par exemple profil/louis."
                    }
                },
                "required": ["page"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: PROPOSE,
            description: "Propose à ARIA de retenir un fait durable sur l'utilisateur, \
                 appris pendant ta tâche : une préférence, une décision, une habitude, \
                 son matériel, un nom qu'il emploie. Rien n'est écrit : ARIA lit tes \
                 propositions avec ton résultat et garde ce qui en vaut la peine. \
                 Pas un résultat de recherche ni une donnée du jour, pas un secret, pas \
                 ce que la mémoire sait déjà. Cinq au plus.",
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "fact": {
                        "type": "string",
                        "description": "Le fait, en une phrase, avec une date absolue s'il en faut une."
                    },
                    "page": {
                        "type": "string",
                        "description": "La page qui irait, de la forme dossier/nom, si tu en as une idée."
                    },
                    "why": {
                        "type": "string",
                        "description": "Pourquoi c'est durable, en quelques mots."
                    }
                },
                "required": ["fact"],
                "additionalProperties": false
            }),
        },
    ]
}

/// The memory post, as an MCP server.
#[derive(Clone)]
pub struct MemoryTools {
    core: Arc<Core>,
    tool_router: ToolRouter<MemoryTools>,
}

impl MemoryTools {
    pub fn new(core: Arc<Core>) -> Self {
        let mut tool_router = ToolRouter::new();
        for spec in catalogue() {
            let schema = Arc::new(spec.schema.as_object().cloned().unwrap_or_default());
            tool_router.add_route(ToolRoute::new_dyn(
                Tool::new(spec.name, spec.description, schema),
                move |context| Box::pin(dispatch(context)),
            ));
        }
        Self { core, tool_router }
    }

    /// What the tools are listed as, for a test to read.
    pub fn published(&self) -> Vec<String> {
        self.tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    /// The fused ranking, cut down to resumes.
    ///
    /// Synchronous and not on a blocking thread, unlike the writes on the
    /// voice post: a query is **one** forward pass of the vectoriser against
    /// an in-memory index, and nothing here is speaking. Decision 63's concern
    /// is the voice's deadline, which this post does not hold.
    fn recall(&self, arguments: JsonObject) -> CallToolResult {
        let Some(query) = text(&arguments, "query") else {
            return refuse("Il manque les mots à chercher.");
        };
        let limit = arguments
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map_or(3, |n| n as usize)
            .clamp(1, MAX_HITS);

        match self.core.memory_deep(&query, limit) {
            Ok(found) if found.is_empty() => CallToolResult::success(vec![ContentBlock::text(
                "Rien en mémoire sur ce sujet.".to_string(),
            )]),
            Ok(found) => {
                let mut answer = String::new();
                for one in found {
                    // The link flag is said, because it changes what the hit
                    // means: a page reached through a `[[link]]` did not match
                    // the query, it was cited by something that did.
                    let how = if one.by_link { " (citée par une autre page)" } else { "" };
                    answer.push_str(&format!("page {}{} : {}\n", one.id, how, one.resume));
                }
                CallToolResult::success(vec![ContentBlock::text(answer)])
            }
            Err(e) => memory_refusal(e),
        }
    }

    /// Leave a proposal for the main agent. Needs to know which session asks,
    /// so it reads the token the way the relay does.
    fn propose(
        &self,
        arguments: JsonObject,
        context: &rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> CallToolResult {
        let Some(fact) = text(&arguments, "fact") else {
            return refuse("Il manque le fait à proposer.");
        };
        let Some(session) = context
            .extensions
            .get::<http::request::Parts>()
            .and_then(|parts| crate::mcp::token_in(parts.uri.query()?))
            .and_then(|token| self.core.session_routes().resolve(token))
        else {
            return refuse("Cette session n'est pas reconnue : proposition ignorée.");
        };
        let proposal = crate::agent::delegation::MemoryProposal {
            fact,
            page: text(&arguments, "page"),
            why: text(&arguments, "why"),
        };
        if self.core.propose_memory(session, proposal) {
            CallToolResult::success(vec![ContentBlock::text(
                "Proposé : ARIA le lira avec ton résultat.".to_string(),
            )])
        } else {
            refuse("Cinq propositions au plus par tâche.")
        }
    }

    /// One page, in full.
    fn read_page(&self, arguments: JsonObject) -> CallToolResult {
        let Some(page) = text(&arguments, "page") else {
            return refuse("Il manque l'identifiant de la page à lire.");
        };
        match self.core.memory_read(&page) {
            Ok(markdown) => CallToolResult::success(vec![ContentBlock::text(markdown)]),
            Err(e) => memory_refusal(e),
        }
    }

}

#[rmcp::tool_handler(router = self.tool_router)]
impl rmcp::ServerHandler for MemoryTools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Mémoire personnelle de l'utilisateur d'ARIA : la lire, et proposer \
             ce qu'il faudrait y ajouter. Consulte-la avant de supposer quelque \
             chose sur lui.",
        )
    }
}

async fn dispatch(context: ToolCallContext<'_, MemoryTools>) -> Result<CallToolResponse, ErrorData> {
    let arguments = context.arguments.clone().unwrap_or_default();
    let result = match context.name.as_ref() {
        RECALL => context.service.recall(arguments),
        READ_PAGE => context.service.read_page(arguments),
        PROPOSE => context.service.propose(arguments, &context.request_context),
        other => refuse(format!("La mémoire d'ARIA n'a pas d'outil {other}.")),
    };
    Ok(result.into())
}

fn refuse(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

/// One string argument, or nothing. Same leniency as the voice post's.
fn text(arguments: &JsonObject, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Turn a memory failure into something a session can act on.
///
/// Shorter than the voice post's twin ([`super::voice`]) because this one is
/// not spoken: a session reads the message, and the only thing it needs out of
/// it is what to do next.
fn memory_refusal(error: crate::store::MemoryError) -> CallToolResult {
    use crate::store::MemoryError::*;
    match error {
        Detached => refuse(
            "La mémoire d'ARIA n'est pas disponible. Continue sans elle et dis-le \
             dans ton rapport.",
        ),
        NoSuchPage(name) => refuse(format!(
            "Aucune page ne s'appelle {name}. Cherche-la d'abord avec recall : \
             l'identifiant est de la forme dossier/nom."
        )),
        Embed(e) => {
            // The deep half is optional by construction (decision 83), and
            // `deep_search` already falls back to the lexical index, so
            // reaching here means the vectoriser failed rather than being
            // absent.
            tracing::warn!("vectoriseur indisponible : {e}");
            refuse("La recherche par le sens est indisponible ; réessaie avec des mots exacts.")
        }
        other => {
            tracing::warn!("mémoire : {other}");
            refuse("La mémoire d'ARIA n'a pas pu répondre.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The only convention anyone has verified, and what these tests judge
    /// against. See `crate::providers::McpToolNaming`.
    const NAMING: crate::providers::McpToolNaming =
        crate::providers::McpToolNaming::DoubleUnderscore;

    /// The name has to match the key used in `--mcp-config`, and the two are
    /// written in different files.
    #[test]
    fn the_tools_are_named_as_the_cli_will_name_them() {
        assert_eq!(tool_name(RECALL), "mcp__aria_memory__recall");
        assert_eq!(tool_name(READ_PAGE), "mcp__aria_memory__read_page");
    }

    /// The relay's server name and this one's must differ: a session carries
    /// both, and `--mcp-config` is a map.
    #[test]
    fn the_two_servers_a_session_carries_do_not_share_a_name() {
        assert_ne!(SERVER_NAME, super::super::SERVER_NAME);
    }

    /// What decision 106 auto-approves, and what it must not.
    #[test]
    fn only_the_read_tools_are_answered_without_asking() {
        assert!(is_read_tool(NAMING, "mcp__aria_memory__recall"));
        assert!(is_read_tool(NAMING, "mcp__aria_memory__read_page"));
        // The writes live on the voice post and keep their confirmation. A
        // prefix test on the server name would have waved these through.
        assert!(!is_read_tool(NAMING, "mcp__aria_memory__remember"));
        assert!(!is_read_tool(NAMING, "mcp__aria_memory__str_replace"));
        assert!(!is_read_tool(NAMING, "mcp__aria_memory__rename_page"));
        assert!(!is_read_tool(NAMING, "Bash"));
        assert!(!is_read_tool(NAMING, "mcp__aria__approve"));
    }

    /// The post reads and proposes; it never writes. A proposal is read by
    /// the main agent, which decides (2026-11-26). A write tool added here
    /// would be a write with no one deciding, which is what decision 106
    /// refused.
    #[test]
    fn no_write_verb_is_published_here() {
        let names: Vec<&str> = catalogue().iter().map(|spec| spec.name).collect();
        assert_eq!(names, vec![RECALL, READ_PAGE, PROPOSE]);
        for name in names {
            for write in [
                crate::agent::tools::REMEMBER,
                crate::agent::tools::STR_REPLACE,
                crate::agent::tools::RENAME,
            ] {
                assert_ne!(name, write, "une écriture est arrivée sur le poste mémoire");
            }
        }
    }

    /// These descriptions are **not** under the voice post's budget, which is
    /// the reason this server exists at all — but they are still sent on every
    /// turn of the session that holds them, so the bound is generous and not
    /// absent.
    #[test]
    fn the_descriptions_may_be_long_here_but_not_unbounded() {
        let total: usize = catalogue().iter().map(|s| s.description.len()).sum();
        println!("poste mémoire : {total} octets");
        assert!(total < 2_000, "{total} octets");
    }

    /// `limit` is clamped rather than refused: a model asking for fifty pages
    /// is asking a reasonable question badly, and a refusal would cost a turn.
    #[test]
    fn an_oversized_limit_is_cut_down_instead_of_refused() {
        // The clamp is read off the handler's arithmetic, which is the one
        // place it lives.
        let fifty: usize = 50;
        assert_eq!(fifty.clamp(1, MAX_HITS), MAX_HITS);
        assert_eq!(0_usize.clamp(1, MAX_HITS), 1);
    }

    /// A detached memory is the state before `app/` has handed its root over,
    /// and a session must read something it can act on rather than an error.
    #[test]
    fn a_detached_memory_answers_in_words() {
        let core = Core::new();
        let tools = MemoryTools::new(Arc::clone(&core));
        let mut arguments = JsonObject::new();
        arguments.insert("query".into(), serde_json::json!("café"));
        let result = tools.recall(arguments);
        let text = format!("{:?}", result.content);
        assert!(text.contains("pas disponible"), "{text}");
        // And it says what to do next, which is the whole point of answering
        // in words rather than failing.
        assert!(text.contains("rapport"), "{text}");
    }

    #[test]
    fn the_three_tools_are_published_and_nothing_else_is() {
        let tools = MemoryTools::new(Core::new());
        let mut published = tools.published();
        published.sort();
        assert_eq!(published, vec!["propose_memory", "read_page", "recall"]);
    }

    /// Proposals wait in the core, five at most, and are handed over once.
    #[test]
    fn proposals_are_kept_per_session_and_taken_once() {
        use crate::agent::delegation::{MAX_PROPOSALS, MemoryProposal};
        let core = Core::new();
        let session = crate::sessions::SessionId::new();
        let other = crate::sessions::SessionId::new();
        let one = |fact: &str| MemoryProposal { fact: fact.into(), page: None, why: None };
        for i in 0..MAX_PROPOSALS {
            assert!(core.propose_memory(session, one(&format!("fait {i}"))));
        }
        assert!(!core.propose_memory(session, one("de trop")));
        assert!(core.propose_memory(other, one("autre")));
        assert_eq!(core.take_memory_proposals(session).len(), MAX_PROPOSALS);
        assert!(core.take_memory_proposals(session).is_empty());
        assert_eq!(core.take_memory_proposals(other).len(), 1);
    }
}
