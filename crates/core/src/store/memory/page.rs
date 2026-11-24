//! A memory page: its header, its body, and the two things that make it
//! findable.
//!
//! Decision 88 gives a page four parts, and two of them exist only so it can be
//! found again: the `id`, which is also where the file sits, and `keywords`,
//! which the writing agent **declares** rather than leaves to be guessed.
//!
//! # The header is OKF, with one field dropped and one added
//!
//! The shape is the Open Knowledge Format (decision 88): `type` is the only
//! mandatory field, and `sources`, `generated`, `verified`, `status` and
//! `stale_after` are what let the core decide whether a fact may be **said out
//! loud** (decision 87) — a different question from whether it is retrievable.
//!
//! **Dropped: `id`.** The original specification writes it into the header, and we do not.
//! The id *is* the path (decision 93), so a header field would be a second
//! copy of it, and a second copy can disagree with the first — on a rename
//! (decision 98) it would have to be rewritten in lock-step for nothing.
//!
//! **Added: `aliases`.** Decision 98 lets a rewrite change the title and
//! therefore the id; the former ids live here so that an old `[[link]]`, and a
//! `remember` naming the page it saw a minute ago, still land on it.
//!
//! # Reading is tolerant, writing is ours
//!
//! The YAML is read by a library, because the user hand-edits these files and
//! any valid YAML may turn up. It is written by [`Page::to_markdown`], by hand,
//! so the key order never moves and a diff stays readable.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// Where a page is, and what links point at it — one string, both jobs.
///
/// A path without its extension: `profil/utilisateur`. Decision 93 settled it
/// that way so a `[[link]]` names the page **and** locates it, with no second
/// mapping to keep in step.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PageId(String);

/// Why a page could not be read, or an id not accepted.
#[derive(Debug, thiserror::Error)]
pub enum PageError {
    /// An id with a path component we will not put on a filesystem.
    #[error("{0:?} is not a usable page id")]
    BadId(String),
    /// No `---` header, or no closing fence.
    #[error("the page has no front matter between `---` fences")]
    NoFrontMatter,
    /// The header is not valid YAML.
    #[error("the front matter is not valid YAML: {0}")]
    BadYaml(String),
    /// OKF's one requirement.
    #[error("the front matter has no `type`, which OKF requires")]
    NoType,
}

impl PageId {
    /// Accept an id, or say why not.
    ///
    /// The rules are the confinement rules of decision 90 expressed one level
    /// up: lowercase ASCII, digits, `-`, `_`, and `/` as the only separator.
    /// Anything else never reaches the filesystem, so a path check is never the
    /// last line of defence.
    pub fn new(raw: impl Into<String>) -> Result<Self, PageError> {
        let raw = raw.into();
        let refused = || PageError::BadId(raw.clone());
        if raw.is_empty() || raw.starts_with('/') || raw.ends_with('/') {
            return Err(refused());
        }
        for segment in raw.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return Err(refused());
            }
            if !segment
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
            {
                return Err(refused());
            }
        }
        Ok(Self(raw))
    }

    /// Build an id from a folder and a title, the way a page is created.
    ///
    /// Decision 98 is what makes this matter: a rewrite may change the title,
    /// so the id a title produces has to be reproducible.
    pub fn from_title(folder: &str, title: &str) -> Result<Self, PageError> {
        let slug = slug(title);
        if slug.is_empty() {
            return Err(PageError::BadId(title.to_owned()));
        }
        if folder.is_empty() {
            Self::new(slug)
        } else {
            Self::new(format!("{}/{slug}", slug_path(folder)))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Where the page lives, relative to the memory root.
    pub fn file_name(&self) -> String {
        format!("{}.md", self.0)
    }

    /// The folder part, or `""` for a page at the top level.
    pub fn folder(&self) -> &str {
        match self.0.rfind('/') {
            Some(cut) => &self.0[..cut],
            None => "",
        }
    }
}

impl std::fmt::Display for PageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who produced or checked something: `human:<id>`, `process:<id>`, or
/// `<producer>/<version>` — OKF's actor forms, kept as written.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

/// Where a fact came from. `resource` is OKF's only required part.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// OKF's lifecycle. `stable` is the default, as the spec says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Draft,
    #[default]
    Stable,
    /// Superseded, and kept. This is what ARIA has instead of mem0's
    /// overwrite — the reason it is in the excluded list of decision 82.
    Deprecated,
}

