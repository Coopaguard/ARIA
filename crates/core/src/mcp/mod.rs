//! The MCP server ARIA exposes to its sessions.
//!
//! Carries the tool named by `--permission-prompt-tool`, which is how the
//! relay receives requests. See epic 02.
//!
//! # How a request finds its way back to the right session
//!
//! Epic 02's exit condition is precise about this: two sessions run at once,
//! each asks for permission, approving one must unblock only that one. The CLI
//! controls the tool's arguments, so there is nowhere to put a session id in
//! them — the routing has to be in the **endpoint**.
//!
//! Each session is therefore launched against its own URL:
//!
//! ```text
//! http://127.0.0.1:7421/mcp?s=<opaque token>
//! ```
//!
//! and the token is minted at launch, handed to that session alone, and
//! forgotten when it ends. It does two jobs at once: it says which session is
//! calling, and it is the only thing that makes the endpoint reachable. A
//! second process that guessed the port still has nothing to say.
//!
//! The query string rather than a path segment because the transport is
//! mounted once, at a fixed path; a path parameter would need one mount per
//! session.
//!
//! # The call blocks, and that is the whole mechanism
//!
//! Claude Code calls this tool and **waits** for the verdict before running
//! anything. So the handler files the request, publishes it, and awaits — for
//! as long as it takes. A pending request does not expire (decision 8), which
//! means this await has no timeout, deliberately. The session is blocked, the
//! interface says so, and nothing is lost.

pub mod subagent;
pub mod voice;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData, tool, tool_router};

use crate::Core;
use crate::permissions::{Decision, Risk};
use crate::sessions::SessionId;

/// The name the server is given in `--mcp-config`.
///
/// It becomes part of the tool's full name, which is what
/// `--permission-prompt-tool` has to be passed.
pub const SERVER_NAME: &str = "aria";

/// The tool's own name.
pub const TOOL_NAME: &str = "approve";

/// The relay, before it has a name.
///
/// What a provider is handed, so **it** spells the name in its own convention
/// — `mcp__aria__approve` on Claude Code, something else elsewhere. The leak
/// sealed on 2026-11-12: this function used to return the Claude Code spelling
/// to engine-agnostic code.
pub fn relay_tool() -> crate::providers::McpTool {
    crate::providers::McpTool::new(SERVER_NAME, TOOL_NAME)
}

/// The relay as **this** build\x27s engine names it.
///
/// Kept for the voice post and for the settings screen, both of which are
/// Claude Code today. Everything that crosses the provider boundary uses
/// [`relay_tool`] instead.
pub fn relay_tool_name() -> String {
    relay_tool().render(crate::providers::McpToolNaming::DoubleUnderscore)
}

/// The `s=` token out of a query string.
///
/// Both MCP endpoints carry one and for the same reason — loopback keeps the
/// network out and nothing else — so the parsing lives in one place. It is
/// deliberately not a full query parser: the only producer is
/// [`crate::transport::relay_url`] and its sibling.
pub fn token_in(query: &str) -> Option<&str> {
    query.split('&').find_map(|pair| pair.strip_prefix("s="))
}

/// Which ARIA session is behind which endpoint token.
///
/// Minted at launch, dropped when the session ends. A token nobody holds is an
/// endpoint nobody can call.
#[derive(Debug, Default)]
pub struct SessionRoutes {
    by_token: Mutex<HashMap<String, SessionId>>,
}

impl SessionRoutes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mint a token for a session and return it.
    pub fn open(&self, session: SessionId) -> String {
        let token = uuid::Uuid::new_v4().to_string();
        self.by_token.lock().unwrap().insert(token.clone(), session);
        token
    }

    pub fn resolve(&self, token: &str) -> Option<SessionId> {
        self.by_token.lock().unwrap().get(token).copied()
    }

    /// Forget a session's endpoint. Called when it ends.
    pub fn close(&self, session: SessionId) {
        self.by_token
            .lock()
            .unwrap()
            .retain(|_, held| *held != session);
    }

    pub fn len(&self) -> usize {
        self.by_token.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What the CLI sends when it asks for permission.
///
/// Every field is optional on purpose. This shape is **not documented** —
/// `docs/cli/claude-code-cli.md` §4 records that the official page for the flag is thin, and the
/// field names below were read off the real CLI. A rename upstream must cost a
/// vaguer prompt, not a session that cannot start.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ApproveParams {
    /// The tool Claude wants to run.
    #[serde(default, alias = "toolName")]
    pub tool_name: String,
    /// Its arguments, as the tool declares them.
    #[serde(default)]
    pub input: serde_json::Value,
    /// The id of the tool call, echoed back by some versions.
    #[serde(default, alias = "toolUseId")]
    pub tool_use_id: Option<String>,
}

