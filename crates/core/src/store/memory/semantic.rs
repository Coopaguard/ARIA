//! The deep search: vectors, their fingerprints, and the fusion.
//!
//! Everything here is arithmetic and SQLite, so it stays in the core.
//! Decision 93 shrank `crates/memory` to a **vectoriser** and nothing else —
//! [`Embedder`] is the whole of that boundary, four methods wide.
//!
//! # No approximate index, and it is arithmetic that says so
//!
//! Decision 84: a thousand pages at 384 dimensions in `f32` is 1.5 MB, and the
//! dot product against all thousand takes a fraction of a millisecond on a
//! processor where AVX2 is already required (decision 63). An ANN index would
//! be a structure to build, maintain and invalidate in order to speed up
//! something that is not slow. So: a contiguous buffer and a full scan.
//!
//! # The fingerprint is why nobody has to ask for a re-index
//!
//! Decision 92: the trigger is the **state of the files**, never our own write
//! calls — a delegated session writes with its own tools and the user writes
//! with an editor, and neither passes through us. So each stored vector carries
//! a fingerprint of what produced it: the text hashed, the model's id, the
//! dimension, and the prefix convention. "What still needs embedding" is then a
//! **query**, not a job list to persist — nothing to lose on a crash, and the
//! catch-up at start-up is the same code as the incremental case.
//!
//! The model's id is in there for a reason that does not announce itself:
//! vectors from two different models in one space do not produce an error, they
//! produce **wrong similarities**.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::Connection;

use super::index::Hit;
use super::page::{Page, PageId};

/// Which side of a retrieval a text is on.
///
/// This exists so the prefix trap of decision 83 cannot be forgotten by a
/// caller: `multilingual-e5` requires `query:` and `passage:`, and omitting
/// them degrades retrieval without breaking anything visible. The prefix is
/// applied inside the implementation, driven by this, so the only remaining
/// mistake is to mislabel — and a mislabel is what the test below catches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Text {
    /// What someone asked.
    Query,
    /// What a page says.
    Passage,
}

/// Why embedding failed.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct EmbedError(pub String);

/// The whole of what `crates/memory` provides.
///
/// Four methods, because decision 93 found that everything else — the index,
/// the edges, the scan, the fusion — is arithmetic that belongs in the core.
/// The crate exists only because a model wants ONNX.
pub trait Embedder: Send + Sync {
    /// Stable identity of the model, part of every fingerprint.
    ///
    /// Must change whenever the vectors it produces change meaning: a different
    /// checkpoint, a different quantisation, a different prefix convention.
    fn id(&self) -> &str;

    fn dims(&self) -> usize;

    /// Embed, normalised to unit length so a dot product is a cosine.
    fn embed(&self, texts: &[&str], kind: Text) -> Result<Vec<Vec<f32>>, EmbedError>;
}

/// The text of a page as it is embedded — one definition, used twice.
///
/// Called by both the fingerprint and the embedding, and that is the point: if
/// the two disagreed, every page would either re-embed for ever or never again.
///
/// Title and keywords lead, because they are what the page's author **declared**
/// it to be about (decision 88), and because decision 96 noted that
/// `EmbeddingGemma`'s own document template puts the title first.
pub fn passage_text(page: &Page) -> String {
    format!(
        "{} {} {}",
        page.front.title,
        page.front.keywords.join(" "),
        page.resume()
    )
    .trim()
    .to_owned()
}

/// What a stored vector was made from.
///
/// A string rather than a struct, because it is compared and never read apart:
/// anything that differs means re-embed.
fn fingerprint(page: &Page, model: &str, dims: usize) -> String {
    format!("{model}/{dims}/{:016x}", fnv1a(passage_text(page).as_bytes()))
}

/// FNV-1a, 64-bit.
///
/// Written out rather than taken from `DefaultHasher`, which is explicitly not
/// stable across Rust releases — a toolchain upgrade would silently invalidate
/// every vector in the store. Not cryptographic, and does not need to be: it
/// detects a change the user made, not one an attacker made.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// The vectors, and the only part of the index worth keeping on disk.
///
/// Decision 93: the lexical half is rebuilt at start-up because it costs
/// milliseconds; a forward pass per page per boot is the thing that would not
/// be. Decision 94 puts this file under `%LOCALAPPDATA%` — local, never
/// roamed — and not among the pages, which a home directory often synchronises.
pub struct Vectors {
    db: Connection,
    /// Every vector, end to end. Contiguous because the dot product vectorises,
    /// and AVX2 is already a requirement (decision 63).
    flat: Vec<f32>,
    ids: Vec<PageId>,
    dims: usize,
}

