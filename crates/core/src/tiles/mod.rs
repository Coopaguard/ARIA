//! MCP Apps (SEP-1865): `ui://` resources and the origin that serves them.
//!
//! The host renders a tile with `allow-scripts` AND `allow-same-origin`,
//! which is only safe because the tile is served from an origin distinct
//! from the application's. That second origin is served from here — proven
//! workable under Tauri on 2026-10-23 by a throwaway prototype.
//!
//! The real boundary is not the `sandbox` attribute: it is this process
//! rejecting a `tools/call` for a tool that does not carry `app` visibility.
//! A tile lives no longer than the window showing it.
//!
//! # Two origins, and what each one is for
//!
//! | | Served by | Origin |
//! |---|---|---|
//! | the window | Tauri | `http://tauri.localhost` |
//! | a tile | this module | `http://127.0.0.1:7420` |
//!
//! Distinct by scheme, host **and** port, which is what makes
//! `allow-same-origin` safe to grant: the tile gets a real origin of its own,
//! so it can run scripts and talk `postMessage`, but it is not the window's
//! origin and cannot reach into it.
//!
//! # Why nothing here is stored
//!
//! Decision 11: a tile lives as long as the application, and not a moment
//! longer. Its session may end and the tile stays; the application restarts
//! and the tile is gone. So this registry is a `HashMap` behind a `Mutex` and
//! that is the whole of it — no table, no file, no serialisation of the HTML,
//! and nothing for epic 05's thirty-day retention to apply to.
//!
//! # Why the port is pinned and failure is loud
//!
//! [`crate::transport::TILES_PORT`] is named literally in the window's CSP
//! `frame-src`. A server that fell back to another port would still start,
//! still answer, and the webview would refuse every tile it served — a feature
//! that looks implemented and shows nothing. So binding it is allowed to fail,
//! and says why.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::transport::TILES_PORT;

/// What SEP-1865 calls a tile's content type.
pub const MIME: &str = "text/html;profile=mcp-app";

/// The restrictive default of SEP-1865, applied when the server declared no
/// `ui.csp` metadata.
///
/// The host **may restrict further and may never allow a domain that was not
/// declared**, so this is the floor and not a suggestion. `frame-src 'none'`
/// is what stops a tile from nesting another frame to climb out;
/// `connect-src 'none'` is what stops it reaching the network, including the
/// event stream on the neighbouring port.
///
/// # Why `'unsafe-inline'` is here, and why it is not the hole it looks like
///
/// A tile is **one self-contained document**: the registry serves it at a
/// single path and there is nowhere for a second file to live. With
/// `script-src 'self'` an inline script is blocked and an external one has no
/// URL, so a tile could run no code at all — and would therefore never be able
/// to speak the `postMessage` JSON-RPC that makes it an MCP App rather than a
/// picture. A throwaway prototype served a second file for exactly this
/// reason; the registry does not, so the policy has to give instead.
///
/// What `'unsafe-inline'` normally protects against is injected script in a
/// document whose own script is trusted. Here there is no trusted script to
/// protect: **the whole document is the untrusted content**, written by a
/// model. Nothing is gained by forbidding the form it arrives in.
///
/// The containment is the rest of this policy: a separate origin, **no
/// network** (`connect-src 'none'`), and a `tools/call` filter in the host
/// process. A script that can only talk to its parent over `postMessage` is
/// exactly what the spec asks a view to be.
///
/// # Why `frame-src` and `object-src` say `'self'` and not `'none'`
///
/// Measured on 2026-11-01, by making the page report its own CSP violations:
/// Chromium's PDF viewer is a **nested browsing context**, so an
/// `<embed type="application/pdf">` is governed by `frame-src` — not by
/// `object-src`, which is what the reasoning said and the browser denied.
///
/// `'self'` is this sandbox origin alone. A tile still cannot frame a remote
/// site, so data cannot leave in the URL of a frame — the hole
/// `connect-src 'none'` closes stays closed. What `'self'` does allow is a
/// tile framing **another tile**, which would be same-origin and therefore
/// scriptable; that is refused by `X-Frame-Options: DENY` on every tile
/// document, and the files that need embedding do not carry it. The two have
/// to be read together.
pub const DEFAULT_CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; \
     style-src 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; \
     media-src 'self'; object-src 'self'; frame-src 'self'; \
     connect-src 'none'; base-uri 'self'";

/// The sandbox origin, written out.
///
/// Pinned by [`TILES_PORT`] and named literally in the window's CSP
/// `frame-src`, so this string and that policy have to agree — which is why it
/// is built in one place and handed to the window rather than written twice.
pub fn origin() -> String {
    format!("http://127.0.0.1:{TILES_PORT}")
}

/// The tool a session calls to put a tile on screen.
pub const SHOW_TILE: &str = "show_tile";

/// The tool a session calls to change a tile it already showed.
pub const REFRESH_TILE: &str = "refresh_tile";

/// Its name in the engine's spelling, which is what a permission verdict sees.
///
/// Takes the naming since 2026-11-12: `mcp__server__tool` is Claude Code's
/// convention, and this function is consulted about a tool name reported by a
/// **delegated session** — so the spelling belongs to whichever engine is
/// holding that session. See `crate::providers::McpToolNaming`.
pub fn show_tile_tool(naming: crate::providers::McpToolNaming) -> String {
    naming.render(crate::mcp::SERVER_NAME, SHOW_TILE)
}

/// Whether `tool` is one of ARIA's own MCP Apps tools.
///
/// # Why this one is answered without asking
///
/// Settled with the user on 2026-10-29, and bounded on purpose: **our own MCP
/// Apps tools only**. Showing a tile has no effect outside this process — it
/// draws in a window the user is already looking at, from a document bound by
/// [`DEFAULT_CSP`], on an origin that cannot reach the network. There is
/// nothing for a confirmation to protect.
///
/// It is also not a choice between asking and not asking, in practice. The
/// pattern registry matches **command lines**, and this tool takes arguments
/// rather than a shell line, so [`crate::permissions::registry::Verdict::unknown`]
/// is all it could ever say: every single tile would be a prompt, on a turn
/// where the user is waiting to be shown something.
///
/// What it does **not** cover: anything a tile asks to run. That goes through
/// [`may_call_from_tile`], which is a closed door.
///
/// `refresh_tile` is the same act on a panel already drawn, and since
/// 2026-11-23 it is answered the same way. Before, every refresh was an
/// approval card — so a session asked to add a map to its tile drew a second
/// tile instead, which needs no approval, and the user got two panels.
pub fn is_own_app_tool(naming: crate::providers::McpToolNaming, tool: &str) -> bool {
    naming.matches(tool, crate::mcp::SERVER_NAME, SHOW_TILE)
        || naming.matches(tool, crate::mcp::SERVER_NAME, REFRESH_TILE)
}

