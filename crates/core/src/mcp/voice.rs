//! The catalogue the main agent is served, over MCP.
//!
//! [`crate::agent::tools`] says what the main agent may do and why each tool
//! has the shape it has. This is the wire that carries it, and without it the
//! post has words and nothing else: the model cannot call `delegate_task` of
//! its own accord, which is what made T3's scenario a test rather than a
//! product.
//!
//! # Not the relay, and not on the relay's endpoint
//!
//! [`super::Relay`] serves `approve` to the **delegated** sessions. It is the
//! permission prompt: a session about to run something calls it, and its
//! endpoint routes back to the session that called. This serves
//! `delegate_task` to the **main agent**, which is the other direction
//! entirely — work going out, not permission coming in.
//!
//! Two servers on two endpoints, therefore. Merging them would hand every
//! delegated session a `delegate_task` of its own, and a sub-agent able to
//! open sub-agents is the one thing this post's design rules out: one voice
//! holds the conversation.
//!
//! # The catalogue is read, never restated
//!
//! The routes are built from [`crate::agent::tools::catalogue`] rather than
//! declared with `#[tool]` attributes. Those descriptions are written *at* the
//! model and tested — "the result is NOT in this tool's answer" is the
//! sentence that makes the whole mechanism work — and a second copy in a macro
//! attribute is a second copy that drifts.
//!
//! # Where the token is checked, and why it is not here
//!
//! `delegate_task` starts work on the user's machine, so the endpoint carries
//! a token: loopback keeps the network out and nothing else, since any process
//! running as this user can reach `127.0.0.1`.
//!
//! The check sits in front of the whole endpoint
//! ([`crate::transport::serve_events_on`]) rather than inside the tool
//! handlers, which is the opposite of what [`super::Relay`] does. Two reasons,
//! and the first is the real one:
//!
//! 1. the relay reads its token because it needs the **routing** — which
//!    session is calling. Here there is one caller, so the token is pure
//!    authentication, and authentication in front of the door also covers
//!    `tools/list`, which a handler-level check leaves open;
//! 2. it leaves this module to be nothing but protocol, so it can be served
//!    over an in-memory pipe and tested against a real MCP client without an
//!    HTTP server in the way.

use std::sync::Arc;

use rmcp::ErrorData;
use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::{
    CallToolResponse, CallToolResult, ContentBlock, JsonObject, ServerCapabilities, ServerConfig,
    Tool,
};

use crate::Core;
use crate::agent::tools::{self, DelegateTask};

/// What `--tools` has to be passed for one of them: `mcp__<server>__<tool>`.
pub fn tool_name(tool: &str) -> String {
    format!("mcp__{}__{}", super::SERVER_NAME, tool)
}

/// What `--permission-prompt-tool` is passed for the main agent.
///
/// The same tool name as [`super::Relay`]'s, on a different server and a
/// different endpoint, because it answers the same question at a different
/// post — and the answers have nothing in common. See [`approve`].
pub fn permission_tool_name() -> String {
    tool_name(super::TOOL_NAME)
}

/// Whether the main agent may run the tool it just reached for.
///
/// `--tools` makes a tool exist; it does not make it runnable. Measured live on
/// 2026-10-26: the model called `delegate_task` of its own accord, the CLI
/// asked for permission, nobody was listening, and the refusal came out of the
/// loudspeaker. So the main agent's session gets a relay too — and the policy
/// on this post is a *rule*, not a list:
///
/// > the only tools that exist here are ARIA's own, and those are allowed by
/// > construction. Anything else arriving here is a tool the catalogue never
/// > published, which is a bug or an escape, and is refused.
///
/// That is narrower than `--allowed-tools`, which was tried first: a blanket
/// pre-approval would still be a blanket. And it is not a step towards
/// `bypassPermissions`, which stays excluded — what `delegate_task` *does* is
/// open a session, and that session goes through [`super::Relay`], its profile
/// and the pattern registry like any other. The user's question is still
/// asked. It is asked one floor down, where there is something concrete to
/// show them.
pub fn approve(tool: &str) -> bool {
    tools::catalogue()
        .iter()
        .any(|spec| !unbacked(spec.name) && tool_name(spec.name) == tool)
}