/// The header of a page.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontMatter {
    /// OKF's one requirement. Free text on purpose: the original specification gives seven
    /// folders, and a vocabulary frozen in code would have to be migrated the
    /// first time one more is wanted.
    ///
    /// `default` although it is required, so that its absence is reported by
    /// [`PageError::NoType`] — "the front matter has no `type`" — rather than
    /// by the serialiser's "missing field `type`". The person who reads that
    /// message is holding a file they edited by hand.
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// **How the writer wants the page found.** Weighted above the body in the
    /// index, which is the whole reason it is a field and not prose.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// Former ids, kept by a rename (decision 98).
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub sources: Vec<Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated: Option<Actor>,
    #[serde(default)]
    pub verified: Vec<Actor>,
    #[serde(default)]
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_after: Option<String>,
}

/// A page, as it sits on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub id: PageId,
    pub front: FrontMatter,
    pub body: String,
}

impl Page {
    /// A new page with nothing in it but a reason to exist.
    pub fn new(id: PageId, kind: &str, title: &str) -> Self {
        Self {
            front: FrontMatter {
                kind: kind.to_owned(),
                title: title.to_owned(),
                generated: Some(Actor { by: "aria/voice".into(), at: None }),
                ..FrontMatter::default()
            },
            id,
            body: String::new(),
        }
    }

    /// Read a page. The id comes from where the file is, never from the header.
    pub fn parse(id: PageId, text: &str) -> Result<Self, PageError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let rest = text
            .strip_prefix("---\n")
            .or_else(|| text.strip_prefix("---\r\n"))
            .ok_or(PageError::NoFrontMatter)?;
        let (header, body) = split_at_closing_fence(rest).ok_or(PageError::NoFrontMatter)?;

        let front: FrontMatter = serde_norway::from_str(header)
            .map_err(|e| PageError::BadYaml(e.to_string()))?;
        if front.kind.trim().is_empty() {
            return Err(PageError::NoType);
        }
        Ok(Self { id, front, body: body.trim_start_matches('\n').to_owned() })
    }

    /// Write the page out, header first, in a fixed key order.
    ///
    /// By hand rather than by the serialiser, for one reason that shows up in
    /// every later diff: a stable order means a one-word change is a one-line
    /// change. It also keeps the header free of the keys that are empty.
    pub fn to_markdown(&self) -> String {
        let f = &self.front;
        let mut out = String::from("---\n");
        out.push_str(&format!("type: {}\n", yaml_scalar(&f.kind)));
        if !f.title.is_empty() {
            out.push_str(&format!("title: {}\n", yaml_scalar(&f.title)));
        }
        if let Some(description) = &f.description {
            out.push_str(&format!("description: {}\n", yaml_scalar(description)));
        }
        if !f.keywords.is_empty() {
            out.push_str(&format!("keywords: {}\n", yaml_list(&f.keywords)));
        }
        if !f.aliases.is_empty() {
            out.push_str(&format!("aliases: {}\n", yaml_list(&f.aliases)));
        }
        if !f.sources.is_empty() {
            out.push_str("sources:\n");
            for source in &f.sources {
                out.push_str(&format!("  - resource: {}\n", yaml_scalar(&source.resource)));
                if let Some(title) = &source.title {
                    out.push_str(&format!("    title: {}\n", yaml_scalar(title)));
                }
                if let Some(author) = &source.author {
                    out.push_str(&format!("    author: {}\n", yaml_scalar(author)));
                }
            }
        }
        if let Some(generated) = &f.generated {
            out.push_str(&format!("generated:\n  by: {}\n", yaml_scalar(&generated.by)));
            if let Some(at) = &generated.at {
                out.push_str(&format!("  at: {}\n", yaml_scalar(at)));
            }
        }
        if !f.verified.is_empty() {
            out.push_str("verified:\n");
            for actor in &f.verified {
                out.push_str(&format!("  - by: {}\n", yaml_scalar(&actor.by)));
                if let Some(at) = &actor.at {
                    out.push_str(&format!("    at: {}\n", yaml_scalar(at)));
                }
            }
        }
        out.push_str(&format!("status: {}\n", status_name(f.status)));
        if let Some(stale_after) = &f.stale_after {
            out.push_str(&format!("stale_after: {}\n", yaml_scalar(stale_after)));
        }
        out.push_str("---\n\n");
        out.push_str(self.body.trim_end());
        out.push('\n');
        out
    }

    /// The pages this one points at, in `[[id]]` form.
    ///
    /// Decision 85: these are **written** by the agent that wrote the page, so
    /// they are more accurate than anything extracted afterwards, and free.
    /// What is not a page link — a local path, a URL — belongs in `sources`,
    /// because the lint owes those three a different diagnosis (decision 88).
    pub fn links(&self) -> Vec<PageId> {
        let mut found = BTreeSet::new();
        let mut rest = self.body.as_str();
        while let Some(start) = rest.find("[[") {
            rest = &rest[start + 2..];
            let Some(end) = rest.find("]]") else { break };
            let name = rest[..end].trim();
            rest = &rest[end + 2..];
            if let Ok(id) = PageId::new(name) {
                if id != self.id {
                    found.insert(id);
                }
            }
        }
        found.into_iter().collect()
    }

    /// The `## Resume` section, which is what `recall` answers with.
    ///
    /// The original specification designates it as the part that can be injected cheaply, and
    /// decision 97 made it the payload of `recall`. Falls back to the whole body
    /// so a page written without the section is not silently unanswerable.
    pub fn resume(&self) -> &str {
        let text = section(&self.body, "Resume")
            .or_else(|| section(&self.body, "Résumé"))
            .unwrap_or_else(|| self.body.trim());
        text
    }

    /// Every name that should find this page: its id and its former ones.
    pub fn names(&self) -> Vec<String> {
        let mut names = vec![self.id.as_str().to_owned()];
        names.extend(self.front.aliases.iter().cloned());
        names
    }
}