/// Whether a tile may ask the operating system to open this link.
///
/// # Only `http` and `https`, and that is not ceremony
///
/// Handing a string to the shell's "open this" is handing it to **every
/// protocol handler the machine has registered**. `file://` opens a document
/// or a folder; `javascript:` is script in whatever opens it; and Windows
/// carries dozens of application handlers — `ms-settings:`, `steam:`,
/// `vscode:` — several of which take parameters. A tile is HTML written by a
/// model that may have read something hostile on the way, so the set it may
/// reach is the one the web uses and nothing else.
///
/// What this deliberately does **not** do is judge the destination. SEP-1865
/// has `ui/open-link` precisely so a view can send the user somewhere, and a
/// list of allowed sites would be a list nobody can keep. The containment is
/// the scheme, plus the fact that the link opens in the user's own browser —
/// outside ARIA, where it can do nothing to ARIA.
pub fn openable(url: &str) -> bool {
    let url = url.trim();
    // Case-insensitive, because `JavaScript:` is the same handler and a plain
    // `starts_with` would wave it through.
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return false;
    }
    // A control character or a space would be read by whatever parses the
    // command line on the other side, and `start "" "<url>"` on Windows is
    // exactly such a parser.
    !url.chars().any(|c| c.is_control() || c == '"')
        && url.len() > "https://".len()
        && url.len() <= 2048
}

/// Which tools a **tile** may call, as SEP-1865's `app` visibility.
///
/// # An empty list, and that is the implementation and not a stub
///
/// The spec puts the boundary here: "the host MUST reject a `tools/call` from a
/// view for a tool that does not carry `app` visibility". Decision 10 says the
/// same thing in the negative — the boundary is *not* the `sandbox` attribute.
///
/// ARIA has no tool today that a tile has any business calling. So the answer
/// is no, for everything, and the door opens one tool at a time by a decision
/// written down. A filter that defaulted to allowing would be the hole the
/// spike was written to prove shut: it checked that a `tools/call` for a
/// `["model"]`-only tool is refused **by the host process**, and that is this
/// function.
pub fn may_call_from_tile(_tool: &str) -> bool {
    false
}

/// The palette, read from the one place it is defined.
///
/// `ui/src/styles/tokens.css` is the window's own `:root`, imported verbatim
/// from the mock-up. Reading it at compile time is what stops a tile from
/// slowly becoming a second theme.
const TOKENS: &str = include_str!("../../../../ui/src/styles/tokens.css");

/// The look of a tile's contents, and the vocabulary the tool announces.
///
/// `ui/src/styles/tile.css`. It lives under `ui/` although the window never
/// loads it, because the rule in this repository is that the design is edited
/// there — and because a class added here has to be added to the tool's
/// description in the same move, or it is a style nothing will ever use.
const TILE_CSS: &str = include_str!("../../../../ui/src/styles/tile.css");

/// The origins a tile document accepts being framed by: the window's, and
/// nothing else.
///
/// # Why three, and why they are written out
///
/// The window has a different origin depending on how ARIA is run, and all
/// three are real:
///
/// | | |
/// |---|---|
/// | `http://localhost:5173` | development — the page comes from Vite, named by `devUrl` |
/// | `http://tauri.localhost` | a built application on Windows |
/// | `tauri://localhost` | a built application elsewhere |
///
/// A tile is served from `http://127.0.0.1:7420`, which is **not** in this
/// list — so one tile cannot frame another, which is what
/// `object-src 'self'` and `frame-src 'self'` would otherwise allow. Both are
/// this one origin, so the policy cannot tell a PDF from a neighbour's
/// document; this list can tell the *window* from both.
///
/// The dev entry has to agree with `app/tauri.conf.json`'s `devUrl`, and a
/// test in `app` reads that file and checks it rather than trusting the
/// comment.
pub const HOST_ORIGINS: &[&str] = &[
    "http://localhost:5173",
    "http://tauri.localhost",
    "tauri://localhost",
];

/// The one line the wrapper adds to every tile: *I am here*.
///
/// # Why the window needs to be told rather than to assume
///
/// « The port is bound » and « you can reach it » are two different claims,
/// and 2026-11-03 is the day they came apart: the core reported the sandbox
/// up, every tile showed the browser's own « 127.0.0.1 a refusé de se
/// connecter », and the window had no way to know which of the two was
/// lying — a cross-origin frame fires `load` for an error page exactly as it
/// does for a document, so watching for `load` proves nothing.
///
/// A message from **inside** the document proves it: it can only be sent by
/// a document that was actually served, parsed and run. Silence after a
/// couple of seconds is the other case, and the window says so in the panel
/// instead of leaving the browser to speak for it.
///
/// Deliberately not JSON-RPC: it is a fact about this frame, not a call, and
/// giving it an id would invite someone to answer it.
const ARRIVED: &str = "<script>try{parent.postMessage(\
    {aria:\"tile.arrived\"},\"*\")}catch(e){}</script>";

/// The few characters that must not reach an attribute or a tag.
///
/// Only used for the `<title>`, which is the one place a caller's string lands
/// outside the body. The body itself is **not** escaped — it is HTML on
/// purpose, and what contains it is the sandbox origin plus [`DEFAULT_CSP`],
/// not an attempt to sanitise a language model's markup.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A file a session hands over for the tile to show.
///
/// # Served, not copied
///
/// The path stays the session's. ARIA serves a view of the file where it lies,
/// so a PDF the session rewrites is the PDF the tile shows on its next load —
/// which is what makes [`Registry::refresh`] a one-line operation rather than
/// a synchronisation problem. Nothing is duplicated into the registry, and
/// nothing has to be cleaned up when a tile goes.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub struct TileFile {
    /// How the tile refers to it: `src="f/<name>"`.
    pub name: String,
    /// Where it is on disk.
    pub path: String,
}