/// The tools the catalogue declares but nothing in the core stands behind yet.
///
/// `run_action` is the short path, and the short path is
/// [`crate::actions`] — nine lines of documentation and no executor. Publishing
/// it would cost its schema in the input of every single turn and buy a tool
/// whose only honest answer is that the action does not exist. Deferred past
/// the MVP by the user on 2026-11-12.
///
/// # Read this before removing the line below
///
/// This function is the **only** thing keeping `run_action` out of [`approve`],
/// and [`approve`] is a rule rather than a list: anything of ARIA\x27s own is
/// allowed by construction. So deleting this the day an executor exists would
/// silently pre-approve **every** action — "turn the volume up" and "empty the
/// recycle bin" judged identically, with nothing in the code to say so.
///
/// `approve` cannot answer on a tool *name* for this one. It has to read the
/// `action` argument, look its risk up in the registry, and pre-approve only
/// `Risk::Low`; an action absent from the registry is refused rather than
/// asked, which is already that module\x27s rule. The whole note is in
/// `docs/todos/actions.md`.
fn unbacked(name: &str) -> bool {
    name == tools::RUN_ACTION
}

/// Everything the main agent is actually served, in the CLI's spelling.
///
/// Goes straight into [`crate::agent::session::Config::allowed_tools`]. It is
/// derived from the catalogue rather than written out, so a tool that is
/// published and a tool that is permitted cannot disagree — and they would
/// disagree silently, as a model that sees a tool and is refused when it calls
/// it.
pub fn allowed_tools() -> Vec<String> {
    published().iter().map(|spec| tool_name(spec.name)).collect()
}

/// The catalogue entries the main agent is actually given, whatever its
/// engine.
///
/// The CLI receives them as MCP routes; the local model of epic 10 as
/// OpenAI-style functions. One filter for both, so `run_action` cannot be
/// withheld from one post and offered on the other.
pub fn published() -> Vec<tools::ToolSpec> {
    tools::catalogue()
        .into_iter()
        .filter(|spec| !unbacked(spec.name))
        .collect()
}

/// The main agent's own MCP server.
#[derive(Clone)]
pub struct VoiceTools {
    core: Arc<Core>,
    tool_router: ToolRouter<VoiceTools>,
}