impl Vectors {
    /// Open the cache, or create it.
    ///
    /// `path` may be `None`, which keeps the vectors in memory only — what the
    /// tests use, and what a run with nowhere to write falls back to rather
    /// than refusing to start.
    pub fn open(path: Option<&Path>, dims: usize) -> rusqlite::Result<Self> {
        let db = match path {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let db = Connection::open(path)?;
                // One writer, concurrent readers — decision 91's queue, in the
                // form SQLite already provides.
                db.pragma_update(None, "journal_mode", "WAL")?;
                db.busy_timeout(std::time::Duration::from_secs(5))?;
                db
            }
            None => Connection::open_in_memory()?,
        };
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS vectors(
                 id TEXT PRIMARY KEY,
                 fingerprint TEXT NOT NULL,
                 vector BLOB NOT NULL
             );",
        )?;
        let mut vectors = Self { db, flat: Vec::new(), ids: Vec::new(), dims };
        vectors.load()?;
        Ok(vectors)
    }

    /// Read every vector into the flat buffer.
    fn load(&mut self) -> rusqlite::Result<()> {
        self.flat.clear();
        self.ids.clear();
        let mut statement = self.db.prepare("SELECT id, vector FROM vectors ORDER BY id")?;
        let rows = statement
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)))?;
        for row in rows {
            let (id, blob) = row?;
            let Ok(id) = PageId::new(id) else { continue };
            // A vector of the wrong width means the model changed under us.
            // Dropped rather than mixed in: decision 92's reason for putting
            // the model's id in the fingerprint.
            if blob.len() != self.dims * 4 {
                continue;
            }
            self.ids.push(id);
            self.flat.extend(
                blob.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            );
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// How much space the whole scan occupies — the number decision 84 rests on.
    pub fn bytes(&self) -> usize {
        self.flat.len() * std::mem::size_of::<f32>()
    }

    /// Pages whose vector is missing or made from something else.
    ///
    /// This **is** the work queue (decision 92): derived from the files every
    /// time it is asked, so there is nothing to persist and nothing to lose.
    pub fn stale<'a>(&self, pages: &'a [Page], model: &str) -> Vec<&'a Page> {
        let held: HashMap<String, String> = self
            .db
            .prepare("SELECT id, fingerprint FROM vectors")
            .and_then(|mut s| {
                let rows = s.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                rows.collect::<rusqlite::Result<HashMap<_, _>>>()
            })
            .unwrap_or_default();

        pages
            .iter()
            .filter(|page| {
                let wanted = fingerprint(page, model, self.dims);
                held.get(page.id.as_str()) != Some(&wanted)
            })
            .collect()
    }

    /// Vectors held for pages that no longer exist.
    pub fn orphans(&self, pages: &[Page]) -> Vec<PageId> {
        let alive: Vec<&str> = pages.iter().map(|p| p.id.as_str()).collect();
        self.ids.iter().filter(|id| !alive.contains(&id.as_str())).cloned().collect()
    }

    /// Store one page's vector, replacing whatever was there.
    pub fn put(&mut self, page: &Page, model: &str, vector: &[f32]) -> rusqlite::Result<()> {
        let blob: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.db.execute(
            "INSERT INTO vectors(id, fingerprint, vector) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET fingerprint = ?2, vector = ?3",
            rusqlite::params![page.id.as_str(), fingerprint(page, model, self.dims), blob],
        )?;
        Ok(())
    }

    pub fn forget(&mut self, id: &PageId) -> rusqlite::Result<()> {
        self.db.execute("DELETE FROM vectors WHERE id = ?1", [id.as_str()])?;
        Ok(())
    }

    /// Reload the flat buffer after writes.
    pub fn refresh(&mut self) -> rusqlite::Result<()> {
        self.load()
    }

    /// The full scan. Cosine, because everything stored is unit length.
    pub fn nearest(&self, query: &[f32], limit: usize) -> Vec<(PageId, f32)> {
        if query.len() != self.dims || self.ids.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(PageId, f32)> = self
            .flat
            .chunks_exact(self.dims)
            .zip(&self.ids)
            .map(|(vector, id)| {
                let dot: f32 = vector.iter().zip(query).map(|(a, b)| a * b).sum();
                (id.clone(), dot)
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.0.cmp(&b.0))
        });
        scored.truncate(limit);
        scored
    }
}

/// How far down a ranking still counts. The usual constant for this fusion.
const RRF_K: f64 = 60.0;