/// The body of a `## <heading>` section, up to the next heading of any level.
fn section<'a>(body: &'a str, heading: &str) -> Option<&'a str> {
    let needle = format!("## {heading}");
    let start = body.find(&needle)? + needle.len();
    let rest = &body[start..];
    let end = rest.find("\n#").unwrap_or(rest.len());
    Some(rest[..end].trim())
}

/// Split at the `---` that closes the header.
fn split_at_closing_fence(rest: &str) -> Option<(&str, &str)> {
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            return Some((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Draft => "draft",
        Status::Stable => "stable",
        Status::Deprecated => "deprecated",
    }
}

/// A YAML scalar, quoted only when it has to be.
///
/// Quoting everything would be safe and would also make every header look like
/// code to the person reading it, which is the opposite of decision 88.
fn yaml_scalar(value: &str) -> String {
    let plain = !value.is_empty()
        && !value.starts_with(['&', '*', '!', '%', '@', '`', '>', '|', '-', '?', '#', ' ', '\''])
        && !value.ends_with(' ')
        && !value.contains([':', '"', '\n', '\r', '\t', '[', ']', '{', '}', ','])
        && !matches!(
            value.to_ascii_lowercase().as_str(),
            "true" | "false" | "null" | "yes" | "no" | "on" | "off" | "~"
        )
        && value.parse::<f64>().is_err();
    if plain {
        value.to_owned()
    } else {
        format!("\"{}\"", value.replace('\\', r"\\").replace('"', "\\\""))
    }
}

fn yaml_list(values: &[String]) -> String {
    let items: Vec<String> = values.iter().map(|v| yaml_scalar(v)).collect();
    format!("[{}]", items.join(", "))
}

/// A title to an id segment: lowercase, no accents, words joined by `-`.
///
/// Accent folding here is the same need the index has (decision 93 uses
/// `unicode61 remove_diacritics 2`), but it cannot be borrowed from SQLite:
/// this runs before anything is indexed, and an id has to be a filename.
pub fn slug(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut pending_dash = false;
    for ch in title.chars() {
        match fold(ch) {
            Some(folded) => {
                for c in folded.chars() {
                    keep(c, &mut out, &mut pending_dash);
                }
            }
            None if ch.is_ascii_alphanumeric() || ch == '_' => {
                keep(ch, &mut out, &mut pending_dash)
            }
            // Anything we do not know how to fold becomes a separator. A
            // shorter id, never a wrong one.
            None => pending_dash = true,
        }
    }
    out
}

/// Append one character, inserting the pending separator first.
fn keep(c: char, out: &mut String, pending_dash: &mut bool) {
    if *pending_dash && !out.is_empty() {
        out.push('-');
    }
    *pending_dash = false;
    out.push(c.to_ascii_lowercase());
}

/// A folder path, each segment slugged, `/` kept.
fn slug_path(folder: &str) -> String {
    folder.split('/').map(slug).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("/")
}

/// One accented character to its ASCII skeleton, or `None` if we do not fold it.
///
/// French and the neighbours a French keyboard reaches. Not a Unicode
/// normaliser, and it does not need to be: the alternative to folding is a
/// separator, so an unlisted character costs a shorter id, never a wrong one.
fn fold(ch: char) -> Option<&'static str> {
    Some(match ch {
        'à' | 'â' | 'ä' | 'á' | 'ã' | 'å' => "a",
        'À' | 'Â' | 'Ä' | 'Á' | 'Ã' | 'Å' => "A",
        'ç' => "c",
        'Ç' => "C",
        'é' | 'è' | 'ê' | 'ë' => "e",
        'É' | 'È' | 'Ê' | 'Ë' => "E",
        'î' | 'ï' | 'ì' | 'í' => "i",
        'Î' | 'Ï' | 'Ì' | 'Í' => "I",
        'ñ' => "n",
        'Ñ' => "N",
        'ô' | 'ö' | 'ò' | 'ó' | 'õ' => "o",
        'Ô' | 'Ö' | 'Ò' | 'Ó' | 'Õ' => "O",
        'ù' | 'û' | 'ü' | 'ú' => "u",
        'Ù' | 'Û' | 'Ü' | 'Ú' => "U",
        'ÿ' | 'ý' => "y",
        'œ' => "oe",
        'Œ' => "OE",
        'æ' => "ae",
        'Æ' => "AE",
        'ß' => "ss",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "---\n\
type: profil\n\
title: Louis\n\
keywords: [café, préférences, boisson]\n\
status: stable\n\
---\n\
\n\
## Resume\n\
Il prend son café serré, sans sucre.\n\
\n\
## Faits\n\
- Travaille sur [[projets/aria]]\n\
- Voir aussi [[procedures/env-dev]]\n";

    #[test]
    fn a_page_reads_its_header_its_resume_and_its_links() {
        let id = PageId::new("profil/louis").expect("id");
        let page = Page::parse(id, SAMPLE).expect("parse");

        assert_eq!(page.front.kind, "profil");
        assert_eq!(page.front.title, "Louis");
        assert_eq!(page.front.keywords, ["café", "préférences", "boisson"]);
        assert_eq!(page.front.status, Status::Stable);
        assert_eq!(page.resume(), "Il prend son café serré, sans sucre.");
        assert_eq!(
            page.links().iter().map(PageId::as_str).collect::<Vec<_>>(),
            ["procedures/env-dev", "projets/aria"]
        );
    }

    /// The round trip is what protects the header a model never sees: a
    /// `str_replace` on the body (decision 91) rewrites the whole file, so a
    /// serialiser that drops `sources` or `verified` would quietly destroy the
    /// provenance decision 87 relies on.
    #[test]
    fn a_page_survives_being_written_and_read_again() {
        let id = PageId::new("profil/louis").expect("id");
        let mut page = Page::parse(id.clone(), SAMPLE).expect("parse");
        page.front.sources.push(Source {
            resource: "https://example.org/a".into(),
            title: Some("Une page : avec deux points".into()),
            author: None,
        });
        page.front.verified.push(Actor { by: "human:louis".into(), at: Some("2026-11-11".into()) });
        page.front.aliases.push("profil/utilisateur".into());
        page.front.stale_after = Some("2027-01-01".into());
        page.front.status = Status::Deprecated;

        let again = Page::parse(id, &page.to_markdown()).expect("reparse");
        assert_eq!(again.front, page.front);
        assert_eq!(again.body.trim(), page.body.trim());
    }

    #[test]
    fn a_header_without_type_is_refused_because_okf_requires_it() {
        let id = PageId::new("profil/louis").expect("id");
        let text = "---\ntitle: Louis\n---\n\ncorps\n";
        assert!(matches!(Page::parse(id, text), Err(PageError::NoType)));
    }

    #[test]
    fn a_page_without_front_matter_is_refused_rather_than_guessed() {
        let id = PageId::new("profil/louis").expect("id");
        assert!(matches!(Page::parse(id, "## Resume\ncorps\n"), Err(PageError::NoFrontMatter)));
    }

    #[test]
    fn an_id_refuses_what_must_never_reach_a_filesystem() {
        for bad in ["", "/absolu", "profil/", "../escape", "profil/../x", "Profil/Louis", "a b"] {
            assert!(PageId::new(bad).is_err(), "{bad:?} aurait dû être refusé");
        }
        assert!(PageId::new("profil/utilisateur").is_ok());
        assert!(PageId::new("veille/2026-11-11-brief").is_ok());
    }

    #[test]
    fn a_title_becomes_an_id_without_accents_or_spaces() {
        assert_eq!(slug("Préférences de café"), "preferences-de-cafe");
        assert_eq!(slug("  Projet ARIA !!  "), "projet-aria");
        assert_eq!(slug("Cœur & âme"), "coeur-ame");
        let id = PageId::from_title("profil", "Préférences de café").expect("id");
        assert_eq!(id.as_str(), "profil/preferences-de-cafe");
        assert_eq!(id.folder(), "profil");
        assert_eq!(id.file_name(), "profil/preferences-de-cafe.md");
    }

    /// A value that would change meaning unquoted. Written out because the
    /// header is emitted by hand, so nothing else would catch it.
    #[test]
    fn a_scalar_that_needs_quoting_gets_it() {
        assert_eq!(yaml_scalar("Louis"), "Louis");
        assert_eq!(yaml_scalar("Projet : ARIA"), "\"Projet : ARIA\"");
        assert_eq!(yaml_scalar("true"), "\"true\"");
        assert_eq!(yaml_scalar("2026"), "\"2026\"");
        assert_eq!(yaml_scalar("- tiret"), "\"- tiret\"");
        assert_eq!(yaml_scalar(""), "\"\"");
    }
}