impl VoiceTools {
    pub fn new(core: Arc<Core>) -> Self {
        let mut tool_router = ToolRouter::new();
        for spec in tools::catalogue() {
            if unbacked(spec.name) {
                continue;
            }
            tool_router.add_route(route(spec));
        }
        // And the relay the CLI asks before running one of them. It is served
        // here but never named in `--tools`, so the model does not see it:
        // what the CLI calls internally and what the model may call are two
        // different lists, and this tool belongs only to the first.
        tool_router.add_route(approval_route());
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

    /// Find what ARIA knows, on the fast path (decisions 89, 97).
    ///
    /// Answers with the page's **id** alongside its two sentences, and that is
    /// not decoration: decision 91 made correction a substitution, so the model
    /// needs the id of what it just read in order to fix it without searching
    /// again.
    fn recall(&self, arguments: JsonObject) -> CallToolResult {
        let Some(query) = text(&arguments, "query") else {
            return refuse("Il manque les mots à chercher.");
        };
        match self.core.memory_recall(&query) {
            Ok(Some(found)) => CallToolResult::success(vec![ContentBlock::text(format!(
                "page {} : {}",
                found.id, found.resume
            ))]),
            // Said plainly, because the model's instruction is to say it rather
            // than fill the silence.
            Ok(None) => CallToolResult::success(vec![ContentBlock::text(
                "Rien en mémoire sur ce sujet.".to_string(),
            )]),
            Err(e) => memory_refusal(e),
        }
    }

    /// The deep half, summarised (decision 105).
    ///
    /// Three pages at most, each as its id and its two sentences — the shape
    /// [`crate::store::Memory::deep_recall`] exists for. Synchronous like
    /// [`VoiceTools::recall`]: a query is one forward pass, which the
    /// measurement of 2026-11-11 puts at about twenty milliseconds, and the
    /// filler is already speaking.
    fn deep_recall(&self, arguments: JsonObject) -> CallToolResult {
        let Some(query) = text(&arguments, "query") else {
            return refuse("Il manque les mots à chercher.");
        };
        match self.core.memory_deep(&query, 3) {
            Ok(found) if found.is_empty() => CallToolResult::success(vec![ContentBlock::text(
                "Rien en mémoire sur ce sujet.".to_string(),
            )]),
            Ok(found) => {
                let said = found
                    .iter()
                    .map(|one| format!("page {} : {}", one.id, one.resume))
                    .collect::<Vec<_>>()
                    .join("\n");
                CallToolResult::success(vec![ContentBlock::text(said)])
            }
            Err(e) => memory_refusal(e),
        }
    }

    /// Keep a fact. The model names the page; the core does not guess
    /// (decision 97).
    ///
    /// **On a blocking thread, and decision 103 is why.** The write now carries
    /// a forward pass of the vectoriser, so it is tens of milliseconds and not
    /// microseconds. Decision 63 measured what holding a runtime worker that
    /// long does here: the synthesis misses its deadline and the voice comes out
    /// chopped. `app/` already sends every model load through `spawn_blocking`
    /// for the same reason; this is that rule applied to the memory.
    async fn remember(&self, arguments: JsonObject) -> CallToolResult {
        let Some(fact) = text(&arguments, "fact") else {
            return refuse("Il manque le fait à retenir.");
        };
        let page = text(&arguments, "page");
        let core = Arc::clone(&self.core);
        let written = tokio::task::spawn_blocking(move || {
            core.with_memory(|memory| memory.remember(&fact, page.as_deref()))
        })
        .await;
        match written {
            Ok(Ok(id)) => {
                CallToolResult::success(vec![ContentBlock::text(format!("Noté dans {id}."))])
            }
            Ok(Err(e)) => memory_refusal(e),
            // The blocking task panicked. Answered in words rather than
            // propagated: the model can retry, and a protocol error would end
            // its turn instead.
            Err(e) => {
                tracing::error!("écriture en mémoire interrompue : {e}");
                refuse("La mémoire n'a pas pu être écrite.")
            }
        }
    }

    /// Correct a fragment, never the header (decision 91).
    ///
    /// Blocking thread for the same reason as [`VoiceTools::remember`]: a
    /// correction rewrites the page, so it re-embeds it (decision 103).
    async fn correct(&self, arguments: JsonObject) -> CallToolResult {
        let (Some(page), Some(from), Some(to)) =
            (text(&arguments, "page"), text(&arguments, "from"), text(&arguments, "to"))
        else {
            return refuse("Il manque la page, le texte à remplacer ou le nouveau.");
        };
        let core = Arc::clone(&self.core);
        let corrected = tokio::task::spawn_blocking(move || {
            core.with_memory(|memory| memory.replace(&page, &from, &to))
        })
        .await;
        match corrected {
            Ok(Ok(())) => CallToolResult::success(vec![ContentBlock::text("Corrigé.".to_string())]),
            Ok(Err(e)) => memory_refusal(e),
            Err(e) => {
                tracing::error!("correction en mémoire interrompue : {e}");
                refuse("La correction n'a pas pu être écrite.")
            }
        }
    }

    /// Rename a page, with the three obligations of decision 98 kept by the
    /// core rather than asked of the model.
    ///
    /// Blocking thread like the other two writes: a new title changes the
    /// embedded text, so the page is re-embedded (decision 103).
    async fn rename(&self, arguments: JsonObject) -> CallToolResult {
        let (Some(page), Some(title)) = (text(&arguments, "page"), text(&arguments, "title"))
        else {
            return refuse("Il manque la page à renommer ou son nouveau titre.");
        };
        let core = Arc::clone(&self.core);
        let renamed = tokio::task::spawn_blocking(move || {
            core.with_memory(|memory| memory.rename(&page, &title))
        })
        .await;
        match renamed {
            // The new id is said back, because it is what every later call
            // needs: the model holds the old one and nothing else would tell it.
            Ok(Ok(id)) => CallToolResult::success(vec![ContentBlock::text(format!(
                "Renommée en {id}."
            ))]),
            Ok(Err(e)) => memory_refusal(e),
            Err(e) => {
                tracing::error!("renommage en mémoire interrompu : {e}");
                refuse("Le renommage n'a pas pu être écrit.")
            }
        }
    }

    /// Hand a task to a session.
    async fn delegate(&self, arguments: JsonObject) -> CallToolResult {
        let call: DelegateTask = match serde_json::from_value(arguments.into()) {
            Ok(call) => call,
            // Returned as a tool error, which the model reads and can act on,
            // rather than as a protocol error, which ends its turn with
            // nothing said. A 4B-class model gets a schema wrong occasionally
            // and the recovery has to be a retry, not silence.
            Err(e) => return refuse(format!("La tâche n'a pas pu être lue : {e}")),
        };

        // Resolved per call, not held: the provider is attached at start-up and
        // may be swapped in the settings screen, and a conversation that
        // outlives the swap must follow it.
        let Some(provider) = self.core.provider() else {
            return refuse("Aucun moteur n'est attaché : la tâche n'a pas été lancée.");
        };
        let Some(conversation) = self.core.conversation() else {
            return refuse("ARIA n'a pas de conversation en cours.");
        };

        match conversation.delegate(provider.as_ref(), call).await {
            // `PENDING`, every time. The result is not in here, and
            // `super::super::agent::tools` is where that is explained to the
            // model.
            // `PENDING`, plus what the user has already been told out loud.
            // The model reads it and adapts instead of announcing a departure
            // ARIA has just announced — see `Conversation::note_waiting`.
            Ok(answer) => CallToolResult::success(vec![ContentBlock::text(
                match conversation.waiting_phrase() {
                    Some(phrase) => format!(
                        "{answer} ARIA vient de dire « {phrase} » à voix haute : \
                         l'utilisateur sait déjà que tu t'en occupes. Ne le redis pas, \
                         sous aucune forme, et n'ajoute rien — attends le résultat.",
                    ),
                    // Lost the race against the tool call, which is rare and
                    // harmless: the model simply learns nothing, and
                    // `sentences::Pending::mute` still stops the stutter.
                    None => answer.to_string(),
                },
            )]),
            Err(e) => {
                tracing::warn!("la délégation a échoué : {e}");
                // The user hears about this through the model, which is the
                // one path that works here: the launch failed, so no session
                // exists to report `task.error`, and the waiting phrase has
                // already been said out loud.
                refuse("La tâche n'a pas pu être lancée.")
            }
        }
    }
}

#[rmcp::tool_handler(router = self.tool_router)]
impl rmcp::ServerHandler for VoiceTools {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Outils du poste ARIA. Appelés par l'agent vocal pour confier du \
             travail à une session.",
        )
    }
}