/// What a name may be before it is a path component.
///
/// The name lands in a URL **and** is matched against the declared list, so
/// the containment is that match: a request for a name nobody declared is a
/// 404, whatever it contains. This is the second line, for the names
/// themselves — a separator or a `..` in one would make a URL nobody can
/// reason about even though it resolves to a declared file.
pub fn nameable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 120
        && name != "."
        && name != ".."
        && !name.contains("..")
        && !name.contains(['/', '\\', '?', '#', '%', ':'])
        && !name.chars().any(char::is_control)
}

/// How many files one tile may carry.
pub const MAX_FILES: usize = 8;

/// The content type for a name, by extension.
///
/// A short table rather than a crate: what a tile can usefully show is what a
/// browser renders by itself, and that is a dozen types. Anything else is
/// served as a download rather than guessed at — `nosniff` is on every
/// response, so a wrong guess would simply not render, silently.
pub fn mime_of(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or_default().to_lowercase();
    match ext.as_str() {
        "mp3" => "audio/mpeg",
        "m4a" | "aac" => "audio/mp4",
        "wav" => "audio/wav",
        "ogg" | "oga" => "audio/ogg",
        "flac" => "audio/flac",
        "mp4" | "m4v" => "video/mp4",
        "webm" => "video/webm",
        "ogv" => "video/ogg",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "txt" | "log" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// A form a tile asks for — **declared**, never drawn by the tile.
///
/// # Why the session describes it instead of writing it
///
/// Settled with the user on 2026-11-01, and the reason is not tidiness.
///
/// The host **cannot tell a form the user filled in from a tile's own script
/// calling the tool**. A tile is HTML written by a model that may have read
/// something hostile on the way, so anything it reports is, at best, "the
/// tile says X" — never "the user said X". A tile that drew its own form and
/// submitted it would therefore let a model put words in the user's mouth.
///
/// So the tile describes the fields and **the window draws them, outside the
/// sandboxed frame**. Three things follow, and the first is the one that
/// mattered:
///
/// - the input is genuinely the user's: the tile's script cannot read it, nor
///   submit it, because it is not in its document;
/// - it looks like the rest of ARIA, which is decision 64 applied again;
/// - the model writes no HTML for it at all.
///
/// The price is a small vocabulary instead of a free hand. A tile cannot
/// invent an exotic control — and that is the trade, not an oversight.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub struct Form {
    /// What the button says. « Envoyer » when empty.
    #[serde(default)]
    pub submit: String,
    pub fields: Vec<Field>,
}

/// One control. Four kinds, and no more until one is missed in practice.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
pub struct Field {
    /// The key this field's answer is reported under.
    pub name: String,
    /// What is written next to it.
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub kind: FieldKind,
    /// The choices, for [`FieldKind::Choice`]. Ignored otherwise.
    #[serde(default)]
    pub options: Vec<String>,
    /// What it starts with.
    #[serde(default)]
    pub value: String,
}

#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Deserialize,
    serde::Serialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum FieldKind {
    #[default]
    Text,
    /// A number. Still carried as text: what the session receives is what the
    /// user typed, and a silent coercion would be a second opinion about it.
    Number,
    /// One of [`Field::options`].
    Choice,
    /// On or off. Reported as `oui` or `non`, because the session reads French.
    Toggle,
}

/// How many fields a tile may ask for at once.
///
/// Six. A tile is a small panel the user can resize down to 210 px, and a
/// questionnaire in it is a questionnaire nobody fills in. A session that
/// needs more than six answers should ask out loud.
pub const MAX_FIELDS: usize = 6;

impl Form {
    /// What is wrong with this form, if anything.
    ///
    /// Refused rather than repaired, for the same reason as the design guard:
    /// a form quietly reshaped is a form whose author believes something else
    /// is on screen.
    pub fn refused(&self) -> Option<String> {
        if self.fields.is_empty() {
            return Some("Un formulaire sans champ ne sert à rien.".into());
        }
        if self.fields.len() > MAX_FIELDS {
            return Some(format!(
                "Au plus {MAX_FIELDS} champs par tuile : une tuile est un petit panneau, \
                 pas un questionnaire. Pose le reste à voix haute."
            ));
        }
        let mut seen = Vec::new();
        for field in &self.fields {
            let name = field.name.trim();
            if name.is_empty() {
                return Some("Chaque champ a besoin d'un `name`.".into());
            }
            if seen.contains(&name) {
                return Some(format!(
                    "Deux champs portent le nom « {name} » : la réponse serait ambiguë."
                ));
            }
            seen.push(name);
            if field.kind == FieldKind::Choice && field.options.len() < 2 {
                return Some(format!(
                    "Le champ « {name} » est un choix : donne-lui au moins deux `options`."
                ));
            }
        }
        None
    }

    /// The button's label, defaulted.
    pub fn button(&self) -> &str {
        let label = self.submit.trim();
        if label.is_empty() { "Envoyer" } else { label }
    }

    /// What the session reads when the form comes back.
    ///
    /// # Why it says where it came from
    ///
    /// The session asked for these fields minutes or hours ago, in a turn it
    /// may no longer have in front of it — and after a resume it certainly
    /// does not. So the message names the tile and the question rather than
    /// handing over bare values, which would read as a non-sequitur.
    ///
    /// Only the declared fields are reported, in the order they were
    /// declared: the window sends back whatever it has, and a key nobody
    /// asked for is a key the session cannot interpret. Dropping it here is
    /// cheaper than teaching every reader to ignore it.
    pub fn report(&self, title: &str, answers: &HashMap<String, String>) -> String {
        let mut out = format!("L'utilisateur a rempli le formulaire de la tuile « {title} » :\n");
        for field in &self.fields {
            let label = if field.label.trim().is_empty() {
                field.name.as_str()
            } else {
                field.label.trim()
            };
            let given = answers.get(&field.name).map(String::as_str).unwrap_or("");
            let given = if given.trim().is_empty() {
                "(laissé vide)"
            } else {
                given.trim()
            };
            out.push_str(&format!("- {label} : {given}\n"));
        }
        out.push_str(
            "Réponds à partir de ces valeurs. Si une tuile doit être mise à jour, \
             appelle `show_tile` à nouveau.",
        );
        out
    }
}

/// One tile, as the registry holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct Tile {
    /// The `ui://` resource, as MCP Apps names it. Also the key.
    pub uri: String,
    /// What the window writes on its title bar.
    pub title: String,
    /// What the session sent: a **fragment**, not a document.
    ///
    /// Semantic HTML with no styling of its own — the look is applied by the
    /// core when the tile is served. See [`Tile::document`].
    pub body: String,
    /// Files the tile may show, by the name it refers to them with.
    pub files: Vec<TileFile>,
    /// The form to draw under it, if the session asked for one.
    ///
    /// Drawn by the **window**, never by the tile — see [`Form`].
    pub form: Option<Form>,
    /// Where it came from, for the line the window shows under it, and where
    /// a submitted form goes back to.
    pub session: crate::sessions::SessionId,
}

