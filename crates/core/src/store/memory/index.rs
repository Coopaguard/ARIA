//! The index: derived, thrown away, rebuilt at start-up.
//!
//! Decision 82 says the index is not the memory, and decision 93 takes that
//! literally for the lexical half — it lives in an **in-memory** SQLite
//! database, rebuilt from the markdown every time the core starts. A few
//! hundred small files cost milliseconds, and in exchange the index can never
//! be stale, can never be corrupt, and never needs a schema migration.
//!
//! The vectors are the other half, and they are the only part worth persisting,
//! because a forward pass per page per boot would be absurd (decision 92).
//! They are not here yet: decision 97 keeps the first slice free of ONNX.
//!
//! # The weights are built from the column list, and that is the point
//!
//! Decision 99 found the trap by running it: `bm25()` takes one weight per
//! **declared** column, and an `UNINDEXED` column still occupies its slot. So
//! `bm25(pages, 10.0, 1.0)` on a three-column table weights `id` and
//! `keywords`, leaves `body` at its default, and the keyword boost silently
//! does nothing at all. Writing the weights out by hand is how that happens, so
//! here they are generated from [`COLUMNS`] — adding a column cannot misalign
//! them. A test still asserts the behaviour, because a generated string can be
//! generated wrongly.

use std::collections::BTreeMap;

use rusqlite::Connection;

use super::page::{Page, PageId};

/// The columns of the full-text table, in declaration order, with the weight
/// `bm25()` gives each one.
///
/// `id` is `UNINDEXED` — it is carried so a hit can name its page, not to be
/// searched — hence a weight of zero. `keywords` is ten times `body` because
/// decision 88 makes it a **declaration** by the page's author of how the page
/// should be found; the body is what the page happens to say.
const COLUMNS: [(&str, f64); 3] = [("id", 0.0), ("keywords", 10.0), ("body", 1.0)];

/// How much a page reached by one link is worth, against a direct hit.
///
/// Decision 85: one hop, with decay, and no personalised PageRank — at this
/// scale a global weighting would solve a problem we do not have.
const HOP_DECAY: f64 = 0.4;

/// A page the index puts forward, and how strongly.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub id: PageId,
    pub score: f64,
    /// `true` when the page was reached through a link rather than matched.
    pub by_link: bool,
}

/// The lexical index and the link graph.
pub struct Index {
    db: Connection,
}

