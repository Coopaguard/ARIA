//! The vectoriser, and that is the whole crate.
//!
//! # Why `multilingual-e5-base`
//!
//! Decision 96, and it corrected the default decision 83 had assumed. The only
//! third-party measurement found with French **broken out per task** is the
//! Granite paper's appendix C, table 12 — worth trusting because it runs
//! against its authors' interest, their own models placing below:
//!
//! | nDCG@10 | Alloprof | BSARD | Syntec | FQuAD | XPQA | avg |
//! |---|---|---|---|---|---|---|
//! | multilingual-e5-base | 34.4 | 18.8 | 82.9 | 82.5 | 59.5 | 55.6 |
//! | granite-278m-multilingual | 44.7 | 16.4 | 81.1 | 70.1 | 58.2 | 54.1 |
//! | multilingual-e5-small | 27.4 | 14.5 | 73.5 | 78.8 | 57.2 | 50.3 |
//!
//! The useful part is not the ranking, it is the spread across columns: one
//! model goes from 14 to 83 depending on the task, so the average means
//! nothing for us. BSARD is legal retrieval; **Syntec is a small corpus of
//! short French documents queried in a few words**, which is our case, and
//! `e5-base` leads it by 9.4 points over `e5-small`. MIT licensed, and not
//! gated on the Hub.
//!
//! `EmbeddingGemma-300M` remains the one to measure against it: 197 MB in q4
//! against roughly 1.1 GB for this one, with no published French figure and the
//! Gemma licence rather than MIT. The bench that settles it is thirty
//! hand-written question-page pairs — not a leaderboard.
//!
//! # The prefix is the trap, so it lives here
//!
//! The E5 family requires `query:` and `passage:`. Omitting them degrades
//! retrieval **without breaking anything visible**, which is the worst shape a
//! defect can take. [`aria_core::store::memory::semantic::Text`] therefore
//! carries the side of the retrieval and the prefix is applied in here, so a
//! caller cannot forget it — only mislabel, which a test can catch.
//!
//! It is also why the prefix convention is part of [`Embedder::id`]: change it
//! and every stored vector has to be remade, which decision 92's fingerprint
//! does on its own as soon as the id moves.

use std::sync::Mutex;

use aria_core::store::memory::semantic::{EmbedError, Embedder, Text};
use fastembed::{EmbeddingModel, TextEmbedding, TextInitOptions};

/// The model, its dimension, and the prefix convention, as one identity.
///
/// Bumped by hand whenever any of the three changes — a different checkpoint, a
/// different quantisation, a different prefix — because that is what makes
/// decision 92 re-embed everything instead of mixing two spaces and producing
/// wrong similarities rather than an error.
const MODEL_ID: &str = "intfloat/multilingual-e5-base+e5prefix/1";

/// What the checkpoint produces. Asserted against the model at load time, so a
/// wrong constant cannot sit here unnoticed.
const DIMS: usize = 768;

/// `multilingual-e5-base`, in this process, on the CPU.
pub struct E5Base {
    /// `TextEmbedding::embed` takes `&mut self`, and [`Embedder`] hands out
    /// `&self` so the memory can hold it behind an `Arc`. Hence the lock — and
    /// it is never held across an await, because nothing here is async.
    inner: Mutex<TextEmbedding>,
}

impl E5Base {
    /// Load the model, downloading the weights on first use.
    ///
    /// `cache` is where the weights are kept — give it the same folder the
    /// other models use, so one directory holds everything a fresh machine has
    /// to fetch.
    ///
    /// `environment` is the process's one ONNX Runtime environment, which `app/`
    /// takes from `aria_voice::asr::environment`. Without it fastembed builds
    /// its own, and `ort` allows **one** per process: whichever of the memory
    /// and the ear started second was refused. It used to be the memory, by
    /// luck of timing; on 2026-11-18 the ear without Parakeet (epic 10) started
    /// faster, lost the race, and ARIA heard nothing at all.
    pub fn load(
        cache: Option<std::path::PathBuf>,
        environment: Option<ort::environment::Environment>,
    ) -> anyhow::Result<Self> {
        let mut options = TextInitOptions::new(EmbeddingModel::MultilingualE5Base);
        if let Some(cache) = cache {
            options = options.with_cache_dir(cache);
        }
        if let Some(environment) = environment {
            options = options.with_environment(environment);
        }
        let started = std::time::Instant::now();
        let mut model = TextEmbedding::try_new(options)?;

        // One forward pass to find out the real width, rather than trusting
        // `DIMS`. A dimension mismatch would not fail: it would store vectors
        // the scan silently drops, and retrieval would just quietly stop
        // working.
        let probe = model.embed(vec!["passage: test"], None)?;
        let dims = probe.first().map(Vec::len).unwrap_or(0);
        anyhow::ensure!(
            dims == DIMS,
            "le modèle rend {dims} dimensions, {DIMS} étaient attendues"
        );
        tracing::info!(model = MODEL_ID, dims, ms = started.elapsed().as_millis(), "vectoriseur prêt");

        Ok(Self { inner: Mutex::new(model) })
    }
}

impl Embedder for E5Base {
    fn id(&self) -> &str {
        MODEL_ID
    }

    fn dims(&self) -> usize {
        DIMS
    }

    fn embed(&self, texts: &[&str], kind: Text) -> Result<Vec<Vec<f32>>, EmbedError> {
        let prefixed: Vec<String> = texts.iter().map(|text| prefixed(text, kind)).collect();
        let mut model = self.inner.lock().map_err(|_| EmbedError("verrou empoisonné".into()))?;
        let mut vectors = model
            .embed(prefixed, None)
            .map_err(|e| EmbedError(format!("l'inférence a échoué : {e}")))?;
        // Normalised here rather than assumed: the scan treats a dot product
        // as a cosine (decision 84), and that only holds for unit vectors.
        for vector in &mut vectors {
            normalise(vector);
        }
        Ok(vectors)
    }
}