impl Tile {
    /// The path this tile is served at, relative to the sandbox origin.
    ///
    /// A `ui://` URI is not a path, so it is not used as one: the id in the
    /// path is the registry's own, and the URI stays an identifier. Letting a
    /// caller-chosen string reach the path would be a traversal waiting to
    /// happen.
    pub fn path(&self) -> String {
        format!("/t/{}", self.id())
    }

    /// The file this tile declared under `name`, if it declared one.
    ///
    /// **The declaration is the containment.** A request names a file, and
    /// only a name the session listed resolves — so no amount of `..` in a
    /// URL reaches a path nobody offered. That is why the path itself is
    /// never parsed out of the request.
    pub fn file(&self, name: &str) -> Option<&TileFile> {
        self.files.iter().find(|held| held.name == name)
    }

    /// Where the window points its iframe.
    ///
    /// Built here and published on [`crate::events::Event::TileReady`], so the
    /// window never assembles it: "a `ui://` URI is not a path" is a rule that
    /// holds only while it lives in one place.
    pub fn source(&self) -> String {
        format!("{}{}", origin(), self.path())
    }

    /// The PDFs this tile declared, as URLs the **window** frames.
    ///
    /// # Why the window, and why without `sandbox`
    ///
    /// Measured on 2026-11-16 against Chrome, which is what WebView2 is:
    /// « Failed to load '…/doc.pdf' as a plugin, because the frame into which
    /// the plugin is loading is sandboxed. » Every tile is a sandboxed frame,
    /// so an `<embed>` of a PDF inside one is blank — always, whatever the
    /// CSP says. Decision 73 had made `frame-src 'self'` right and never seen
    /// a PDF, because headless Chrome does not paint them.
    ///
    /// So the window draws a second frame **beside** the tile, without the
    /// attribute — the same move as the declared form (decision 69). What
    /// makes it safe to leave unsandboxed is this URL and nothing the tile
    /// writes: it names a file the session declared, ending in `.pdf`, served
    /// as `application/pdf` with `nosniff`. It can only ever be a PDF, and the
    /// viewer is Chromium's own, on an origin that is not the window's.
    pub fn documents(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|file| mime_of(&file.name) == "application/pdf")
            .map(|file| format!("{}/f/{}", self.source(), file.name))
            .collect()
    }

    /// The page actually served: ARIA's look, wrapped around what was sent.
    ///
    /// # Why the core dresses the tile and the session does not
    ///
    /// A model asked for a styled document produces a styled document, and it
    /// is never quite this interface — a different amber, a different radius,
    /// a font that is not the one the rest of the window uses. One tile like
    /// that is a blemish; three are two products in one window.
    ///
    /// So the tool takes **semantic HTML with no styling**, and everything
    /// below is applied over it. The model spends no tokens on CSS, gets none
    /// of it wrong, and a change of look is one file rather than a retrained
    /// habit.
    ///
    /// # Why the stylesheet is embedded rather than linked
    ///
    /// The tile is served from a **separate origin**, which is the whole point
    /// of decision 10 — so it cannot reach the window's stylesheet, and the
    /// policy in [`DEFAULT_CSP`] would refuse the request anyway. Embedding is
    /// the only way to share the design across that boundary.
    ///
    /// The two files are [`include_str!`]d from `ui/src/styles/`, so there is
    /// **one** palette in the repository and not a copy that drifts. If either
    /// moves, this stops compiling, which is the failure we want: the other
    /// one is a tile that quietly stops matching.
    pub fn document(&self) -> String {
        format!(
            "<!doctype html>\n<html lang=\"fr\"><head><meta charset=\"utf-8\">\n\
             <base href=\"{}/\">\n\
             <title>{}</title>\n<style>\n{TOKENS}\n{TILE_CSS}\n</style>\n\
             </head><body>\n{}\n{ARRIVED}\n</body></html>",
            self.path(),
            escape(&self.title),
            self.body
        )
    }

    fn id(&self) -> &str {
        self.uri.rsplit('/').next().unwrap_or_default()
    }
}

/// Every tile this run of the application has produced.
#[derive(Default)]
pub struct Registry {
    held: Mutex<HashMap<String, Tile>>,
    next: Mutex<u64>,
    /// Whether the origin that serves these is up. See [`Registry::serving`].
    served: std::sync::atomic::AtomicBool,
}

impl Registry {
    /// Register one tile and give back the `ui://` URI that names it.
    ///
    /// The URI is minted here rather than taken from the caller, because the
    /// caller is a language model: a URI it chose could collide with a live
    /// tile, or carry a path separator, or simply be a string it made up twice.
    pub fn add(
        &self,
        session: crate::sessions::SessionId,
        title: impl Into<String>,
        body: impl Into<String>,
        files: Vec<TileFile>,
        form: Option<Form>,
    ) -> Tile {
        let id = {
            let mut next = self.next.lock().expect("tile counter poisoned");
            *next += 1;
            *next
        };
        let tile = Tile {
            uri: format!("ui://aria/tile/{id}"),
            title: title.into(),
            body: body.into(),
            files,
            form,
            session,
        };
        self.held
            .lock()
            .expect("tiles poisoned")
            .insert(tile.uri.clone(), tile.clone());
        tile
    }