impl Index {
    /// An empty index.
    pub fn new() -> rusqlite::Result<Self> {
        let db = Connection::open_in_memory()?;
        let columns = COLUMNS.iter().map(|(name, _)| *name).collect::<Vec<_>>();
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE pages USING fts5(
                 {} UNINDEXED, {}, {},
                 tokenize = \"unicode61 remove_diacritics 2\"
             );
             CREATE TABLE names(name TEXT PRIMARY KEY, id TEXT NOT NULL);
             CREATE TABLE edges(src TEXT NOT NULL, dst TEXT NOT NULL);
             CREATE INDEX edges_src ON edges(src);
             CREATE INDEX edges_dst ON edges(dst);",
            columns[0], columns[1], columns[2]
        ))?;
        Ok(Self { db })
    }

    /// Build the whole index from the pages on disk.
    pub fn rebuild(&mut self, pages: &[Page]) -> rusqlite::Result<()> {
        let tx = self.db.transaction()?;
        tx.execute_batch("DELETE FROM pages; DELETE FROM names; DELETE FROM edges;")?;
        {
            let mut page = tx.prepare("INSERT INTO pages(id, keywords, body) VALUES (?1, ?2, ?3)")?;
            let mut name = tx.prepare("INSERT OR REPLACE INTO names(name, id) VALUES (?1, ?2)")?;
            let mut edge = tx.prepare("INSERT INTO edges(src, dst) VALUES (?1, ?2)")?;
            for p in pages {
                // The title joins the keywords rather than the body: a page is
                // very often looked for by what it is called, and that is a
                // declaration too.
                let keywords = format!("{} {}", p.front.title, p.front.keywords.join(" "));
                page.execute(rusqlite::params![p.id.as_str(), keywords, p.body])?;
                for n in p.names() {
                    name.execute(rusqlite::params![n, p.id.as_str()])?;
                }
                for target in p.links() {
                    edge.execute(rusqlite::params![p.id.as_str(), target.as_str()])?;
                }
            }
        }
        tx.commit()
    }

    /// Resolve a name to a page: its id, or any alias a rename left behind.
    ///
    /// The alias half is what decision 98 added and decision 97 made necessary
    /// — the model names the page in `remember`, and it may name the id it saw
    /// before a rewrite changed it.
    pub fn resolve(&self, name: &str) -> rusqlite::Result<Option<PageId>> {
        let found: Option<String> = self
            .db
            .query_row("SELECT id FROM names WHERE name = ?1", [name], |row| row.get(0))
            .ok();
        Ok(found.and_then(|id| PageId::new(id).ok()))
    }

    /// The fast path: an id, an alias, or a few keywords (decision 89).
    ///
    /// Lexical only. It needs no model, so it is the one that always answers —
    /// and when the semantic half is absent, this is the whole of retrieval
    /// rather than a failure.
    pub fn search(&self, query: &str, limit: usize) -> rusqlite::Result<Vec<Hit>> {
        if let Some(id) = self.resolve(query.trim())? {
            return Ok(vec![Hit { id, score: f64::INFINITY, by_link: false }]);
        }
        let Some(expression) = match_expression(query) else { return Ok(Vec::new()) };

        let mut direct: BTreeMap<String, f64> = BTreeMap::new();
        {
            let mut statement = self.db.prepare(&format!(
                "SELECT id, bm25(pages, {}) FROM pages WHERE pages MATCH ?1
                 ORDER BY bm25(pages, {}) LIMIT ?2",
                weights(),
                weights()
            ))?;
            let rows = statement.query_map(
                rusqlite::params![expression, limit as i64 * 2],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
            )?;
            for row in rows {
                let (id, bm25) = row?;
                // bm25 is negative, better being more negative. Flipped here so
                // everything above this line compares scores the obvious way.
                direct.insert(id, -bm25);
            }
        }

        let mut scores = direct.clone();
        let mut reached_by_link = Vec::new();
        {
            let mut neighbours = self
                .db
                .prepare("SELECT dst FROM edges WHERE src = ?1 UNION SELECT src FROM edges WHERE dst = ?1")?;
            for (id, score) in &direct {
                let rows = neighbours.query_map([id], |row| row.get::<_, String>(0))?;
                for row in rows {
                    let neighbour = row?;
                    if direct.contains_key(&neighbour) {
                        continue;
                    }
                    let hopped = score * HOP_DECAY;
                    let entry = scores.entry(neighbour.clone()).or_insert(0.0);
                    if hopped > *entry {
                        *entry = hopped;
                        reached_by_link.push(neighbour);
                    }
                }
            }
        }

        let mut hits: Vec<Hit> = scores
            .into_iter()
            .filter_map(|(id, score)| {
                let by_link = !direct.contains_key(&id);
                PageId::new(id).ok().map(|id| Hit { id, score, by_link })
            })
            .collect();
        hits.sort_by(|a, b| {
            b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        hits.truncate(limit);
        Ok(hits)
    }

    /// Pages one link away, either direction.
    ///
    /// Public because the deep path does its own hop: it fuses two rankings
    /// first (decision 85), and expanding before the fusion would let a page
    /// reached by a link compete as though it had been matched.
    pub fn neighbours(&self, id: &PageId) -> rusqlite::Result<Vec<PageId>> {
        let mut statement = self.db.prepare(
            "SELECT dst FROM edges WHERE src = ?1 UNION SELECT src FROM edges WHERE dst = ?1",
        )?;
        let rows = statement.query_map([id.as_str()], |row| row.get::<_, String>(0))?;
        let mut found = Vec::new();
        for row in rows {
            if let Ok(neighbour) = PageId::new(row?) {
                found.push(neighbour);
            }
        }
        Ok(found)
    }

    /// Pages that link to this one — what a rename has to rewrite (decision 98).
    pub fn inbound(&self, target: &PageId) -> rusqlite::Result<Vec<PageId>> {
        let mut statement = self.db.prepare("SELECT DISTINCT src FROM edges WHERE dst = ?1")?;
        let rows = statement.query_map([target.as_str()], |row| row.get::<_, String>(0))?;
        let mut found = Vec::new();
        for row in rows {
            if let Ok(id) = PageId::new(row?) {
                found.push(id);
            }
        }
        Ok(found)
    }

    /// Links pointing at a page that does not exist — the first thing the lint
    /// of decision 85 owes us.
    pub fn broken_links(&self) -> rusqlite::Result<Vec<(PageId, PageId)>> {
        let mut statement = self.db.prepare(
            "SELECT e.src, e.dst FROM edges e
             LEFT JOIN names n ON n.name = e.dst
             WHERE n.id IS NULL ORDER BY e.src, e.dst",
        )?;
        let rows = statement
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        let mut found = Vec::new();
        for row in rows {
            let (src, dst) = row?;
            if let (Ok(src), Ok(dst)) = (PageId::new(src), PageId::new(dst)) {
                found.push((src, dst));
            }
        }
        Ok(found)
    }
}

/// `0.0, 10.0, 1.0` — one weight per declared column, generated.
fn weights() -> String {
    COLUMNS.iter().map(|(_, w)| format!("{w:.1}")).collect::<Vec<_>>().join(", ")
}

/// Turn free text into an FTS5 `MATCH` expression.
///
/// FTS5's match syntax is a small language: `AND`, `NEAR`, `*`, `^`, and
/// punctuation that is a syntax error rather than a term. A spoken query goes
/// through here — "c'est quoi ses préférences ?" — so every term is quoted and
/// the terms are OR'd. OR rather than AND because this path exists to **find**
/// something; `bm25` then does the ranking, which is what it is for.
fn match_expression(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|term| term.chars().count() > 1)
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(id: &str, title: &str, keywords: &[&str], body: &str) -> Page {
        let mut p = Page::new(PageId::new(id).expect("id"), "test", title);
        p.front.keywords = keywords.iter().map(|k| (*k).to_owned()).collect();
        p.body = body.to_owned();
        p
    }

    fn index(pages: &[Page]) -> Index {
        let mut index = Index::new().expect("index");
        index.rebuild(pages).expect("rebuild");
        index
    }

    /// **The guard of decision 99.** It does not check that `bm25` exists; it
    /// checks that our weights are aligned with our columns. A decoy page says
    /// the word four times in its body; the page that *declares* it as a
    /// keyword must still win. Written `0.0, 10.0, 1.0` it passes, written
    /// `10.0, 1.0` — one weight short, the shape that looks right — the decoy
    /// wins and nothing anywhere reports an error.
    #[test]
    fn a_declared_keyword_outranks_the_same_word_repeated_in_a_body() {
        let index = index(&[
            page("profil/louis", "Louis", &["café", "préférences"], "Rien d'autre à dire."),
            page("leurre/bruit", "Bruit", &[], "café café café café"),
        ]);
        let hits = index.search("café", 5).expect("search");
        assert_eq!(hits.first().map(|h| h.id.as_str()), Some("profil/louis"));
        assert_eq!(weights(), "0.0, 10.0, 1.0");
    }

    /// Decision 93's reason for SQLite over a hand-rolled BM25.
    #[test]
    fn an_unaccented_query_finds_an_accented_page() {
        let index = index(&[page("profil/louis", "Louis", &["préférences"], "Son café est serré.")]);
        for query in ["preferences", "préférences", "PREFERENCES"] {
            let hits = index.search(query, 5).expect("search");
            assert_eq!(hits.len(), 1, "{query:?} n'a rien trouvé");
        }
    }

    #[test]
    fn an_id_or_an_alias_answers_directly() {
        let mut aliased = page("profil/louis", "Louis", &[], "corps");
        aliased.front.aliases.push("profil/utilisateur".into());
        let index = index(&[aliased]);

        for name in ["profil/louis", "profil/utilisateur"] {
            let hits = index.search(name, 5).expect("search");
            assert_eq!(hits.first().map(|h| h.id.as_str()), Some("profil/louis"), "{name}");
            assert!(!hits[0].by_link);
        }
        assert_eq!(index.resolve("profil/inconnu").expect("resolve"), None);
    }

    /// Decision 85's one hop: a page that says nothing about the query can
    /// still be worth answering with, if the page that does points at it.
    #[test]
    fn one_hop_along_a_link_reaches_a_page_the_words_do_not() {
        let index = index(&[
            page("profil/louis", "Louis", &["café"], "Voir [[procedures/moulin]]."),
            page("procedures/moulin", "Moulin", &[], "Réglage fin, douze secondes."),
        ]);
        let hits = index.search("café", 5).expect("search");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].id.as_str(), "profil/louis");
        assert_eq!(hits[1].id.as_str(), "procedures/moulin");
        assert!(hits[1].by_link, "la seconde page a été atteinte par un lien");
        assert!(hits[1].score < hits[0].score, "le saut doit décroître");
    }

    /// A spoken query is not a query language. This is the one that would make
    /// `recall` return an error in the middle of a turn.
    #[test]
    fn a_spoken_query_with_punctuation_does_not_become_a_syntax_error() {
        let index = index(&[page("profil/louis", "Louis", &["café"], "corps")]);
        for query in [
            "c'est quoi ses préférences ?",
            "café *",
            "NEAR(a b)",
            "\"guillemets\" et (parenthèses)",
            "AND OR NOT",
        ] {
            index.search(query, 5).unwrap_or_else(|e| panic!("{query:?} a échoué : {e}"));
        }
        assert!(index.search("   ", 5).expect("vide").is_empty());
        assert!(index.search("", 5).expect("vide").is_empty());
    }

    #[test]
    fn the_lint_sees_a_link_to_a_page_that_is_not_there() {
        let index = index(&[page("profil/louis", "Louis", &[], "Voir [[procedures/absente]].")]);
        let broken = index.broken_links().expect("lint");
        assert_eq!(broken.len(), 1);
        assert_eq!(broken[0].1.as_str(), "procedures/absente");
    }

    #[test]
    fn inbound_links_are_what_a_rename_must_rewrite() {
        let index = index(&[
            page("profil/louis", "Louis", &[], "Voir [[projets/aria]]."),
            page("veille/sujets", "Sujets", &[], "Et [[projets/aria]] aussi."),
            page("projets/aria", "ARIA", &[], "corps"),
        ]);
        let mut inbound = index
            .inbound(&PageId::new("projets/aria").expect("id"))
            .expect("inbound");
        inbound.sort();
        assert_eq!(
            inbound.iter().map(PageId::as_str).collect::<Vec<_>>(),
            ["profil/louis", "veille/sujets"]
        );
    }
}
