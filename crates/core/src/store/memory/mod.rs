//! The memory: pages on disk, an index derived from them, four operations.
//!
//! # What an agent can do, and which agent
//!
//! | Operation | Who calls it | Decision |
//! |---|---|---|
//! | [`Memory::recall`] | the voice agent, every turn | 86, 89, 97 |
//! | [`Memory::remember`] | the voice agent, naming the page itself | 91, 97 |
//! | [`Memory::replace`] | the voice agent, to correct a fragment | 91 |
//! | [`Memory::rename`] | a rewrite that changes the title | 98 |
//!
//! # When a page is vectorised, and why it is on the write
//!
//! Decision 103: **every write vectorises the page it wrote, synchronously,
//! before it answers.** One page is one forward pass — tens of milliseconds,
//! against a catch-up of three hundred that is a minute — so the cost decision
//! 63 warned about is the batch's, not the write's. What it buys is that a fact
//! just told to ARIA is reachable by meaning immediately instead of at the next
//! silence, which is exactly when someone asks about it again.
//!
//! [`Memory::vectorise_pending`] stays, and is now only the catch-up: a first
//! start, a model changed, pages the user edited in an editor, a write whose
//! forward pass failed. Decision 92's fingerprint is what makes the two the
//! same code.
//!
//! # Why a page that will not parse does not stop the memory
//!
//! The whole argument of decision 88 is that the user opens these files and
//! edits them. So one broken page must cost one page, never the memory: a file
//! that will not parse is counted and skipped. The alternative — refusing to
//! start — would punish exactly the behaviour the format exists to allow.

pub mod index;
pub mod page;
pub mod semantic;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use index::{Hit, Index};
use page::{Page, PageError, PageId, Status};
use semantic::{Embedder, Text, Vectors};

use super::write::{self, Root};

/// How much a page reached by one link keeps, on the deep path.
///
/// Separate from the lexical constant on purpose: here it applies to a fused
/// rank score, not to a BM25 score, and the two are not the same quantity.
const DEEP_HOP_DECAY: f64 = 0.4;

/// Files that are the index's, not pages: generated, and rebuilt (decision 88).
const RESERVED: [&str; 2] = ["index.md", "log.md"];

/// Where a fact goes when the model names no page (decision 97).
const JOURNAL: &str = "journal";