/// One catalogue entry, as a route.
fn route(spec: tools::ToolSpec) -> ToolRoute<VoiceTools> {
    let schema = Arc::new(
        spec.schema
            .as_object()
            .cloned()
            // A tool whose schema is not an object would be refused by the CLI
            // at start-up. An empty one is the degraded form that still loads.
            .unwrap_or_default(),
    );
    ToolRoute::new_dyn(
        Tool::new(spec.name, spec.description, schema),
        move |context| Box::pin(dispatch(context)),
    )
}

/// The relay, as a route. Not in [`allowed_tools`], on purpose.
fn approval_route() -> ToolRoute<VoiceTools> {
    let schema = Arc::new(
        serde_json::to_value(schemars::schema_for!(super::ApproveParams))
            .ok()
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default(),
    );
    ToolRoute::new_dyn(
        Tool::new(
            super::TOOL_NAME,
            "Vérifie qu'un outil du poste ARIA peut être exécuté.",
            schema,
        ),
        move |context| Box::pin(dispatch(context)),
    )
}

impl VoiceTools {
    /// Run one of the main agent's tools, by its bare name.
    ///
    /// The MCP router calls this, and so does the local model's tool loop
    /// (epic 10) — one set of handlers, so the two engines cannot disagree
    /// about what a tool does. The relay's `approve` is not here: it answers
    /// the CLI's permission question, which only the CLI asks.
    pub async fn call(&self, name: &str, arguments: JsonObject) -> CallToolResult {
        match name {
            tools::DELEGATE_TASK => self.delegate(arguments).await,
            // `recall` is SQLite in memory and answers without an await. The
            // two writes do not: decision 103 put a forward pass on the write,
            // so they go through `spawn_blocking` and are awaited. Either way
            // the core's memory lock is never held across an await — it is
            // taken and released inside the closure.
            tools::RECALL => self.recall(arguments),
            tools::DEEP_RECALL => self.deep_recall(arguments),
            tools::REMEMBER => self.remember(arguments).await,
            tools::STR_REPLACE => self.correct(arguments).await,
            tools::RENAME => self.rename(arguments).await,
            // Unreachable through the router, which only carries what it
            // published; reachable from a local model, which can name
            // anything. Answered rather than panicked: a tool name is input.
            other => refuse(format!("ARIA ne connaît pas l'outil {other}.")),
        }
    }
}