    /// `resources/read`, as MCP Apps spells it.
    pub fn get(&self, uri: &str) -> Option<Tile> {
        self.held.lock().expect("tiles poisoned").get(uri).cloned()
    }

    /// The tile served at `/t/{id}`.
    fn by_id(&self, id: &str) -> Option<Tile> {
        self.get(&format!("ui://aria/tile/{id}"))
    }

    /// Replace a tile's contents in place, keeping its URI.
    ///
    /// # Why a tile is refreshed rather than replaced
    ///
    /// A session that reworks a document should not make a second tile appear
    /// next to the first: the user is looking at one panel, placed where they
    /// put it and sized how they sized it. Keeping the URI keeps all of that,
    /// and the window only reloads the frame.
    ///
    /// **A file needs no update at all.** The registry serves files from the
    /// session's own paths, so a PDF rewritten on disk is already the PDF the
    /// tile will show — the refresh is only there to tell the window to look
    /// again. `body` is for when the markup changed too.
    pub fn refresh(&self, uri: &str, body: Option<String>) -> bool {
        let mut held = self.held.lock().expect("tiles poisoned");
        match held.get_mut(uri) {
            Some(tile) => {
                if let Some(body) = body {
                    tile.body = body;
                }
                true
            }
            None => false,
        }
    }

    /// Point a tile at the session that actually answered it.
    ///
    /// A resume gives a **new** session id — new process, new activity feed —
    /// and the tile has to follow, or every later submission starts another
    /// one. See `Core::deliver_to_session`.
    pub fn reattach(&self, uri: &str, session: crate::sessions::SessionId) -> bool {
        let mut held = self.held.lock().expect("tiles poisoned");
        match held.get_mut(uri) {
            Some(tile) => {
                tile.session = session;
                true
            }
            None => false,
        }
    }

    /// The tiles a session put on screen and the user has not closed, as
    /// `(uri, title)`, oldest first.
    pub fn shown_by(&self, session: crate::sessions::SessionId) -> Vec<(String, String)> {
        let held = self.held.lock().expect("tiles poisoned");
        let mut shown: Vec<_> = held
            .values()
            .filter(|tile| tile.session == session)
            .map(|tile| (tile.uri.clone(), tile.title.clone()))
            .collect();
        // The URI ends in a counter, so its numeric order is creation order.
        shown.sort_by_key(|(uri, _)| {
            uri.rsplit('/').next().and_then(|id| id.parse::<u64>().ok()).unwrap_or(0)
        });
        shown
    }

    /// Move every tile of `from` to `to` — a resume, which is a new session id
    /// for the same conversation. See [`Registry::reattach`].
    pub fn follow(&self, from: crate::sessions::SessionId, to: crate::sessions::SessionId) {
        for tile in self.held.lock().expect("tiles poisoned").values_mut() {
            if tile.session == from {
                tile.session = to;
            }
        }
    }

    /// Forget one. The window closing a tile is the only caller: nothing else
    /// in the product removes one, because nothing else owns it.
    pub fn remove(&self, uri: &str) -> bool {
        self.held
            .lock()
            .expect("tiles poisoned")
            .remove(uri)
            .is_some()
    }

    /// Whether the second origin is actually up.
    ///
    /// # Why the window has to be told, and not left to find out
    ///
    /// Seen on 2026-11-03: every tile showed the browser's own « 127.0.0.1 a
    /// refusé de se connecter » page inside its panel. That reads as *the
    /// tiles are broken*, and sends you into the tile code — when the truth
    /// was simply that nothing was listening on the port.
    ///
    /// The window cannot check for itself: its own CSP allows `connect-src`
    /// to the event stream and nothing else, so it may not probe `/health`.
    /// So the core says, and [`crate::Snapshot`] carries it.
    pub fn serving(&self) -> bool {
        self.served.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Called by whoever bound the port, once, with the outcome.
    pub fn set_serving(&self, up: bool) {
        self.served.store(up, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn count(&self) -> usize {
        self.held.lock().expect("tiles poisoned").len()
    }
}

#[derive(Clone)]
struct Sandbox {
    tiles: Arc<Registry>,
}

/// Bind [`TILES_PORT`], synchronously, before anything is spawned.
///
/// Same shape as [`crate::transport::bind_events`] and for the same reason: a
/// caller can make the origin reachable and only then start what will load
/// from it. It hands back a standard listener, because converting to a `tokio`
/// one needs a running reactor and this is callable from outside the runtime.
pub fn bind() -> anyhow::Result<std::net::TcpListener> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], TILES_PORT));
    let listener = std::net::TcpListener::bind(addr).with_context(|| {
        format!(
            "le port {TILES_PORT} est pris — les tuiles ne peuvent pas être servies, \
             et la CSP de la fenêtre nomme ce port, donc en changer en silence \
             afficherait des tuiles vides"
        )
    })?;
    tracing::info!("bac à sable des tuiles sur http://{addr}/");
    Ok(listener)
}

/// Serve the sandbox origin until the process ends.
pub async fn serve(tiles: Arc<Registry>) -> anyhow::Result<()> {
    serve_on(bind()?, tiles).await
}

/// The same server on a listener the caller already has, so a test can bind
/// port 0 and be given whatever the OS has free.
pub async fn serve_on(listener: std::net::TcpListener, tiles: Arc<Registry>) -> anyhow::Result<()> {
    listener.set_nonblocking(true)?;
    let listener = tokio::net::TcpListener::from_std(listener)?;
    let app = Router::new()
        .route("/t/{id}", get(tile))
        .route("/t/{id}/f/{name}", get(tile_file))
        .route("/health", get(health))
        .with_state(Sandbox { tiles });
    axum::serve(listener, app)
        .await
        .context("le bac à sable des tuiles s'est arrêté")
}

async fn health() -> &'static str {
    "ok"
}