/// What a session hands over to show something.
///
/// Two fields and no more. A position, a size or a grid slot would all be
/// state the user then fights with — decision 11 says the layout is not even
/// persisted, so letting a model choose one would be giving it a say in
/// something nobody keeps.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ShowTileParams {
    /// Optional. A short heading above the tile; leave it empty when the
    /// content already says what it is.
    #[serde(default)]
    pub title: String,
    /// A fragment of semantic HTML, with no styling of its own.
    ///
    /// Not a document: the core wraps it and applies the look — see
    /// [`crate::tiles::Tile::document`] for why that is the core's job.
    #[serde(default, alias = "html")]
    pub body: String,
    /// Files the tile may show, each under the name it refers to.
    ///
    /// Served from the sandbox origin where they lie — nothing is copied —
    /// so the tile writes `src="f/<name>"` and a file the session rewrites
    /// is the file the tile shows next time it loads.
    #[serde(default)]
    pub files: Vec<crate::tiles::TileFile>,
    /// Fields to ask the user for, **declared** rather than drawn.
    ///
    /// The window renders them outside the sandboxed frame, which is what
    /// makes the answer the user's rather than the tile's. See
    /// [`crate::tiles::Form`].
    #[serde(default)]
    pub form: Option<crate::tiles::Form>,
}

/// Whether a tile's body is trying to be a document or to dress itself.
///
/// Two refusals, and they are the same refusal twice: the look belongs to the
/// core. A `<style>` block or a `style=` attribute wins over the sheet the
/// core applies, so one tile would come out in its own colours; a full
/// document would nest a second `<html>` inside the one that wraps it.
///
/// Matched on the lowercased text rather than parsed. That is enough for what
/// this is — a **guard rail for a cooperating model**, not a sanitiser. What
/// actually contains a tile is the separate origin and
/// [`crate::tiles::DEFAULT_CSP`]; this only stops an honest mistake from
/// reaching the screen, which is what the user asked for on 2026-10-30.
fn overrides_the_design(body: &str) -> Option<String> {
    let lower = body.to_lowercase();
    if ["<!doctype", "<html", "<head", "<body"]
        .iter()
        .any(|tag| lower.contains(tag))
    {
        return Some(
            "`body` est un fragment, pas un document : enlève <!doctype>, <html>, \
             <head> et <body>, et ne garde que le contenu."
                .into(),
        );
    }
    if lower.contains("<style") || lower.contains("style=") {
        return Some(
            "N'écris aucun style : ARIA applique le sien par-dessus. Enlève <style> \
             et les attributs style=, et utilise les classes annoncées — big, label, \
             note, row, ok, warn, bad."
                .into(),
        );
    }
    None
}

/// Which tile to refresh, and optionally what to put in it.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RefreshTileParams {
    /// The `ui://` the tile was created under.
    pub uri: String,
    /// New markup. Omitted when only a file changed on disk.
    #[serde(default)]
    pub body: Option<String>,
}

/// The relay, as an MCP server.
#[derive(Clone)]
pub struct Relay {
    core: Arc<Core>,
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Relay>,
}

#[tool_router]
impl Relay {
    pub fn new(core: Arc<Core>) -> Self {
        Self {
            core,
            tool_router: Self::tool_router(),
        }
    }