/// The E5 prefix, by side of the retrieval.
///
/// A free function so the convention is testable without loading 1.1 GB of
/// weights — which is the difference between a test that runs on every
/// `cargo test` and one behind `#[ignore]`.
fn prefixed(text: &str, kind: Text) -> String {
    match kind {
        Text::Query => format!("query: {text}"),
        Text::Passage => format!("passage: {text}"),
    }
}

fn normalise(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in vector {
            *v /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Where the two real-model tests look for the weights.
    ///
    /// The same folder the product uses — `%LOCALAPPDATA%/aria/models/memory`,
    /// decision 104 — so that downloading a gigabyte once serves both. The
    /// product resolves it through `aria_voice::models_root()`, which this crate
    /// deliberately does not depend on; mirroring one path in a test is the
    /// cheaper of the two prices, and `ARIA_MEMORY_MODEL` overrides it.
    /// Loaded once for the whole test binary.
    ///
    /// Not a nicety: run in parallel, the two real-model tests each start their
    /// own download of the same file and the second one dies on
    /// `Lock acquisition failed` — observed on 2026-11-11, on the very first
    /// run. A `OnceLock` fixes the race and halves the time, where
    /// `--test-threads=1` would only have hidden it behind a flag someone has
    /// to remember.
    fn model() -> &'static E5Base {
        static MODEL: std::sync::OnceLock<E5Base> = std::sync::OnceLock::new();
        MODEL.get_or_init(|| E5Base::load(weights(), None).expect("chargement"))
    }

    fn weights() -> Option<PathBuf> {
        if let Ok(dir) = std::env::var("ARIA_MEMORY_MODEL") {
            return Some(PathBuf::from(dir));
        }
        let base = std::env::var("LOCALAPPDATA").or_else(|_| std::env::var("HOME")).ok()?;
        Some(PathBuf::from(base).join("aria").join("models").join("memory"))
    }

    #[test]
    fn the_prefixes_are_the_ones_the_model_card_requires() {
        assert_eq!(prefixed("du café", Text::Query), "query: du café");
        assert_eq!(prefixed("du café", Text::Passage), "passage: du café");
    }

    /// The prefix convention is part of the model's identity, so that changing
    /// it invalidates every stored vector through decision 92's fingerprint
    /// rather than silently mixing two conventions in one space.
    #[test]
    fn the_identity_names_the_prefix_convention_and_not_just_the_model() {
        assert!(MODEL_ID.contains("multilingual-e5-base"));
        assert!(MODEL_ID.contains("prefix"), "la convention de préfixe fait partie de l'identité");
    }

    #[test]
    fn normalising_makes_a_dot_product_a_cosine() {
        let mut vector = vec![3.0, 4.0];
        normalise(&mut vector);
        let length: f32 = vector.iter().map(|v| v * v).sum();
        assert!((length - 1.0).abs() < 1e-6, "longueur {length}");
        // And a zero vector is left alone rather than turned into NaNs.
        let mut zero = vec![0.0, 0.0];
        normalise(&mut zero);
        assert_eq!(zero, [0.0, 0.0]);
    }

    /// The real model. `#[ignore]` for the reason the other three weight tests
    /// are: it downloads several hundred megabytes and takes time. It is the
    /// only thing that proves the French side actually works, so it is written
    /// as a recall check and not as "a vector came out".
    ///
    /// `cargo test -p aria-memory -- --ignored --nocapture`
    #[test]
    #[ignore = "télécharge les poids du modèle"]
    fn the_real_model_puts_the_right_french_page_first() {
        let model = model();
        assert_eq!(model.dims(), DIMS);

        let passages = [
            "Il prend son café serré, sans sucre, le matin.",
            "La voiture grise est garée au sous-sol.",
            "Le moulin se règle sur douze secondes.",
        ];
        let embedded = model.embed(&passages, Text::Passage).expect("passages");
        let query = model
            .embed(&["quelle boisson aime-t-il"], Text::Query)
            .expect("requête")
            .remove(0);

        let scores: Vec<f32> = embedded
            .iter()
            .map(|p| p.iter().zip(&query).map(|(a, b)| a * b).sum())
            .collect();
        println!("scores : {scores:?}");
        let best = scores
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .expect("un maximum");
        assert_eq!(best, 0, "« boisson » doit ramener le café : {scores:?}");
    }

    /// What one write costs, now that decision 103 put a forward pass on it.
    ///
    /// The number this prints is the one that decision matters on: a page is
    /// one passage, and if it were not tens of milliseconds but hundreds, the
    /// write would have to go back to the silence. Measured and not assumed,
    /// which is the whole reason this test exists rather than a comment.
    ///
    /// `cargo test -p aria-memory speed -- --ignored --nocapture`
    #[test]
    #[ignore = "télécharge les poids du modèle"]
    fn speed_of_one_passage_is_what_a_write_now_pays() {
        let model = model();
        let passage =
            "Louis café moulin Il prend son café serré, sans sucre, et règle son moulin à douze secondes.";

        // One pass to warm whatever the runtime warms, then ten measured.
        model.embed(&[passage], Text::Passage).expect("chauffe");
        let start = std::time::Instant::now();
        for _ in 0..10 {
            model.embed(&[passage], Text::Passage).expect("passe");
        }
        let each = start.elapsed() / 10;
        println!("une passe avant sur un passage : {each:?}");
        assert!(
            each < std::time::Duration::from_millis(400),
            "une écriture paierait {each:?} : c'est l'ordre de grandeur qui remettrait 
             la vectorisation au silence"
        );
    }
}