/// One tile, with its own CSP on the response.
///
/// The CSP travels as a **header** and not only as a `<meta>`, because the
/// document is the model's HTML: a tile that forgot its meta tag, or wrote a
/// laxer one, must still be bound by ours. A header cannot be relaxed from
/// inside the document.
///
/// No `Access-Control-Allow-Origin`, on purpose. A tile talks to the host
/// through `postMessage`, never through a cross-origin fetch, so there is
/// nothing to allow and allowing it would hand the window's origin a reader.
async fn tile(State(sandbox): State<Sandbox>, Path(id): Path<String>) -> Response {
    // Every knock is logged, because the question « did the window even
    // reach us? » had no answer on 2026-11-03 and cost two rounds of
    // guessing. One line per tile load is nothing.
    tracing::debug!("bac à sable : document demandé pour la tuile {id}");
    let Some(tile) = sandbox.tiles.by_id(&id) else {
        return (StatusCode::NOT_FOUND, "aucune tuile").into_response();
    };
    let mut response = axum::response::Html(tile.document()).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(DEFAULT_CSP),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // A tile may be framed by the **window**, and by nothing else.
    //
    // This was `X-Frame-Options: DENY` from 2026-11-01 to 2026-11-03, and it
    // was wrong in the most expensive way: `DENY` refuses *everyone*,
    // including the one frame that has to work. Every tile came up empty in
    // the application while every test passed, because the tests loaded a
    // tile as a **top-level page** — the one context the header does not
    // govern. Two rounds of diagnosis went looking at ports.
    //
    // `frame-ancestors` is the directive that can say "this window, not that
    // tile": it takes a source list where `X-Frame-Options` takes a verb.
    // `SAMEORIGIN` would have been worse than useless — it allows exactly the
    // tile-in-tile case this is for, and forbids the window, which is a
    // different origin.
    if let Ok(value) = HeaderValue::from_str(&format!(
        "{DEFAULT_CSP}; frame-ancestors {}",
        HOST_ORIGINS.join(" ")
    )) {
        headers.insert(header::CONTENT_SECURITY_POLICY, value);
    }
    response
}