    /// Ask the user whether a session may do something.
    ///
    /// Deliberately **not** marked as requiring user interaction. `docs/cli/claude-code-cli.md` §4
    /// records that a relay cannot approve a tool carrying that marker — an
    /// `allow` on one is converted into a refusal — so marking our own would
    /// make it uncallable, which is the opposite of its job.
    #[tool(
        name = "approve",
        description = "Demande à l'utilisateur d'ARIA l'autorisation d'exécuter une action."
    )]
    pub async fn approve(
        &self,
        Parameters(params): Parameters<ApproveParams>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        tracing::debug!("relay: {} {}", params.tool_name, params.input);
        let Some(session) = self.session_of(&context) else {
            // No token, or one we no longer hold. Refusing is the only safe
            // answer: an unrouted approval would be an approval nobody asked
            // for, granted to a caller we cannot identify.
            tracing::warn!("a permission request arrived on an unrouted endpoint");
            return Ok(deny(
                "ARIA n'a pas pu rattacher cette demande à une session.",
            ));
        };

        let command = command_in(&params.input);
        let decision = self
            .core
            .decide_permission(session, &params.tool_name, command.clone())
            .await;

        Ok(match decision {
            Decision::Allow | Decision::AllowAlways { .. } => {
                // The input goes back unchanged. Rewriting what Claude asked
                // for is a power this relay deliberately does not take: the
                // user approved the action they were shown, not a variant of
                // it.
                allow(&params.input)
            }
            Decision::Deny => deny("Refusé par l'utilisateur d'ARIA."),
        })
    }

    /// Put a tile on the user's screen.
    ///
    /// # Why a session is the one that calls this
    ///
    /// Epic 07: "la session appelle l'outil avec son HTML, le core la place
    /// dans la grille". The main agent is not a candidate — its whole prompt
    /// is one or two spoken sentences and a delegation — so the HTML comes
    /// from whoever did the work and has something to show.
    ///
    /// The result carries `_meta.ui.resourceUri`, which is how MCP Apps names
    /// the resource the host should render. The URI is **minted by the
    /// registry**, never taken from the arguments: see
    /// [`crate::tiles::Registry::add`].
    #[tool(
        name = "show_tile",
        description = "Affiche une tuile dans l'interface d'ARIA : un petit panneau \
             déplaçable, pour ce qui se montre mieux qu'il ne se dit — un relevé, un \
             tableau, une comparaison de chiffres. N'utilise pas cet outil pour du \
             texte ni pour une réponse d'une ligne : ça se prononce. \
             \
             `body` est un FRAGMENT de HTML sémantique, PAS un document : pas de \
             <!doctype>, pas de <html>, pas de <head>, pas de <body>. \
             N'ÉCRIS AUCUN STYLE — ni <style>, ni attribut style, ni couleur, ni \
             police, ni taille, et n'en pose pas non plus depuis un script. ARIA \
             applique son propre dessin par-dessus. Ces éléments sont déjà mis en \
             forme : h1 h2 h3, p, ul, ol, table (th, td, et class=\"num\" pour une \
             colonne de nombres), strong, em, code, hr. Et ces classes : \
             class=\"big\" pour le chiffre qu'on vient lire, class=\"label\" pour la \
             légende au-dessus d'un bloc, class=\"note\" pour une précision \
             secondaire, class=\"row\" pour aligner deux choses sur une ligne, \
             class=\"ok\" class=\"warn\" class=\"bad\" pour un état. \
             \
             La tuile est rendue UNE SEULE FOIS et n'est jamais rafraîchie, et elle \
             n'a AUCUN accès au réseau. Une valeur que tu écris en dur est donc figée \
             à l'instant où tu l'écris : une heure écrite ainsi restera l'heure qu'il \
             était. Si quelque chose doit continuer à bouger ET que le navigateur \
             peut le calculer tout seul — une horloge, un compte à rebours, un temps \
             écoulé —, mets un petit <script> en ligne : il s'exécute. Il ne peut \
             rien télécharger, donc une donnée qui vient d'ailleurs ne peut pas se \
             mettre à jour : n'en montre que ce que tu sais au moment de l'appel. \
             \
             Pour envoyer l'utilisateur sur un site, n'écris pas un <a href> : la \
             tuile ne navigue pas. Appelle `ui/open-link` en JSON-RPC sur \
             postMessage, avec {\"url\": \"https://…\"} — ça ouvre SON navigateur, \
             avec sa session et son plein écran. Seuls http et https passent. \
             \
             Un PDF déclaré dans `files` est affiché par ARIA sous la tuile : ne \
             l'intègre pas toi-même dans `body` (ni <embed>, ni <iframe>), il y \
             resterait blanc. \
             \
             Pour lui DEMANDER quelque chose, n'écris pas de <input> ni de <form> \
             dans `body` : ils seraient dans le bac à sable, et ce que tu en lirais \
             ne serait pas attribuable à l'utilisateur. Déclare-le dans `form` et \
             ARIA le dessine lui-même, hors de la tuile. Au plus six champs, \
             chacun avec un `name` unique et un `kind` parmi text, number, choice \
             (donne au moins deux `options`) et toggle. La réponse te reviendra \
             comme un message, même si ta session s'est terminée entre-temps."
    )]
    pub async fn show_tile(
        &self,
        Parameters(params): Parameters<ShowTileParams>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let Some(session) = self.session_of(&context) else {
            // Same answer as an unrouted approval, and for the same reason: a
            // tile from a caller we cannot name is a tile we cannot attribute
            // in the interface.
            tracing::warn!("une tuile est arrivée sur un point d'accès non routé");
            return Ok(deny("ARIA n'a pas pu rattacher cette tuile à une session."));
        };
        if params.body.trim().is_empty() {
            return Ok(deny(
                "Une tuile vide ne s'affiche pas : donne son contenu dans `body`.",
            ));
        }
        // Refused rather than stripped. Taking the markup apart would mean
        // parsing a language model's HTML with string matching, which is the
        // kind of thing that works until it does not; and a tile that was
        // silently reshaped is a tile whose author believes something else is
        // on screen. The message says what to send instead, so the retry is
        // one turn.
        if let Some(reason) = overrides_the_design(&params.body) {
            tracing::warn!("tuile refusée : {reason}");
            return Ok(deny(&reason));
        }
        // Same treatment as the body, and for the same reason: a form quietly
        // repaired is a form whose author believes something else is on
        // screen. The message says what to send instead.
        let files = params.files;
        if files.len() > crate::tiles::MAX_FILES {
            return Ok(deny(&format!(
                "Au plus {} fichiers par tuile.",
                crate::tiles::MAX_FILES
            )));
        }
        if let Some(bad) = files
            .iter()
            .find(|file| !crate::tiles::nameable(&file.name))
        {
            return Ok(deny(&format!(
                "« {} » ne peut pas servir de nom de fichier : pas de séparateur, \
                 pas de `..`, et rien d'exotique. Donne un nom simple comme `rapport.pdf`.",
                bad.name
            )));
        }

        let form = params.form;
        if let Some(reason) = form.as_ref().and_then(crate::tiles::Form::refused) {
            tracing::warn!("formulaire de tuile refusé : {reason}");
            return Ok(deny(&reason));
        }

        // Optional since 2026-11-16: a tile without a title shows only its
        // content, which is how the user wants most of them. An empty title
        // is kept empty rather than replaced by a word that says nothing.
        let title = params.title.trim().to_string();
        let uri = self
            .core
            .show_tile(session, title, params.body, files, form);
        tracing::info!("tuile affichée : {uri}");

        // The URI is in the text and not only in `_meta`: the CLI does not
        // show `_meta` to the model, and without the URI a session asked to
        // add something to its tile can only draw a second one.
        let mut result = CallToolResult::success(vec![ContentBlock::text(format!(
            "La tuile {uri} est affichée. Ne décris pas son contenu : l'utilisateur le \
             voit. Pour la compléter ensuite, refresh_tile avec cette uri, pas une \
             nouvelle tuile."
        ))]);
        result.meta = Some(rmcp::model::MetaObject(
            serde_json::json!({ "ui": { "resourceUri": uri } })
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ));
        Ok(result)
    }

    /// Refresh a tile the session already put on screen.
    ///
    /// The URI came back in `_meta.ui.resourceUri` when the tile was created.
    #[tool(
        name = "refresh_tile",
        description = "Actualise une tuile déjà affichée, sans en créer une seconde : \
             l'utilisateur garde le panneau là où il l'a mis et à la taille qu'il lui \
             a donnée. `uri` est celle que `show_tile` t'a rendue. \
             Si tu as seulement RÉÉCRIT UN FICHIER sur le disque, c'est tout ce qu'il \
             faut : ARIA sert les fichiers là où ils sont, donc le nouveau contenu est \
             déjà le bon — il n'y a qu'à faire relire la tuile. Ne donne `body` que si \
             le HTML lui-même a changé."
    )]
    pub async fn refresh_tile(
        &self,
        Parameters(params): Parameters<RefreshTileParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let body = params.body.filter(|text| !text.trim().is_empty());
        if let Some(body) = body.as_deref()
            && let Some(reason) = overrides_the_design(body)
        {
            return Ok(deny(&reason));
        }
        if self.core.refresh_tile(&params.uri, body) {
            tracing::info!("tuile actualisée : {}", params.uri);
            Ok(CallToolResult::success(vec![ContentBlock::text(
                "La tuile est actualisée.",
            )]))
        } else {
            // Not an error: the user may simply have closed it.
            Ok(deny(
                "Cette tuile n'existe plus — l'utilisateur l'a fermée. \
                 Appelle `show_tile` si tu veux en remettre une.",
            ))
        }
    }

    /// Read the endpoint token out of the HTTP request.
    fn session_of(
        &self,
        context: &rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Option<SessionId> {
        let parts = context.extensions.get::<http::request::Parts>()?;
        let token = token_in(parts.uri.query()?)?;
        self.core.session_routes().resolve(token)
    }
}