async fn dispatch(context: ToolCallContext<'_, VoiceTools>) -> Result<CallToolResponse, ErrorData> {
    let arguments = context.arguments.clone().unwrap_or_default();
    let result = match context.name.as_ref() {
        super::TOOL_NAME => decide(arguments),
        other => context.service.call(other, arguments).await,
    };
    Ok(result.into())
}

/// Answer the CLI's permission question for the main agent's post.
fn decide(arguments: JsonObject) -> CallToolResult {
    let value = serde_json::Value::Object(arguments);
    // The shape is undocumented and both spellings have been seen, which is
    // why it goes through the same struct the relay uses.
    let params: super::ApproveParams = match serde_json::from_value(value.clone()) {
        Ok(params) => params,
        Err(e) => {
            tracing::warn!("demande d'autorisation illisible : {e}");
            return super::deny("ARIA n'a pas pu lire cette demande.");
        }
    };

    if approve(&params.tool_name) {
        tracing::debug!("poste vocal : {} autorisé", params.tool_name);
        // The input goes back unchanged, as the relay does: rewriting what was
        // asked for is a power neither of them takes.
        super::allow(&params.input)
    } else {
        // Loud, because it should be impossible: `--tools` restricts the
        // session to the catalogue, so a name that is not in it means the
        // restriction did not hold.
        tracing::warn!(
            "le poste vocal a refusé {} — un outil hors catalogue",
            params.tool_name
        );
        super::deny("Cet outil n'appartient pas au poste vocal d'ARIA.")
    }
}