/// One of a tile's declared files, with ranges so media can be seeked.
///
/// # Why ranges are not optional
///
/// A `<video>` is not a download: the browser asks for the last bytes to find
/// the index, then jumps. Against a server that answers 200 with the whole
/// body every time, playback starts and the slider does nothing — the failure
/// looks like a broken control rather than a missing header, which is exactly
/// the kind of symptom that sends you looking in the wrong place.
///
/// Deliberately a **single** range, which is all a media element asks for.
/// Multipart ranges exist and nothing here needs them.
async fn tile_file(
    State(sandbox): State<Sandbox>,
    Path((id, name)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Response {
    let Some(tile) = sandbox.tiles.by_id(&id) else {
        return (StatusCode::NOT_FOUND, "aucune tuile").into_response();
    };
    // Resolved against what the session declared, never against the request.
    let Some(file) = tile.file(&name) else {
        return (StatusCode::NOT_FOUND, "fichier non déclaré").into_response();
    };

    let Ok(bytes) = std::fs::read(&file.path) else {
        // The session owns the path and may have moved or deleted it since.
        // Said as a 404 rather than a 500: nothing is broken here.
        tracing::warn!("tuile {id} : {} est illisible", file.path);
        return (StatusCode::NOT_FOUND, "fichier illisible").into_response();
    };

    let mime = mime_of(&name);
    let total = bytes.len() as u64;
    let asked = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| one_range(value, total));

    let mut response = match asked {
        Some((from, to)) => {
            let body = bytes[from as usize..=to as usize].to_vec();
            let mut partial = (StatusCode::PARTIAL_CONTENT, body).into_response();
            if let Ok(value) = HeaderValue::from_str(&format!("bytes {from}-{to}/{total}")) {
                partial.headers_mut().insert(header::CONTENT_RANGE, value);
            }
            partial
        }
        None => bytes.into_response(),
    };

    let out = response.headers_mut();
    if let Ok(value) = HeaderValue::from_str(mime) {
        out.insert(header::CONTENT_TYPE, value);
    }
    // Says ranges are understood. Without it a media element will not even
    // try to seek.
    out.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    // The session may rewrite the file under us — that is the whole of
    // `Registry::refresh` — so nothing is kept.
    out.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    out.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// `bytes=from-to`, clamped to what exists. `None` when it cannot be honoured.
///
/// Open-ended (`bytes=500-`) and suffix (`bytes=-500`) forms both appear in
/// the wild — Chromium opens a video with `bytes=0-` and seeks with a suffix.
fn one_range(header: &str, total: u64) -> Option<(u64, u64)> {
    if total == 0 {
        return None;
    }
    let spec = header.strip_prefix("bytes=")?.trim();
    // One range only. A comma means several, which nothing here serves.
    if spec.contains(',') {
        return None;
    }
    let (start, end) = spec.split_once('-')?;
    let (from, to) = match (start.trim(), end.trim()) {
        ("", last) => {
            // The last `last` bytes.
            let want: u64 = last.parse().ok()?;
            (total.saturating_sub(want.max(1)), total - 1)
        }
        (first, "") => (first.parse().ok()?, total - 1),
        (first, last) => (
            first.parse().ok()?,
            last.parse::<u64>().ok()?.min(total - 1),
        ),
    };
    (from <= to && from < total).then_some((from, to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::SessionId;

    fn a_session() -> SessionId {
        SessionId::new()
    }

    #[test]
    fn a_tile_is_named_by_the_registry_and_not_by_its_author() {
        let registry = Registry::default();
        let first = registry.add(
            a_session(),
            "Météo",
            "<!doctype html><p>1",
            Vec::new(),
            None,
        );
        let second = registry.add(
            a_session(),
            "Météo",
            "<!doctype html><p>2",
            Vec::new(),
            None,
        );
        assert_ne!(first.uri, second.uri, "two tiles, two URIs");
        assert!(first.uri.starts_with("ui://aria/tile/"));
    }

    /// The path a tile is served at is built from the registry's own id, so
    /// nothing a model wrote can reach it.
    #[test]
    fn the_served_path_carries_no_author_supplied_text() {
        let registry = Registry::default();
        let tile = registry.add(
            a_session(),
            "../../etc/passwd",
            "<!doctype html>",
            Vec::new(),
            None,
        );
        assert_eq!(tile.path(), "/t/1");
    }

    #[test]
    fn a_registered_tile_is_readable_by_its_uri() {
        let registry = Registry::default();
        let tile = registry.add(
            a_session(),
            "Météo",
            "<!doctype html><p>23 degrés",
            Vec::new(),
            None,
        );
        assert_eq!(registry.get(&tile.uri), Some(tile.clone()));
        assert_eq!(registry.by_id("1"), Some(tile));
        assert!(registry.get("ui://aria/tile/404").is_none());
    }

    #[test]
    fn closing_a_tile_forgets_it() {
        let registry = Registry::default();
        let tile = registry.add(a_session(), "Météo", "<!doctype html>", Vec::new(), None);
        assert_eq!(registry.count(), 1);
        assert!(registry.remove(&tile.uri));
        assert_eq!(registry.count(), 0);
        assert!(
            !registry.remove(&tile.uri),
            "removing twice is not an error"
        );
    }

    /// Decision 11 in the one place it is mechanical: there is nowhere for a
    /// tile to be read back from, so nothing survives the process.
    #[test]
    fn a_fresh_registry_holds_nothing() {
        assert_eq!(Registry::default().count(), 0);
    }

    /// A follow-up is told the tiles its session still has on screen, and a
    /// resume carries them over: otherwise « ajoute la carte » draws a second
    /// panel next to the first.
    #[test]
    fn a_session_s_tiles_are_listed_in_order_and_follow_a_resume() {
        let tiles = Registry::default();
        let (first, other, resumed) = (
            crate::sessions::SessionId::new(),
            crate::sessions::SessionId::new(),
            crate::sessions::SessionId::new(),
        );
        let a = tiles.add(first, "Trajet", "<p>5 h</p>", vec![], None).uri;
        tiles.add(other, "Météo", "<p>soleil</p>", vec![], None);
        let b = tiles.add(first, "", "<p>péages</p>", vec![], None).uri;

        let shown = tiles.shown_by(first);
        assert_eq!(shown, vec![(a.clone(), "Trajet".into()), (b.clone(), String::new())]);

        tiles.follow(first, resumed);
        assert!(tiles.shown_by(first).is_empty());
        assert_eq!(tiles.shown_by(resumed).len(), 2);
        assert_eq!(tiles.shown_by(other).len(), 1, "une autre session garde les siennes");

        tiles.remove(&a);
        assert_eq!(tiles.shown_by(resumed), vec![(b, String::new())]);
    }

    #[test]
    fn our_own_tile_tool_is_the_only_thing_answered_without_asking() {
        assert!(is_own_app_tool(crate::providers::McpToolNaming::DoubleUnderscore, "mcp__aria__show_tile"));
        assert!(is_own_app_tool(crate::providers::McpToolNaming::DoubleUnderscore, SHOW_TILE));
        assert!(is_own_app_tool(crate::providers::McpToolNaming::DoubleUnderscore, "mcp__aria__refresh_tile"));
        // Everything else keeps its confirmation, including the tools whose
        // names look adjacent.
        for other in [
            "mcp__aria__approve",
            "mcp__aria__delegate_task",
            "mcp__aria__run_action",
            "Bash",
            "show_tile_but_not_really",
        ] {
            assert!(
                !is_own_app_tool(crate::providers::McpToolNaming::DoubleUnderscore, other),
                "{other} ne doit pas être auto-approuvé"
            );
        }
    }

    /// `ui/open-link` hands a string to the machine's protocol handlers, so
    /// the set it may reach is the web's and nothing else.
    #[test]
    fn only_a_web_link_can_be_opened() {
        for good in [
            "https://www.netflix.com/",
            "http://localhost:3000/rapport",
            "https://example.org/a?b=c#d",
        ] {
            assert!(openable(good), "refusé à tort : {good}");
        }
        for bad in [
            "file:///C:/Users/someone/Documents",
            "javascript:alert(1)",
            // The same handler, spelled to slip past a `starts_with`.
            "JavaScript:alert(1)",
            "ms-settings:privacy",
            "steam://run/440",
            "vscode://file/C:/secrets",
            "data:text/html,<script>",
            "https://",
            "",
            // A quote ends the argument in `start \"\" \"<url>\"`.
            "https://ok.test/\" && calc",
            "https://ok.test/\nmalin",
        ] {
            assert!(!openable(bad), "accepté à tort : {bad}");
        }
    }

    fn a_field(name: &str, kind: FieldKind) -> Field {
        Field {
            name: name.into(),
            label: String::new(),
            kind,
            options: Vec::new(),
            value: String::new(),
        }
    }

    /// Refused rather than repaired, like the design guard — and each refusal
    /// has to say what to send instead, or the model retries the same thing.
    #[test]
    fn a_form_that_cannot_be_answered_is_refused_with_the_reason() {
        let empty = Form {
            submit: String::new(),
            fields: Vec::new(),
        };
        assert!(empty.refused().is_some());

        let too_many = Form {
            submit: String::new(),
            fields: (0..=MAX_FIELDS)
                .map(|n| a_field(&format!("f{n}"), FieldKind::Text))
                .collect(),
        };
        assert!(too_many.refused().expect("refusé").contains("voix haute"));

        let nameless = Form {
            submit: String::new(),
            fields: vec![a_field(" ", FieldKind::Text)],
        };
        assert!(nameless.refused().expect("refusé").contains("name"));

        // Two fields under one name: the answer would be ambiguous, and the
        // session would read whichever the window happened to write last.
        let twice = Form {
            submit: String::new(),
            fields: vec![
                a_field("ville", FieldKind::Text),
                a_field("ville", FieldKind::Text),
            ],
        };
        assert!(twice.refused().expect("refusé").contains("ville"));

        // A choice of one is not a choice.
        let mut lonely = a_field("jours", FieldKind::Choice);
        lonely.options = vec!["demain".into()];
        let thin = Form {
            submit: String::new(),
            fields: vec![lonely],
        };
        assert!(thin.refused().expect("refusé").contains("options"));
    }

    #[test]
    fn a_usable_form_passes_and_its_button_has_a_label() {
        let mut choice = a_field("jours", FieldKind::Choice);
        choice.options = vec!["demain".into(), "la semaine".into()];
        let form = Form {
            submit: String::new(),
            fields: vec![a_field("ville", FieldKind::Text), choice],
        };
        assert_eq!(form.refused(), None);
        assert_eq!(form.button(), "Envoyer");
    }

    /// What the session reads, and the two things it must contain: where the
    /// answers came from, and only the fields that were asked for.
    #[test]
    fn only_the_declared_fields_reach_the_session() {
        let mut field = a_field("ville", FieldKind::Text);
        field.label = "Ville".into();
        let form = Form {
            submit: String::new(),
            fields: vec![field],
        };

        let mut answers = HashMap::new();
        answers.insert("ville".to_string(), "Saint-Just".to_string());
        // A key nobody asked for. The window should not send one; the core
        // does not rely on that.
        answers.insert("admin".to_string(), "true".to_string());

        let report = form.report("Météo · Lyon", &answers);
        assert!(report.contains("Météo · Lyon"), "{report}");
        assert!(report.contains("- Ville : Saint-Just"), "{report}");
        assert!(
            !report.contains("admin"),
            "un champ non déclaré a traversé : {report}"
        );
    }

    /// A field left alone is reported as empty rather than omitted: a session
    /// that asked three questions must see three answers, or it cannot tell a
    /// blank from a dropped key.
    #[test]
    fn an_untouched_field_is_reported_as_empty() {
        let form = Form {
            submit: String::new(),
            fields: vec![a_field("ville", FieldKind::Text)],
        };
        let report = form.report("Météo", &HashMap::new());
        assert!(report.contains("laissé vide"), "{report}");
    }

    /// Decision 10 and SEP-1865: the host refuses, and it refuses by default.
    #[test]
    fn a_tile_may_call_nothing_at_all_today() {
        for tool in [
            "mcp__aria__show_tile",
            "mcp__aria__delegate_task",
            "mcp__aria__approve",
            "Bash",
            "",
        ] {
            assert!(
                !may_call_from_tile(tool),
                "aucun outil ne porte encore la visibilité app : {tool}"
            );
        }
    }

    /// The floor SEP-1865 sets. Each clause here is load-bearing, so a laxer
    /// default would pass every other test in this file and open a hole.
    #[test]
    fn the_default_policy_is_the_restrictive_one() {
        for clause in [
            "default-src 'none'",
            // The one that matters most: a tile cannot reach the network, so
            // it cannot send anywhere what it was given.
            "connect-src 'none'",
            "base-uri 'self'",
        ] {
            assert!(DEFAULT_CSP.contains(clause), "manque : {clause}");
        }
        // No host may appear anywhere in it. A tile reaches its parent and
        // nothing else, so any origin written here would be a way out.
        assert!(!DEFAULT_CSP.contains("http"), "{DEFAULT_CSP}");
        // `'self'` is this sandbox origin only — never the window's, which is
        // served by Tauri on another one. Everything a tile may load is
        // therefore something a session declared.
        for own in [
            "media-src 'self'",
            "object-src 'self'",
            "img-src 'self'",
            // Le visionneur PDF est un contexte de navigation imbriqué :
            // mesuré le 2026-11-01, `object-src` ne suffit pas.
            "frame-src 'self'",
        ] {
            assert!(DEFAULT_CSP.contains(own), "manque : {own}");
        }
    }

    /// `object-src 'self'` buys the PDF viewer and would have cost tile
    /// isolation, so the cost is paid by a header instead.
    ///
    /// Both a PDF and a neighbouring tile's document live on this one origin,
    /// so the policy cannot tell them apart: an `<embed>` of `/t/2` would be
    /// same-origin and scriptable from `/t/1`. `X-Frame-Options: DENY` on the
    /// **document** — and not on the files — draws the line the policy cannot.
    ///
    /// The compensation itself is a **header**, so it is asserted on the wire
    /// — `tests/tiles.rs`, `a_tile_refuses_to_be_embedded_in_another`. What is
    /// checked here is only that the two were changed together: this test
    /// fails the day someone tightens the policy back and leaves the header,
    /// or the reverse.
    #[test]
    fn a_policy_that_allows_embedding_is_a_policy_that_needs_the_header() {
        assert!(
            DEFAULT_CSP.contains("object-src 'self'"),
            "si object-src redevient 'none', le visionneur PDF disparaît — et \
             l'en-tête X-Frame-Options des documents n'a plus de raison d'être"
        );
    }

    /// The point of the whole arrangement: what the session sent arrives
    /// wrapped in ARIA's look, and the session wrote none of it.
    #[test]
    fn what_is_served_is_the_fragment_dressed_by_the_core() {
        let registry = Registry::default();
        let tile = registry.add(
            a_session(),
            "Météo",
            "<p class=\"big\">12°</p>",
            Vec::new(),
            None,
        );
        let page = tile.document();

        assert!(page.starts_with("<!doctype html>"), "{page}");
        assert!(
            page.contains("<p class=\"big\">12°</p>"),
            "le fragment est servi tel quel"
        );
        // The palette, read from `ui/` at compile time. If this assertion ever
        // fails it is because the tokens moved, and a tile silently stopped
        // matching the window — which is the failure this whole file exists
        // to prevent.
        assert!(
            page.contains("--amber"),
            "la palette de la fenêtre est embarquée"
        );
        assert!(page.contains(".big"), "la feuille des tuiles aussi");
    }

    /// A title is the one caller-supplied string that lands outside the body.
    #[test]
    fn a_title_cannot_close_the_tag_it_sits_in() {
        let registry = Registry::default();
        let tile = registry.add(
            a_session(),
            "</title><script>alert(1)</script>",
            "<p>ok",
            Vec::new(),
            None,
        );
        assert!(
            !tile.document().contains("<script>alert"),
            "{}",
            tile.document()
        );
    }

    /// A tile is one document, so its script is inline or it does not exist.
    ///
    /// Asserted because the obvious tightening — `script-src 'self'` — is what
    /// was written first, and it makes the `postMessage` channel unusable
    /// without failing anything: the tile simply renders and never speaks.
    #[test]
    fn a_tile_can_run_its_own_inline_script_and_only_that() {
        assert!(DEFAULT_CSP.contains("script-src 'unsafe-inline'"));
        assert!(
            !DEFAULT_CSP.contains("script-src 'self'"),
            "il n'y a pas de second fichier à charger : voir l'en-tête"
        );
    }
}