#[rmcp::tool_handler(router = self.tool_router)]
impl rmcp::ServerHandler for Relay {
    fn get_info(&self) -> ServerConfig {
        // Tools, and nothing else. A relay advertising resources or prompts
        // would be claiming a surface it does not have.
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Relais d'autorisation d'ARIA. Appelé par Claude Code avant \
                 d'exécuter une action qui demanderait confirmation.",
        )
    }
}

/// The verdict shape the CLI expects.
///
/// Not from the documentation — `docs/cli/claude-code-cli.md` §4 says the official page for the
/// flag is thin — but probed against CLI 2.1.165. The result is a text content
/// block whose text is this JSON object.
pub(crate) fn allow(input: &serde_json::Value) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(
        serde_json::json!({
            "behavior": "allow",
            "updatedInput": input,
        })
        .to_string(),
    )])
}

pub(crate) fn deny(message: &str) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(
        serde_json::json!({
            "behavior": "deny",
            "message": message,
        })
        .to_string(),
    )])
}

/// Pull the command out of a tool's arguments.
///
/// Tools spell it differently and new ones will spell it differently again, so
/// this tries the keys we know and gives up gracefully. Giving up means a
/// request the registry cannot match, which is **asked at medium risk** — the
/// safe direction.
pub fn command_in(input: &serde_json::Value) -> Option<String> {
    for key in ["command", "cmd", "script"] {
        if let Some(value) = input.get(key).and_then(|v| v.as_str()) {
            return Some(value.to_string());
        }
    }
    for key in ["file_path", "path", "notebook_path"] {
        if let Some(value) = input.get(key).and_then(|v| v.as_str()) {
            return Some(value.to_string());
        }
    }
    None
}