fn refuse(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

/// One string argument, or nothing.
///
/// Permissive about an empty string, which a small model sends in place of
/// omitting an optional field — treated as absent rather than as a page named
/// `""`, which would be a confusing refusal for a correct intention.
fn text(arguments: &JsonObject, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Turn a memory failure into something a model can act on.
///
/// Every one of these is read out loud by way of the model, so each says what
/// to do next rather than what went wrong internally. `Detached` in particular
/// is not the model's fault and must not look like it.
fn memory_refusal(error: crate::store::MemoryError) -> CallToolResult {
    use crate::store::MemoryError::*;
    match error {
        Detached => refuse("La mémoire d'ARIA n'est pas disponible pour le moment."),
        NoSuchPage(name) => {
            refuse(format!("Aucune page ne s'appelle {name}. Cherche-la d'abord avec recall."))
        }
        NotUnique { found, .. } => refuse(format!(
            "Ce fragment apparaît {found} fois : donne-en un plus long pour le distinguer."
        )),
        NoMatch { .. } => {
            refuse("Ce fragment n'est pas dans la page : relis-la avec recall avant de corriger.")
        }
        WouldOverwrite(id) => refuse(format!("La page {id} existe déjà : choisis un autre titre.")),
        Embed(e) => {
            // The deep half is optional by construction (decision 83), so this
            // is worth a log and a soft answer, never a hard failure.
            tracing::warn!("vectoriseur indisponible : {e}");
            refuse("La recherche approfondie n'est pas disponible ; les mots-clés, si.")
        }
        other => {
            tracing::warn!("mémoire : {other}");
            refuse("La mémoire n'a pas pu être écrite.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_names_are_the_ones_the_flag_wants() {
        assert_eq!(
            tool_name(tools::DELEGATE_TASK),
            "mcp__aria__delegate_task",
            "`--tools` names a tool as mcp__<server>__<tool>, and the server \
             name has to match the key in --mcp-config"
        );
    }

    /// What is published and what is permitted are derived from one list, so
    /// they cannot drift — and the drift would be silent: a model that sees a
    /// tool and is refused when it calls it.
    #[test]
    fn everything_the_model_may_call_is_also_allowed_by_the_relay() {
        for name in allowed_tools() {
            assert!(approve(&name), "{name}");
        }
    }

    /// The short path has no executor yet, so it is not offered. When
    /// `crate::actions` grows a registry this test is what says so.
    ///
    /// The memory tools, by contrast, **are** offered: the core stands behind
    /// them as of 2026-11-11, and a run with no memory folder attached answers
    /// with a refusal a model can read rather than being absent.
    #[test]
    fn the_short_path_is_not_offered_while_nothing_stands_behind_it() {
        assert_eq!(
            allowed_tools(),
            vec![
                "mcp__aria__delegate_task".to_string(),
                "mcp__aria__recall".to_string(),
                "mcp__aria__deep_recall".to_string(),
                "mcp__aria__remember".to_string(),
                "mcp__aria__str_replace".to_string(),
                "mcp__aria__rename_page".to_string(),
            ]
        );
        assert!(
            !VoiceTools::new(Core::new())
                .published()
                .contains(&tools::RUN_ACTION.to_string())
        );
    }

    /// Decision 103, proven where the user asked for it: through the tool.
    ///
    /// Nothing is called between the tool answering and the question asked of
    /// the memory. `pending_vectors` reading zero means the forward pass and
    /// the row in SQLite both happened inside `remember`.
    #[tokio::test]
    async fn the_write_tool_vectorises_the_page_before_it_answers() {
        use crate::store::memory::semantic::tests::Bag;
        use crate::store::{Memory, Root};
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut memory = Memory::open(Root::open(dir.path()).expect("root")).expect("memory");
        memory
            .attach_embedder(Arc::new(Bag::new(&["louis", "cafe", "moulin"])), None)
            .expect("attach");

        let core = Arc::new(Core::new());
        core.attach_memory(memory);
        let tools = VoiceTools::new(Arc::clone(&core));

        let answer = tools
            .remember(
                serde_json::json!({ "fact": "Il prend son cafe serre.", "page": "profil/louis" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            )
            .await;
        let said = format!("{:?}", answer.content);
        assert!(said.contains("profil/louis"), "{said}");
        assert_ne!(answer.is_error, Some(true), "{said}");

        let pending = core.with_memory(|memory| Ok(memory.pending_vectors())).expect("memoire");
        assert_eq!(pending, 0, "l'ecriture devait vectoriser avant de repondre");
        assert!(core.with_memory(|memory| Ok(memory.deep_ready())).expect("memoire"));
    }

    /// Decision 98's three obligations, reached through the tool.
    ///
    /// The point of a tool of its own: the model says a title and gets a new id
    /// back, and the core has done the three things it cannot be asked to do
    /// — rewritten the inbound link, kept the former id working, moved the
    /// history. All three are asserted here, because separately they are
    /// worthless: a rename that keeps the alias but breaks the links is as bad
    /// as one that does neither.
    #[tokio::test]
    async fn the_rename_tool_keeps_the_links_and_the_former_id_working() {
        use crate::store::{Memory, Root};
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("tempdir");
        let mut memory = Memory::open(Root::open(dir.path()).expect("root")).expect("memory");
        memory.remember("Il prend son café serré.", Some("profil/utilisateur")).expect("page");
        memory
            .remember("Voir [[profil/utilisateur]] pour le détail.", Some("veille/sujets"))
            .expect("lien");
        // A second page in the same folder, for the collision below. The folder
        // matters: a rename changes the **leaf** and keeps the folder, so
        // `veille/sujets` renamed "Louis" becomes `veille/louis` and collides
        // with nothing. Learned by writing this assertion wrong.
        memory.remember("Un homonyme.", Some("profil/autre")).expect("homonyme");

        let core = Arc::new(Core::new());
        core.attach_memory(memory);
        let tools = VoiceTools::new(Arc::clone(&core));

        let answer = tools
            .rename(
                serde_json::json!({ "page": "profil/utilisateur", "title": "Louis" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            )
            .await;
        let said = format!("{:?}", answer.content);
        assert_ne!(answer.is_error, Some(true), "{said}");
        // The new id is said back, which is what every later call needs.
        assert!(said.contains("profil/louis"), "{said}");

        // 1. the inbound link follows, and 2. the former id still resolves.
        let found = core.memory_recall("profil/utilisateur").expect("recall").expect("page");
        assert_eq!(found.id.as_str(), "profil/louis");
        let linked = core
            .with_memory(|memory| {
                Ok(memory
                    .page(&crate::store::memory::page::PageId::new("veille/sujets").unwrap())
                    .map(|page| page.body.clone())
                    .unwrap_or_default())
            })
            .expect("mémoire");
        assert!(linked.contains("[[profil/louis]]"), "{linked}");

        // And a second page already holding the title is refused in words,
        // never by overwriting it.
        let refused = tools
            .rename(
                serde_json::json!({ "page": "profil/autre", "title": "Louis" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            )
            .await;
        assert_eq!(refused.is_error, Some(true), "{:?}", refused.content);
    }

    /// The policy of the post, as a rule rather than a list.
    #[test]
    fn only_arias_own_tools_are_allowed_here() {
        assert!(approve("mcp__aria__delegate_task"));
        // Not published, so not allowed — the two follow one list.
        //
        // If you are here because this line started failing, you have just
        // given `run_action` an executor, and this assertion is the one thing
        // that noticed: the rule above now pre-approves every action in the
        // registry, whatever it does. Make `approve` read the `action`
        // argument and its risk before you change this line. See `unbacked`.
        assert!(!approve("mcp__aria__run_action"));
        // And nothing from the coding agent, whatever it is called.
        for tool in [
            "Bash",
            "Write",
            "mcp__other__delegate_task",
            "delegate_task",
        ] {
            assert!(!approve(tool), "{tool}");
        }
    }

    /// The relay is served but never offered: what the CLI calls internally
    /// and what the model may call are two different lists.
    #[test]
    fn the_relay_is_on_the_wire_and_not_in_the_catalogue() {
        let published = VoiceTools::new(Core::new()).published();
        assert!(published.contains(&super::super::TOOL_NAME.to_string()));
        assert!(!allowed_tools().contains(&permission_tool_name()));
        assert_eq!(permission_tool_name(), "mcp__aria__approve");
    }

    #[test]
    fn a_tool_from_outside_the_catalogue_is_refused_with_a_reason() {
        let verdict = decide(
            serde_json::json!({ "tool_name": "Bash", "input": { "command": "rm -rf /" } })
                .as_object()
                .cloned()
                .unwrap(),
        );
        let text = format!("{:?}", verdict.content);
        assert!(text.contains("deny"), "{text}");
        assert!(!text.contains("allow"), "{text}");
    }

    #[test]
    fn a_delegation_is_allowed_with_its_arguments_untouched() {
        let verdict = decide(
            serde_json::json!({
                "tool_name": "mcp__aria__delegate_task",
                "input": { "task": "chercher la météo" }
            })
            .as_object()
            .cloned()
            .unwrap(),
        );
        let text = format!("{:?}", verdict.content);
        assert!(text.contains("allow"), "{text}");
        assert!(text.contains("chercher la météo"), "{text}");
    }

    /// The schema travels from the catalogue to the wire intact. The CLI
    /// refuses a tool whose schema is not an object, at start-up, for the
    /// whole session.
    #[test]
    fn the_schema_reaches_the_wire_as_an_object() {
        let tools = VoiceTools::new(Core::new()).tool_router.list_all();
        let delegate = tools
            .iter()
            .find(|t| t.name == tools::DELEGATE_TASK)
            .expect("delegate_task is published");
        assert_eq!(
            delegate.input_schema.get("type").and_then(|v| v.as_str()),
            Some("object")
        );
        assert!(delegate.input_schema.contains_key("properties"));
        // The sentence that makes the mechanism work travels with it.
        assert!(
            delegate
                .description
                .as_deref()
                .is_some_and(|d| d.contains("plus tard"))
        );
    }

    #[tokio::test]
    async fn a_delegation_with_no_engine_attached_says_so_instead_of_hanging() {
        let server = VoiceTools::new(Core::new());
        let result = server
            .delegate(
                serde_json::json!({ "task": "ranger les factures" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            )
            .await;
        assert_eq!(result.is_error, Some(true));
        assert!(format!("{:?}", result.content).contains("moteur"));
    }

    #[tokio::test]
    async fn a_task_that_is_not_a_task_is_refused_as_text_not_as_a_protocol_error() {
        let server = VoiceTools::new(Core::new());
        let result = server
            .delegate(
                serde_json::json!({ "tache": 12 })
                    .as_object()
                    .cloned()
                    .unwrap(),
            )
            .await;
        assert_eq!(result.is_error, Some(true));
        // The model has to be able to read why and call again.
        assert!(format!("{:?}", result.content).contains("pas pu être lue"));
    }
}