/// Merge two rankings into one.
///
/// Decision 85 says "fuse the two **rankings**", and that is literally this:
/// reciprocal rank fusion adds `1 / (k + rank)` across the lists. Ranks rather
/// than scores, because BM25 and a cosine are not on the same scale and
/// normalising them would mean inventing a conversion and tuning it.
pub fn fuse(lexical: &[Hit], semantic: &[(PageId, f32)], limit: usize) -> Vec<Hit> {
    let mut scores: HashMap<PageId, (f64, bool)> = HashMap::new();
    for (rank, hit) in lexical.iter().enumerate() {
        let entry = scores.entry(hit.id.clone()).or_insert((0.0, hit.by_link));
        entry.0 += 1.0 / (RRF_K + rank as f64 + 1.0);
        entry.1 &= hit.by_link;
    }
    for (rank, (id, _)) in semantic.iter().enumerate() {
        let entry = scores.entry(id.clone()).or_insert((0.0, false));
        entry.0 += 1.0 / (RRF_K + rank as f64 + 1.0);
        // Matched by meaning, so not "reached only through a link".
        entry.1 = false;
    }
    let mut fused: Vec<Hit> = scores
        .into_iter()
        .map(|(id, (score, by_link))| Hit { id, score, by_link })
        .collect();
    fused.sort_by(|a, b| {
        b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });
    fused.truncate(limit);
    fused
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A vectoriser with no model: each text becomes a bag of its words.
    ///
    /// The reason this exists is not convenience. The whole retrieval pipeline
    /// — fingerprints, staleness, the scan, the fusion — is testable
    /// **deterministically and without downloading a gigabyte**, which is what
    /// keeps these tests in `cargo test` instead of behind `#[ignore]` with the
    /// three that load real weights.
    pub struct Bag {
        pub id: String,
        /// The vocabulary, in a fixed order: one dimension per word.
        pub vocabulary: Vec<String>,
        /// Prefixes seen, so a test can prove they were applied.
        pub seen: std::sync::Mutex<Vec<String>>,
    }

    impl Bag {
        pub fn new(words: &[&str]) -> Self {
            Self {
                id: "bag/1".into(),
                vocabulary: words.iter().map(|w| (*w).to_owned()).collect(),
                seen: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl Embedder for Bag {
        fn id(&self) -> &str {
            &self.id
        }

        fn dims(&self) -> usize {
            self.vocabulary.len()
        }

        fn embed(&self, texts: &[&str], kind: Text) -> Result<Vec<Vec<f32>>, EmbedError> {
            let prefix = match kind {
                Text::Query => "query: ",
                Text::Passage => "passage: ",
            };
            let mut out = Vec::new();
            for text in texts {
                let prefixed = format!("{prefix}{text}");
                self.seen.lock().unwrap().push(prefixed.clone());
                let lowered = prefixed.to_lowercase();
                let mut vector: Vec<f32> = self
                    .vocabulary
                    .iter()
                    .map(|word| if lowered.contains(word) { 1.0 } else { 0.0 })
                    .collect();
                let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
                if norm > 0.0 {
                    for v in &mut vector {
                        *v /= norm;
                    }
                }
                out.push(vector);
            }
            Ok(out)
        }
    }

    fn page(id: &str, title: &str, keywords: &[&str], resume: &str) -> Page {
        let mut p = Page::new(PageId::new(id).expect("id"), "test", title);
        p.front.keywords = keywords.iter().map(|k| (*k).to_owned()).collect();
        p.body = format!("## Resume\n{resume}\n");
        p
    }

    #[test]
    fn the_prefix_is_applied_by_the_embedder_so_a_caller_cannot_forget_it() {
        let bag = Bag::new(&["cafe"]);
        bag.embed(&["du café"], Text::Passage).expect("embed");
        bag.embed(&["du café"], Text::Query).expect("embed");
        let seen = bag.seen.lock().unwrap().clone();
        assert_eq!(seen, ["passage: du café", "query: du café"]);
    }

    #[test]
    fn a_page_needs_embedding_once_and_then_does_not() {
        let bag = Bag::new(&["cafe", "moulin"]);
        let mut vectors = Vectors::open(None, bag.dims()).expect("open");
        let pages = vec![page("profil/louis", "Louis", &["cafe"], "Café serré.")];

        assert_eq!(vectors.stale(&pages, bag.id()).len(), 1);
        let text = passage_text(&pages[0]);
        let vector = bag.embed(&[text.as_str()], Text::Passage).expect("embed").remove(0);
        vectors.put(&pages[0], bag.id(), &vector).expect("put");
        assert!(vectors.stale(&pages, bag.id()).is_empty(), "rien ne doit rester à faire");
    }

    /// Decision 92's whole mechanism in one test: the trigger is the state of
    /// the file, so a page changed by **anyone** — us, a delegated session, the
    /// user in an editor — comes back as work to do.
    #[test]
    fn a_page_changed_by_anyone_becomes_work_again() {
        let bag = Bag::new(&["cafe", "the"]);
        let mut vectors = Vectors::open(None, bag.dims()).expect("open");
        let mut pages = vec![page("profil/louis", "Louis", &["cafe"], "Café serré.")];
        let text = passage_text(&pages[0]);
        let vector = bag.embed(&[text.as_str()], Text::Passage).expect("embed").remove(0);
        vectors.put(&pages[0], bag.id(), &vector).expect("put");

        pages[0].body = "## Resume\nIl est passé au thé.\n".into();
        assert_eq!(vectors.stale(&pages, bag.id()).len(), 1, "le contenu a changé");
    }

    /// Changing model must invalidate everything, detectably — two models'
    /// vectors in one space give wrong similarities, not an error.
    #[test]
    fn changing_model_invalidates_every_vector() {
        let bag = Bag::new(&["cafe"]);
        let mut vectors = Vectors::open(None, bag.dims()).expect("open");
        let pages = vec![page("profil/louis", "Louis", &["cafe"], "Café serré.")];
        let vector = bag
            .embed(&[passage_text(&pages[0]).as_str()], Text::Passage)
            .expect("embed")
            .remove(0);
        vectors.put(&pages[0], bag.id(), &vector).expect("put");

        assert!(vectors.stale(&pages, "bag/1").is_empty());
        assert_eq!(vectors.stale(&pages, "e5-base/1").len(), 1, "autre modèle, tout à refaire");
    }

    #[test]
    fn the_scan_finds_the_nearest_page_and_says_how_much_it_costs() {
        let bag = Bag::new(&["cafe", "moulin", "voiture"]);
        let mut vectors = Vectors::open(None, bag.dims()).expect("open");
        let pages = vec![
            page("profil/louis", "Louis", &["cafe"], "Café serré."),
            page("garage/auto", "Auto", &["voiture"], "Une voiture grise."),
        ];
        for p in &pages {
            let vector =
                bag.embed(&[passage_text(p).as_str()], Text::Passage).expect("embed").remove(0);
            vectors.put(p, bag.id(), &vector).expect("put");
        }
        vectors.refresh().expect("refresh");

        let query = bag.embed(&["cafe"], Text::Query).expect("embed").remove(0);
        let near = vectors.nearest(&query, 2);
        assert_eq!(near[0].0.as_str(), "profil/louis");
        assert!(near[0].1 > near[1].1);
        // Decision 84's arithmetic, asserted rather than asserted about.
        assert_eq!(vectors.bytes(), 2 * 3 * 4);
    }

    #[test]
    fn a_vector_whose_page_is_gone_is_an_orphan() {
        let bag = Bag::new(&["cafe"]);
        let mut vectors = Vectors::open(None, bag.dims()).expect("open");
        let pages = vec![page("profil/louis", "Louis", &["cafe"], "Café.")];
        let vector = bag
            .embed(&[passage_text(&pages[0]).as_str()], Text::Passage)
            .expect("embed")
            .remove(0);
        vectors.put(&pages[0], bag.id(), &vector).expect("put");
        vectors.refresh().expect("refresh");

        assert!(vectors.orphans(&pages).is_empty());
        assert_eq!(vectors.orphans(&[]).len(), 1);
    }

    /// The point of fusing rankings rather than scores: a page each half ranks
    /// second comes out above a page only one half saw at all.
    #[test]
    fn fusion_rewards_a_page_both_halves_agree_on() {
        let lexical = vec![
            Hit { id: PageId::new("a").unwrap(), score: 9.0, by_link: false },
            Hit { id: PageId::new("b").unwrap(), score: 3.0, by_link: false },
        ];
        let semantic = vec![
            (PageId::new("c").unwrap(), 0.9),
            (PageId::new("b").unwrap(), 0.8),
        ];
        let fused = fuse(&lexical, &semantic, 3);
        assert_eq!(fused[0].id.as_str(), "b", "les deux moitiés sont d'accord sur b");
    }

    #[test]
    fn the_vector_cache_survives_being_closed_and_reopened() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("index").join("vectors.db");
        let bag = Bag::new(&["cafe", "moulin"]);
        let pages = vec![page("profil/louis", "Louis", &["cafe"], "Café serré.")];

        {
            let mut vectors = Vectors::open(Some(&path), bag.dims()).expect("open");
            let vector = bag
                .embed(&[passage_text(&pages[0]).as_str()], Text::Passage)
                .expect("embed")
                .remove(0);
            vectors.put(&pages[0], bag.id(), &vector).expect("put");
        }

        let reopened = Vectors::open(Some(&path), bag.dims()).expect("reopen");
        assert_eq!(reopened.len(), 1);
        assert!(reopened.stale(&pages, bag.id()).is_empty(), "rien à refaire après un redémarrage");
    }
}