/// What the user is shown, in French.
pub fn describe(tool: &str, command: Option<&str>, risk: Risk) -> String {
    let what = match command {
        Some(command) => format!("{tool} — {command}"),
        None => tool.to_string(),
    };
    match risk {
        Risk::High => format!("{what} (irréversible)"),
        _ => what,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_relay_tool_name_is_the_one_the_flag_wants() {
        // `mcp__<server>__<tool>`, and it has to match the key used in
        // `--mcp-config` or the CLI looks for a tool that is not there.
        assert_eq!(relay_tool_name(), "mcp__aria__approve");
    }

    #[test]
    fn a_token_routes_to_its_session_and_to_no_other() {
        let routes = SessionRoutes::new();
        let first = SessionId::new();
        let second = SessionId::new();
        let a = routes.open(first);
        let b = routes.open(second);

        // Epic 02's exit condition in miniature: approving one must not
        // unblock the other.
        assert_eq!(routes.resolve(&a), Some(first));
        assert_eq!(routes.resolve(&b), Some(second));
        assert_ne!(a, b);
    }

    #[test]
    fn an_unknown_token_routes_nowhere() {
        let routes = SessionRoutes::new();
        routes.open(SessionId::new());
        assert_eq!(routes.resolve("not-a-token"), None);
    }

    #[test]
    fn closing_a_session_takes_its_endpoint_with_it() {
        let routes = SessionRoutes::new();
        let session = SessionId::new();
        let token = routes.open(session);
        routes.close(session);
        assert_eq!(routes.resolve(&token), None);
        assert!(routes.is_empty());
    }

    #[test]
    fn the_params_survive_a_field_being_renamed_upstream() {
        // The shape is undocumented. A rename must cost a vaguer prompt, not a
        // session that will not start.
        let params: ApproveParams =
            serde_json::from_value(serde_json::json!({ "something_else": 1 })).unwrap();
        assert_eq!(params.tool_name, "");
        assert_eq!(command_in(&params.input), None);
    }

    #[test]
    fn both_spellings_of_the_tool_name_are_accepted() {
        let snake: ApproveParams =
            serde_json::from_value(serde_json::json!({ "tool_name": "Bash" })).unwrap();
        let camel: ApproveParams =
            serde_json::from_value(serde_json::json!({ "toolName": "Bash" })).unwrap();
        assert_eq!(snake.tool_name, "Bash");
        assert_eq!(camel.tool_name, "Bash");
    }

    #[test]
    fn an_allow_returns_the_input_unchanged() {
        // The relay does not rewrite what Claude asked for: the user approved
        // the action they were shown, not a variant of it.
        let input = serde_json::json!({ "command": "git status" });
        let result = allow(&input);
        let text = format!("{:?}", result.content);
        assert!(text.contains("allow"));
        assert!(text.contains("git status"));
    }

    #[test]
    fn a_high_risk_action_says_so_in_what_the_user_reads() {
        let text = describe("Bash", Some("rm -rf build"), Risk::High);
        assert!(text.contains("irréversible"), "{text}");
    }
}

#[cfg(test)]
mod tile_guard {
    use super::overrides_the_design;

    /// Settled with the user on 2026-10-30: the tool returns plain content and
    /// the core applies the look. These are the two ways a model takes it back.
    #[test]
    fn a_document_and_a_stylesheet_are_both_refused() {
        for body in [
            "<!doctype html><html><body><p>23°</p></body></html>",
            "<html><p>23°</p></html>",
            "<head><title>x</title></head>",
            "<style>p{color:red}</style><p>23°</p>",
            "<p style=\"color:red\">23°</p>",
        ] {
            let refusal = overrides_the_design(body);
            assert!(refusal.is_some(), "accepté à tort : {body}");
            // The refusal has to say what to send instead, or the model
            // retries the same thing.
            let said = refusal.unwrap();
            assert!(
                said.contains("fragment") || said.contains("classes"),
                "{said}"
            );
        }
    }

    /// And the shape that is meant to go through, untouched.
    #[test]
    fn the_vocabulary_the_description_announces_is_accepted() {
        for body in [
            "<p class=\"label\">Lyon</p><p class=\"big\">12°</p>",
            "<table><tr><th>Jour</th><th class=\"num\">°C</th></tr>\
             <tr><td>Demain</td><td class=\"num\">12</td></tr></table>",
            "<h2>Prévision</h2><ul><li>Averses</li></ul><p class=\"note\">source locale</p>",
            "<p>Un <strong>total</strong> de <code>42</code>, <em>stable</em>.</p>",
        ] {
            assert_eq!(overrides_the_design(body), None, "refusé à tort : {body}");
        }
    }

    /// `lifestyle=` and `<styles>` are not what the guard is looking for, but
    /// a careless `contains` would say they are.
    #[test]
    fn the_guard_does_not_fire_on_a_word_that_merely_contains_style() {
        assert_eq!(overrides_the_design("<p>un style de vie</p>"), None);
    }

    /// A script is **allowed**, and this test is here so nobody tightens it by
    /// symmetry with `<style>`.
    ///
    /// Reported on 2026-10-30: a tile showing the time showed the time it was
    /// when it was written, for ever. A tile is rendered once and never
    /// refreshed, so anything that has to keep moving has to move itself —
    /// and an inline script is the only way it can, since
    /// [`crate::tiles::DEFAULT_CSP`] allows exactly that and nothing else.
    /// Decision 61 already pays for it; this is what uses it.
    #[test]
    fn a_clock_may_carry_the_script_that_makes_it_tick() {
        let clock = "<p class=\"label\">Heure locale</p><p class=\"big\" id=\"t\">–</p>\
             <script>setInterval(()=>{document.getElementById('t').textContent=\
             new Date().toLocaleTimeString('fr-FR')},1000)</script>";
        assert_eq!(
            overrides_the_design(clock),
            None,
            "une horloge doit pouvoir battre"
        );
    }
}