/// Why an operation on the memory did not happen.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error(transparent)]
    Path(#[from] write::PathRefusal),
    #[error(transparent)]
    Page(#[from] PageError),
    #[error("the index could not be built: {0}")]
    Index(#[from] rusqlite::Error),
    #[error("no page is named {0:?}")]
    NoSuchPage(String),
    /// `str_replace` semantics, and the reason the model copes with them: an
    /// ambiguous edit is refused rather than applied to the first match.
    #[error("{found} fragments match in {id}, so the replacement is ambiguous")]
    NotUnique { id: String, found: usize },
    #[error("nothing in {id} matches the fragment to replace")]
    NoMatch { id: String },
    /// Decision 98: a rename never overwrites.
    #[error("{0:?} already exists, so the rename would overwrite it")]
    WouldOverwrite(String),
    /// The vectoriser failed. Never fatal to the memory: the lexical half
    /// answers without it (decision 83).
    #[error("the vectoriser failed: {0}")]
    Embed(String),
    /// No memory folder has been attached yet.
    ///
    /// The core receives its root from `app/` (decision 94), so between
    /// start-up and that call there is no memory — the same shape as a provider
    /// that is not attached, and answered the same way rather than panicked.
    #[error("no memory folder is attached")]
    Detached,
}

/// What `recall` answers with: the page, and two sentences from it.
#[derive(Clone, Debug, PartialEq)]
pub struct Recalled {
    /// Decision 91 made this explicit: a correction needs the id, and without
    /// it the model would have to search again to fix what it just read.
    pub id: PageId,
    pub resume: String,
    pub by_link: bool,
}

/// The semantic half, when a vectoriser has been attached.
///
/// Optional in the type, and that is decision 83 honoured rather than described:
/// the lexical half needs no model and always answers, so a missing or
/// still-loading embedder degrades retrieval instead of stopping ARIA.
struct Deep {
    embedder: Arc<dyn Embedder>,
    vectors: Vectors,
}

/// The memory, loaded.
pub struct Memory {
    root: Root,
    pages: BTreeMap<PageId, Page>,
    index: Index,
    /// Files found that would not parse. Reported rather than hidden.
    unreadable: Vec<String>,
    deep: Option<Deep>,
}

impl Memory {
    /// Load every page under the root and build the index.
    pub fn open(root: Root) -> Result<Self, MemoryError> {
        let mut memory = Self {
            root,
            pages: BTreeMap::new(),
            index: Index::new()?,
            unreadable: Vec::new(),
            deep: None,
        };
        memory.reload()?;
        Ok(memory)
    }

    /// Attach a vectoriser, and the cache its vectors live in.
    ///
    /// Called when the weights have finished loading, not at start-up —
    /// decision 97 corrected decision 83 on this: the embedder has its **own**
    /// loading state and must never gate the voice, so the memory works before
    /// it arrives and improves when it does.
    ///
    /// `cache` is the path under `%LOCALAPPDATA%` (decision 94). `None` keeps
    /// the vectors in memory, which is what a run with nowhere to write falls
    /// back to rather than refusing.
    pub fn attach_embedder(
        &mut self,
        embedder: Arc<dyn Embedder>,
        cache: Option<&Path>,
    ) -> Result<(), MemoryError> {
        let vectors = Vectors::open(cache, embedder.dims())?;
        tracing::info!(
            model = embedder.id(),
            dims = embedder.dims(),
            held = vectors.len(),
            "vectoriseur attaché"
        );
        self.deep = Some(Deep { embedder, vectors });
        Ok(())
    }

    /// Whether the deep path can answer at all.
    pub fn deep_ready(&self) -> bool {
        self.deep.as_ref().is_some_and(|deep| !deep.vectors.is_empty())
    }

    /// How many pages are waiting to be vectorised (decision 92).
    pub fn pending_vectors(&self) -> usize {
        match &self.deep {
            Some(deep) => {
                let pages: Vec<Page> = self.pages.values().cloned().collect();
                deep.vectors.stale(&pages, deep.embedder.id()).len()
            }
            None => 0,
        }
    }

    /// Catch up on vectors, at most `budget` pages.
    ///
    /// **This is the catch-up, no longer the normal path** — decision 103 put
    /// the vector on the write. What is left for it is everything that did not
    /// pass through [`Memory::commit`]: a first start, a model changed under
    /// us, pages the user edited in an editor, and a write whose forward pass
    /// failed. It also drops the vectors of pages that no longer exist, which
    /// a write cannot notice.
    ///
    /// **The caller decides when, and decision 92 is strict about it: never
    /// while ARIA is speaking.** A forward pass takes CPU, decision 63
    /// established what that costs here — the synthesis is the bottleneck and
    /// the voice comes out chopped when something else eats cores. The budget
    /// is how a catch-up of three hundred pages is spread over several silences
    /// instead of holding one.
    ///
    /// Returns how many pages were embedded.
    pub fn vectorise_pending(&mut self, budget: usize) -> Result<usize, MemoryError> {
        let Some(deep) = &mut self.deep else { return Ok(0) };
        let pages: Vec<Page> = self.pages.values().cloned().collect();

        for orphan in deep.vectors.orphans(&pages) {
            deep.vectors.forget(&orphan)?;
        }

        let stale: Vec<Page> =
            deep.vectors.stale(&pages, deep.embedder.id()).into_iter().take(budget).cloned().collect();
        if stale.is_empty() {
            deep.vectors.refresh()?;
            return Ok(0);
        }

        let texts: Vec<String> = stale.iter().map(semantic::passage_text).collect();
        let borrowed: Vec<&str> = texts.iter().map(String::as_str).collect();
        let embedded = deep
            .embedder
            .embed(&borrowed, Text::Passage)
            .map_err(|e| MemoryError::Embed(e.to_string()))?;
        if embedded.len() != stale.len() {
            return Err(MemoryError::Embed(format!(
                "{} vecteurs rendus pour {} pages",
                embedded.len(),
                stale.len()
            )));
        }
        for (page, vector) in stale.iter().zip(&embedded) {
            deep.vectors.put(page, deep.embedder.id(), vector)?;
        }
        deep.vectors.refresh()?;
        Ok(stale.len())
    }

    /// Vectorise one page, now, as part of the write that produced it.
    ///
    /// Decision 103. Three things it does **not** do, each on purpose:
    ///
    /// - it does not re-embed a page whose fingerprint still matches. A rename
    ///   calls [`Memory::commit`] once per inbound link plus once for the page,
    ///   so that is unbounded — but the embedded text is title + keywords +
    ///   resume, and rewriting a `[[link]]` in a body changes none of them. One
    ///   forward pass, however many pages pointed at this one;
    /// - it does not fail the write (see the caller): the page is on disk
    ///   before this runs, and decision 92's fingerprint means a vector that
    ///   did not get made is simply still stale — the next catch-up takes it;
    /// - it does not touch orphans, which are a sweep and not a write.
    ///
    /// Returns whether a vector was actually written.
    fn vectorise_now(&mut self, id: &PageId) -> Result<bool, MemoryError> {
        let Some(page) = self.pages.get(id).cloned() else { return Ok(false) };
        let Some(deep) = &mut self.deep else { return Ok(false) };

        let one = std::slice::from_ref(&page);
        if deep.vectors.stale(one, deep.embedder.id()).is_empty() {
            return Ok(false);
        }

        let text = semantic::passage_text(&page);
        let mut embedded = deep
            .embedder
            .embed(&[text.as_str()], Text::Passage)
            .map_err(|e| MemoryError::Embed(e.to_string()))?;
        if embedded.is_empty() {
            return Err(MemoryError::Embed(format!("aucun vecteur rendu pour {id}")));
        }
        deep.vectors.put(&page, deep.embedder.id(), &embedded.remove(0))?;
        deep.vectors.refresh()?;
        Ok(true)
    }

    /// The deep path: both halves fused, then one hop (decisions 85, 89).
    ///
    /// "Slow but more detail" — and decision 89 named what is actually slow,
    /// because getting that wrong would mean optimising the wrong end. Not the
    /// scan: a thousand pages is 1.5 MB. It is encoding the query, one forward
    /// pass, and what this returns — whole pages instead of two sentences,
    /// which is a cost in **tokens** and is why the voice agent defers it.
    ///
    /// Without a vectoriser this is the lexical search, which is a degradation
    /// and not a failure.
    pub fn deep_search(&self, query: &str, limit: usize) -> Result<Vec<Hit>, MemoryError> {
        let lexical = self.index.search(query, limit * 2)?;
        let Some(deep) = &self.deep else { return Ok(lexical) };

        let mut encoded = deep
            .embedder
            .embed(&[query], Text::Query)
            .map_err(|e| MemoryError::Embed(e.to_string()))?;
        if encoded.is_empty() {
            return Ok(lexical);
        }
        let near = deep.vectors.nearest(&encoded.remove(0), limit * 2);
        let fused = semantic::fuse(&lexical, &near, limit * 2);

        // The hop comes after the fusion, so a page reached by a link never
        // competes as though it had been matched by either half.
        let mut hits = fused.clone();
        let matched: Vec<PageId> = fused.iter().map(|hit| hit.id.clone()).collect();
        for hit in &fused {
            for neighbour in self.index.neighbours(&hit.id)? {
                if matched.contains(&neighbour) || hits.iter().any(|h| h.id == neighbour) {
                    continue;
                }
                hits.push(Hit {
                    id: neighbour,
                    score: hit.score * DEEP_HOP_DECAY,
                    by_link: true,
                });
            }
        }
        hits.sort_by(|a, b| {
            b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    /// What a delegated session reads: the pages themselves, not two sentences.
    pub fn deep_pages(&self, query: &str, limit: usize) -> Result<Vec<&Page>, MemoryError> {
        Ok(self
            .deep_search(query, limit)?
            .into_iter()
            .filter_map(|hit| self.pages.get(&hit.id))
            .filter(|page| page.front.status != Status::Deprecated)
            .collect())
    }

    /// Re-read the folder and rebuild the index.
    ///
    /// Decision 92's trigger is the **state of the files**, not our own write
    /// calls — because a delegated session writes with its own tools and the
    /// user writes with an editor, and neither passes through here.
    pub fn reload(&mut self) -> Result<(), MemoryError> {
        self.pages.clear();
        self.unreadable.clear();
        let root = self.root.path().to_path_buf();
        self.collect(&root, &mut Vec::new());
        let pages: Vec<Page> = self.pages.values().cloned().collect();
        self.index.rebuild(&pages)?;
        if !self.unreadable.is_empty() {
            tracing::warn!(count = self.unreadable.len(), pages = ?self.unreadable,
                "pages de mémoire illisibles, ignorées");
        }
        Ok(())
    }

    fn collect(&mut self, dir: &std::path::Path, trail: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // `.history` and anything else hidden is not a page.
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            if path.is_dir() {
                trail.push(name);
                self.collect(&path, trail);
                trail.pop();
                continue;
            }
            if !name.ends_with(".md") || RESERVED.contains(&name.as_str()) {
                continue;
            }
            let stem = name.trim_end_matches(".md");
            let relative = if trail.is_empty() {
                stem.to_owned()
            } else {
                format!("{}/{stem}", trail.join("/"))
            };
            match PageId::new(&relative)
                .and_then(|id| {
                    let text = std::fs::read_to_string(&path).map_err(|_| {
                        PageError::BadId(relative.clone())
                    })?;
                    Page::parse(id, &text)
                }) {
                Ok(page) => {
                    self.pages.insert(page.id.clone(), page);
                }
                Err(_) => self.unreadable.push(relative),
            }
        }
    }

    pub fn page(&self, id: &PageId) -> Option<&Page> {
        self.pages.get(id)
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Files found that would not parse — what the lint of decision 85 reports
    /// first, because an unreadable page is invisible rather than wrong.
    pub fn unreadable(&self) -> &[String] {
        &self.unreadable
    }

    /// The fast path: an id, an alias, or a few keywords.
    ///
    /// Answers with the `## Resume` section, not the page (decision 97). A
    /// `deprecated` page is skipped: it is kept so nothing is lost, not so it
    /// can be said out loud.
    pub fn recall(&self, query: &str) -> Result<Option<Recalled>, MemoryError> {
        for hit in self.index.search(query, 5)? {
            let Some(page) = self.pages.get(&hit.id) else { continue };
            if page.front.status == Status::Deprecated {
                continue;
            }
            return Ok(Some(Recalled {
                id: hit.id,
                resume: page.resume().to_owned(),
                by_link: hit.by_link,
            }));
        }
        Ok(None)
    }

    /// Several answers, for a caller that can afford to read more than two
    /// sentences — a delegated session, or the lint.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Hit>, MemoryError> {
        Ok(self.index.search(query, limit)?)
    }

    /// Several answers, each summarised rather than whole (decisions 89, 105).
    ///
    /// The deep search fused and then **cut down to resumes**, which is the
    /// shape decision 89 was actually arguing about. What it priced as too
    /// expensive for a speaking turn was not the scan — a thousand pages is
    /// 1.5 MB — it was returning whole pages. So this is the deep ranking at
    /// the fast path's price: the model gets the ids and two sentences each,
    /// and reads in full only what it decides to read ([`Memory::read`]).
    ///
    /// A `deprecated` page is skipped, as in [`Memory::recall`] and for the
    /// same reason.
    pub fn deep_recall(&self, query: &str, limit: usize) -> Result<Vec<Recalled>, MemoryError> {
        let mut found = Vec::new();
        for hit in self.deep_search(query, limit)? {
            let Some(page) = self.pages.get(&hit.id) else { continue };
            if page.front.status == Status::Deprecated {
                continue;
            }
            found.push(Recalled {
                id: hit.id,
                resume: page.resume().to_owned(),
                by_link: hit.by_link,
            });
        }
        Ok(found)
    }

    /// One page, in full, as it is on disk.
    ///
    /// The counterpart of [`Memory::recall`] for a reader that is not speaking.
    /// `recall` answers with the `## Resume` section because everything it
    /// returns is about to be pronounced (decision 97); a delegated session
    /// reads instead, and the sections `recall` drops — `## Faits`, the
    /// sources, the links — are exactly what it came for.
    ///
    /// Resolved by id or alias only, never by keywords: the caller got the id
    /// from a search, and a `read_page` that quietly returned a *different*
    /// page because the name was close would be worse than a refusal.
    ///
    /// A `deprecated` page **is** returned here, unlike in `recall`. It is
    /// skipped there because it must not be said out loud; a reader that asked
    /// for it by id is owed what the file says.
    pub fn read(&self, name: &str) -> Result<String, MemoryError> {
        let id = self
            .index
            .resolve(name.trim())?
            .ok_or_else(|| MemoryError::NoSuchPage(name.to_owned()))?;
        let page = self
            .pages
            .get(&id)
            .ok_or_else(|| MemoryError::NoSuchPage(name.to_owned()))?;
        Ok(page.to_markdown())
    }

    /// Keep a fact. The model names the page; the core does not guess.
    ///
    /// Decision 97, and the motive is the one behind the tile request and the
    /// links: what the model knows, it says. Three cases, in order:
    ///
    /// - the name resolves, by id or by alias → the fact joins that page;
    /// - the name is a usable id that nothing holds → the page is created,
    ///   because naming it was an intention and a typo shows up in a file
    ///   rather than in silence;
    /// - no name → a dated page in `journal/`, which is where the original specification puts
    ///   what happened on a given day.
    pub fn remember(&mut self, fact: &str, page: Option<&str>) -> Result<PageId, MemoryError> {
        let fact = fact.trim();
        let id = match page.map(str::trim).filter(|name| !name.is_empty()) {
            Some(name) => match self.index.resolve(name)? {
                Some(id) => id,
                None => PageId::new(name)?,
            },
            None => PageId::new(format!("{JOURNAL}/{}", today()))?,
        };

        let mut target = match self.pages.get(&id) {
            Some(existing) => existing.clone(),
            None => {
                let kind = if id.folder().is_empty() { "note" } else { id.folder() };
                Page::new(id.clone(), kind, &title_from(&id))
            }
        };
        append_fact(&mut target, fact);
        self.commit(target)?;
        self.log(&format!("remember | {id}"), fact)?;
        Ok(id)
    }

    /// Correct a fragment, without ever seeing the header.
    ///
    /// Decision 91 chose substitution over rewriting the page for exactly this:
    /// the model cannot touch `sources` or `verified`, and an ambiguous edit
    /// fails loudly instead of landing on the first match.
    pub fn replace(&mut self, page: &str, from: &str, to: &str) -> Result<(), MemoryError> {
        let id = self
            .index
            .resolve(page.trim())?
            .ok_or_else(|| MemoryError::NoSuchPage(page.to_owned()))?;
        let mut target =
            self.pages.get(&id).cloned().ok_or_else(|| MemoryError::NoSuchPage(page.to_owned()))?;

        let found = target.body.matches(from).count();
        match found {
            0 => return Err(MemoryError::NoMatch { id: id.to_string() }),
            1 => {}
            found => return Err(MemoryError::NotUnique { id: id.to_string(), found }),
        }
        target.body = target.body.replacen(from, to, 1);
        self.commit(target)?;
        self.log(&format!("replace | {id}"), to)?;
        Ok(())
    }

    /// Give a page a new title, and therefore a new id (decision 98).
    ///
    /// Three obligations, and none of them can wait for the next lint:
    /// inbound links are rewritten now, the former id is kept as an alias so an
    /// older reference still lands, and the history follows the page.
    ///
    /// The Windows trap decision 98 noted — a rename differing only in case —
    /// cannot arise: [`PageId`] accepts lowercase only, so two ids that differ
    /// in case do not both exist.
    pub fn rename(&mut self, page: &str, new_title: &str) -> Result<PageId, MemoryError> {
        let old = self
            .index
            .resolve(page.trim())?
            .ok_or_else(|| MemoryError::NoSuchPage(page.to_owned()))?;
        let mut target =
            self.pages.get(&old).cloned().ok_or_else(|| MemoryError::NoSuchPage(page.to_owned()))?;
        let new = PageId::from_title(old.folder(), new_title)?;

        if new != old {
            if self.pages.contains_key(&new) {
                return Err(MemoryError::WouldOverwrite(new.to_string()));
            }
            // The former id becomes an alias before anything moves, so a
            // `remember` naming the old id keeps working from the next reload.
            let former = old.as_str().to_owned();
            if !target.front.aliases.contains(&former) {
                target.front.aliases.push(former);
            }
            target.id = new.clone();
        }
        target.front.title = new_title.trim().to_owned();

        // Inbound links first: if anything fails, the page has not moved yet
        // and the graph is still whole.
        if new != old {
            for source in self.index.inbound(&old)? {
                let Some(linking) = self.pages.get(&source).cloned() else { continue };
                let mut linking = linking;
                let rewritten = linking
                    .body
                    .replace(&format!("[[{old}]]"), &format!("[[{new}]]"))
                    .replace(&format!("[[ {old} ]]"), &format!("[[{new}]]"));
                if rewritten != linking.body {
                    linking.body = rewritten;
                    self.commit(linking)?;
                }
            }
        }

        self.commit(target)?;
        if new != old {
            let path = self.root.resolve(&old.file_name())?;
            let _ = std::fs::remove_file(path);
            self.pages.remove(&old);
            // The vector of an id that no longer exists would answer searches
            // with a page nobody can open. Dropped here rather than left to
            // the sweep, for the same reason the links were rewritten here.
            if let Some(deep) = &mut self.deep {
                let _ = deep.vectors.forget(&old);
                let _ = deep.vectors.refresh();
            }
            write::move_history(&self.root, old.as_str(), new.as_str())?;
            self.index.rebuild(&self.pages.values().cloned().collect::<Vec<_>>())?;
        }
        self.log(&format!("rename | {old} -> {new}"), new_title)?;
        Ok(new)
    }

    /// Write a page: keep the version it replaces, then replace it atomically.
    fn commit(&mut self, page: Page) -> Result<(), MemoryError> {
        let path = self.root.resolve(&page.id.file_name())?;
        if let Ok(previous) = std::fs::read_to_string(&path) {
            write::archive(&self.root, page.id.as_str(), &previous)?;
        }
        write::write_atomic(&path, &page.to_markdown())?;
        let id = page.id.clone();
        self.pages.insert(id.clone(), page);
        let pages: Vec<Page> = self.pages.values().cloned().collect();
        self.index.rebuild(&pages)?;

        // Decision 103: the vector follows the write, in the same call, so the
        // tool does not answer "noté" while half the memory cannot find it.
        //
        // **And a failure here costs the vector, never the fact.** The file is
        // already on disk and the lexical index already holds it; refusing the
        // write now would throw away what the user said because a model was
        // not loaded. The page simply stays stale, and that is a state the
        // catch-up already knows how to read.
        if let Err(e) = self.vectorise_now(&id) {
            tracing::warn!(page = %id, "vecteur non écrit, la page reste à vectoriser : {e}");
        }
        Ok(())
    }

    /// One line per change, newest first — the reserved `log.md` of the motif.
    ///
    /// Newest first because OKF asks for it, and because the only reader who
    /// opens this file wants to know what just happened.
    fn log(&self, what: &str, detail: &str) -> Result<(), MemoryError> {
        let path = self.root.resolve("log.md")?;
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let body = existing.strip_prefix("# Journal des changements\n").unwrap_or(&existing);
        let detail = detail.lines().next().unwrap_or("").trim();
        let entry = format!("\n## [{}] {what}\n{detail}\n", today());
        write::write_atomic(&path, &format!("# Journal des changements\n{entry}{body}"))?;
        Ok(())
    }
}

/// Add a fact under `## Faits`, creating the section if the page has none.
fn append_fact(page: &mut Page, fact: &str) {
    let line = format!("- {fact}");
    if page.body.contains(&line) {
        return;
    }
    match page.body.find("## Faits") {
        Some(start) => {
            let rest = &page.body[start..];
            let cut = rest.find("\n#").map(|at| start + at).unwrap_or(page.body.len());
            let mut body = page.body.clone();
            body.insert_str(cut, &format!("\n{line}"));
            page.body = body;
        }
        None => {
            if !page.body.is_empty() && !page.body.ends_with('\n') {
                page.body.push('\n');
            }
            page.body.push_str(&format!("\n## Faits\n{line}\n"));
        }
    }
}

/// A readable title for a page created from an id.
fn title_from(id: &PageId) -> String {
    let last = id.as_str().rsplit('/').next().unwrap_or(id.as_str());
    let mut title = last.replace(['-', '_'], " ");
    if let Some(first) = title.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    title
}

/// `YYYY-MM-DD`, UTC. Absolute dates, never "last week" — the original specification.
fn today() -> String {
    write::today()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> (tempfile::TempDir, Memory) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = Root::open(dir.path()).expect("root");
        let memory = Memory::open(root).expect("open");
        (dir, memory)
    }

    #[test]
    fn an_empty_folder_is_an_empty_memory_not_a_failure() {
        let (_dir, memory) = memory();
        assert!(memory.is_empty());
        assert_eq!(memory.recall("café").expect("recall"), None);
    }

    #[test]
    fn a_fact_remembered_is_on_disk_and_findable() {
        let (_dir, mut memory) = memory();
        let id = memory
            .remember("Il prend son café serré, sans sucre.", Some("profil/louis"))
            .expect("remember");
        assert_eq!(id.as_str(), "profil/louis");

        // On disk, parseable, and the header survived.
        let path = memory.root.resolve("profil/louis.md").expect("resolve");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.starts_with("---\ntype: profil\n"), "{text}");
        assert!(text.contains("- Il prend son café serré"));

        // And a fresh load finds it, which is the only proof that matters.
        let reloaded = Memory::open(Root::open(memory.root.path()).expect("root")).expect("open");
        let found = reloaded.recall("café").expect("recall").expect("une page");
        assert_eq!(found.id.as_str(), "profil/louis");
        assert!(found.resume.contains("café serré"));
    }

    #[test]
    fn a_fact_with_no_page_named_lands_in_a_dated_journal() {
        let (_dir, mut memory) = memory();
        let id = memory.remember("Il a acheté un moulin.", None).expect("remember");
        assert!(id.as_str().starts_with("journal/"), "{id}");
        assert!(memory.page(&id).is_some());
    }

    #[test]
    fn a_correction_replaces_one_fragment_and_leaves_the_header_alone() {
        let (_dir, mut memory) = memory();
        memory.remember("Il prend son café avec du sucre.", Some("profil/louis")).expect("remember");
        let before = memory.page(&PageId::new("profil/louis").unwrap()).unwrap().front.clone();

        memory.replace("profil/louis", "avec du sucre", "sans sucre").expect("replace");

        let after = memory.page(&PageId::new("profil/louis").unwrap()).unwrap();
        assert!(after.body.contains("sans sucre"));
        assert!(!after.body.contains("avec du sucre"));
        assert_eq!(after.front, before, "l'en-tête ne doit pas bouger");
    }

    #[test]
    fn an_ambiguous_correction_is_refused_rather_than_applied_to_the_first_match() {
        let (_dir, mut memory) = memory();
        memory.remember("café serré", Some("profil/louis")).expect("un");
        memory.remember("café long", Some("profil/louis")).expect("deux");

        let refused = memory.replace("profil/louis", "café", "thé");
        assert!(matches!(refused, Err(MemoryError::NotUnique { found: 2, .. })), "{refused:?}");
        assert!(matches!(
            memory.replace("profil/louis", "absent", "x"),
            Err(MemoryError::NoMatch { .. })
        ));
    }

    /// Decision 98, all three obligations in one test, because they only have
    /// value together: a rename that keeps the alias but breaks the links is
    /// as bad as one that does neither.
    #[test]
    fn a_rename_rewrites_inbound_links_keeps_the_alias_and_moves_the_history() {
        let (_dir, mut memory) = memory();
        memory.remember("Un fait.", Some("profil/utilisateur")).expect("page");
        // A second write, so there is a version to move.
        memory.remember("Un autre fait.", Some("profil/utilisateur")).expect("page");
        memory
            .remember("Voir [[profil/utilisateur]] pour le détail.", Some("veille/sujets"))
            .expect("lien");

        let new = memory.rename("profil/utilisateur", "Louis").expect("rename");
        assert_eq!(new.as_str(), "profil/louis");

        // 1. the inbound link now points at the new id
        let linking = memory.page(&PageId::new("veille/sujets").unwrap()).unwrap();
        assert!(linking.body.contains("[[profil/louis]]"), "{}", linking.body);
        assert!(!linking.body.contains("[[profil/utilisateur]]"));

        // 2. the former id still resolves, which is what `remember` needs
        let renamed = memory.page(&new).expect("page");
        assert!(renamed.front.aliases.contains(&"profil/utilisateur".to_string()));
        assert_eq!(
            memory.recall("profil/utilisateur").expect("recall").map(|r| r.id),
            Some(new.clone())
        );

        // 3. the history moved with the page
        let moved = memory
            .root
            .resolve(&format!("{}/profil/louis", write::HISTORY_DIR))
            .expect("resolve");
        assert!(moved.exists(), "l'historique n'a pas suivi");

        // and the old file is gone
        assert!(!memory.root.resolve("profil/utilisateur.md").unwrap().exists());
    }

    #[test]
    fn a_rename_onto_an_existing_page_fails_instead_of_overwriting() {
        let (_dir, mut memory) = memory();
        memory.remember("Un.", Some("profil/louis")).expect("un");
        memory.remember("Deux.", Some("profil/utilisateur")).expect("deux");

        let refused = memory.rename("profil/utilisateur", "Louis");
        assert!(matches!(refused, Err(MemoryError::WouldOverwrite(_))), "{refused:?}");
        // And nothing moved.
        assert!(memory.page(&PageId::new("profil/utilisateur").unwrap()).is_some());
        assert_eq!(memory.page(&PageId::new("profil/louis").unwrap()).unwrap().body.trim(), "## Faits\n- Un.");
    }

    #[test]
    fn a_page_that_will_not_parse_costs_one_page_not_the_memory() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("profil")).expect("mkdir");
        std::fs::write(dir.path().join("profil/cassee.md"), "pas d'en-tête du tout\n")
            .expect("write");
        std::fs::write(
            dir.path().join("profil/bonne.md"),
            "---\ntype: profil\ntitle: Bonne\n---\n\n## Resume\nTout va bien.\n",
        )
        .expect("write");

        let memory = Memory::open(Root::open(dir.path()).expect("root")).expect("open");
        assert_eq!(memory.len(), 1);
        assert_eq!(memory.unreadable(), ["profil/cassee"]);
        assert!(memory.recall("profil/bonne").expect("recall").is_some());
    }

    #[test]
    fn a_deprecated_page_is_kept_but_never_answered_with() {
        let (_dir, mut memory) = memory();
        memory.remember("Il préférait le thé.", Some("profil/louis")).expect("page");
        let id = PageId::new("profil/louis").unwrap();
        let mut page = memory.page(&id).unwrap().clone();
        page.front.status = Status::Deprecated;
        memory.commit(page).expect("commit");

        assert!(memory.page(&id).is_some(), "la page reste");
        assert_eq!(memory.recall("thé").expect("recall"), None);
    }

    /// Without a vectoriser the deep path is the lexical one. A degradation,
    /// never a failure — decision 83, and the reason the embedder is optional
    /// in the type rather than assumed present.
    #[test]
    fn the_deep_path_answers_before_any_vectoriser_is_attached() {
        let (_dir, mut memory) = memory();
        memory.remember("Il prend son café serré.", Some("profil/louis")).expect("page");

        assert!(!memory.deep_ready());
        assert_eq!(memory.pending_vectors(), 0, "sans vectoriseur, rien n'est en attente");
        let hits = memory.deep_search("café", 3).expect("deep");
        assert_eq!(hits.first().map(|h| h.id.as_str()), Some("profil/louis"));
    }

    /// What the deep search buys over the fast one: a page whose **words** the
    /// query does not contain, found because its meaning is close.
    #[test]
    fn the_deep_path_finds_a_page_the_keywords_would_miss() {
        let (_dir, mut memory) = memory();
        memory.remember("Il adore le [[procedures/moulin]].", Some("profil/louis")).expect("un");
        memory.remember("Réglage fin, douze secondes.", Some("procedures/moulin")).expect("deux");
        memory.remember("Une voiture grise au garage.", Some("garage/auto")).expect("trois");

        // A vocabulary that connects "boisson" to the coffee page and to
        // nothing else: the lexical half cannot make that link, since the word
        // appears on no page.
        let bag = Arc::new(semantic::tests::Bag::new(&["louis", "moulin", "auto", "boisson"]));
        memory.attach_embedder(bag, None).expect("attach");
        assert!(memory.pending_vectors() >= 3, "tout est à vectoriser au départ");

        let done = memory.vectorise_pending(100).expect("vectorise");
        assert_eq!(done, memory.len());
        assert_eq!(memory.pending_vectors(), 0, "et rien ne reste");
        assert!(memory.deep_ready());

        // Asked again, nothing is re-embedded — decision 92's fingerprint.
        assert_eq!(memory.vectorise_pending(100).expect("encore"), 0);

        let hits = memory.deep_search("moulin", 3).expect("deep");
        assert!(
            hits.iter().any(|h| h.id.as_str() == "procedures/moulin"),
            "la page du moulin doit sortir : {hits:?}"
        );
    }

    /// The budget exists so a catch-up of three hundred pages is spread over
    /// several silences instead of holding one (decisions 63 and 92).
    #[test]
    fn the_catch_up_is_spread_across_calls_by_its_budget() {
        let (_dir, mut memory) = memory();
        for i in 0..5 {
            memory.remember(&format!("Fait {i}."), Some(&format!("notes/n{i}"))).expect("page");
        }
        let bag = Arc::new(semantic::tests::Bag::new(&["fait", "note"]));
        memory.attach_embedder(bag, None).expect("attach");

        assert_eq!(memory.pending_vectors(), 5);
        assert_eq!(memory.vectorise_pending(2).expect("deux"), 2);
        assert_eq!(memory.pending_vectors(), 3);
        assert_eq!(memory.vectorise_pending(2).expect("deux"), 2);
        assert_eq!(memory.vectorise_pending(2).expect("un"), 1);
        assert_eq!(memory.pending_vectors(), 0);
    }

    /// Decision 103, and the whole point of it: nothing is called between the
    /// write and the search, and the deep half already answers.
    #[test]
    fn a_fact_remembered_is_vectorised_before_the_tool_answers() {
        let (_dir, mut memory) = memory();
        let bag = Arc::new(semantic::tests::Bag::new(&["louis", "cafe", "moulin"]));
        memory.attach_embedder(bag, None).expect("attach");

        memory.remember("Il prend son café serré.", Some("profil/louis")).expect("page");

        // No `vectorise_pending` anywhere above. That is the assertion.
        assert_eq!(memory.pending_vectors(), 0, "la page devait être vectorisée par l'écriture");
        assert!(memory.deep_ready());
        let hits = memory.deep_search("café", 3).expect("deep");
        assert_eq!(hits.first().map(|h| h.id.as_str()), Some("profil/louis"));
    }

    /// A page rewritten is re-embedded by the write that rewrote it.
    ///
    /// `pending_vectors` reading zero is the proof and not a formality: the
    /// fingerprint is taken from the **new** text, so a stored vector made from
    /// the old one would show up here as stale (decision 92).
    #[test]
    fn rewriting_a_page_re_embeds_it_in_the_same_call() {
        let (_dir, mut memory) = memory();
        memory.remember("Café serré.", Some("profil/louis")).expect("page");
        let bag = Arc::new(semantic::tests::Bag::new(&["cafe", "the"]));
        memory.attach_embedder(bag, None).expect("attach");
        memory.vectorise_pending(10).expect("vectorise");
        assert_eq!(memory.pending_vectors(), 0);

        memory.replace("profil/louis", "Café serré", "Thé vert").expect("replace");
        assert_eq!(memory.pending_vectors(), 0, "la réécriture devait re-vectoriser");
    }

    /// A rename leaves no vector behind under the former id.
    ///
    /// It would not be an orphan waiting for a sweep, it would be a **hit**:
    /// `deep_search` scans the vectors, so the old id would come back out of a
    /// search for a page that no longer exists.
    #[test]
    fn a_rename_drops_the_vector_of_the_former_id() {
        let (_dir, mut memory) = memory();
        let bag = Arc::new(semantic::tests::Bag::new(&["louis", "cafe"]));
        memory.attach_embedder(bag, None).expect("attach");
        memory.remember("Café serré.", Some("profil/utilisateur")).expect("page");

        memory.rename("profil/utilisateur", "Louis").expect("rename");

        let hits = memory.deep_search("café", 10).expect("deep");
        assert!(
            hits.iter().any(|h| h.id.as_str() == "profil/louis"),
            "la page renommée doit sortir : {hits:?}"
        );
        assert!(
            !hits.iter().any(|h| h.id.as_str() == "profil/utilisateur"),
            "l'ancien identifiant ne doit plus avoir de vecteur : {hits:?}"
        );
    }

    /// A vectoriser that fails must not take the memory with it.
    #[test]
    fn a_vectoriser_that_fails_leaves_the_fast_path_working() {
        struct Broken;
        impl semantic::Embedder for Broken {
            fn id(&self) -> &str {
                "broken/1"
            }
            fn dims(&self) -> usize {
                4
            }
            fn embed(
                &self,
                _texts: &[&str],
                _kind: Text,
            ) -> Result<Vec<Vec<f32>>, semantic::EmbedError> {
                Err(semantic::EmbedError("les poids ne sont pas là".into()))
            }
        }

        let (_dir, mut memory) = memory();
        memory.remember("Café serré.", Some("profil/louis")).expect("page");
        memory.attach_embedder(Arc::new(Broken), None).expect("attach");

        // Decision 103 put a forward pass on the write, so this is where a
        // broken model could cost the user a fact. It must not: the write
        // succeeds, and the page is simply left to vectorise later.
        memory.remember("Il a acheté un moulin.", Some("profil/louis")).expect("écriture");
        assert!(memory.page(&PageId::new("profil/louis").unwrap()).unwrap().body.contains("moulin"));
        assert_eq!(memory.pending_vectors(), 1, "la page reste à vectoriser");

        assert!(matches!(memory.vectorise_pending(10), Err(MemoryError::Embed(_))));
        assert!(matches!(memory.deep_search("café", 3), Err(MemoryError::Embed(_))));
        // And the path that needs no model still answers, which is the whole
        // point of decision 83.
        assert!(memory.recall("café").expect("recall").is_some());
    }

    #[test]
    fn the_log_records_each_change_newest_first() {
        let (_dir, mut memory) = memory();
        memory.remember("Premier fait.", Some("profil/louis")).expect("un");
        memory.remember("Second fait.", Some("profil/louis")).expect("deux");

        let log = std::fs::read_to_string(memory.root.resolve("log.md").unwrap()).expect("read");
        let first = log.find("Second fait").expect("le second est là");
        let second = log.find("Premier fait").expect("le premier est là");
        assert!(first < second, "le plus récent doit être en haut");
        assert!(log.starts_with("# Journal des changements\n"));
        // And `log.md` is not itself a page.
        assert!(memory.page(&PageId::new("log").unwrap()).is_none());
    }
}
