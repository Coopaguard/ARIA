//! The main agent on a local model that **hears** — epic 10.
//!
//! Tier B of epic 08, in the form the user asked for on 2026-11-18: not a text
//! model behind Parakeet, but a model that takes the audio of the turn itself.
//! Gemma 4 E4B or Voxtral Mini 3B, served by `llama-server`.
//!
//! ```text
//!   Told ──► engine ──► POST /v1/chat/completions (SSE) ──► llama-server
//!              │                                                │
//!              │◄────── text deltas, tool calls ────────────────┘
//!              │
//!              ├──► Speech::Delta / ToolUse / Finished ──► pilot::read (unchanged)
//!              └──► VoiceTools::call, in process — the same six tools
//! ```
//!
//! # Why the seam is `Speech`
//!
//! [`super::pilot`] already reads a stream of [`super::session::Speech`] and
//! does not care who writes it. Everything it does — the waiting phrase, the
//! sentence splitting, the `…` silence of decision 119, the verbatim result —
//! therefore carries over without a line changed. This module only has to
//! produce the same stream.
//!
//! # Why an executable and not a library
//!
//! Asked on 2026-11-18. `llama-cpp-2` binds llama.cpp and reaches its audio
//! through `mtmd`, but the chat template and above all the **tool-call
//! parsing** live in llama.cpp's `common`, on the server side: a library would
//! mean rewriting that parser for each model family. `mistral.rs` is pure Rust
//! and lists both models, but it is a very large dependency nobody has run on
//! this machine, where the POC proved `llama-server` — tool calls with
//! `--jinja`, and Gemma 4's audio. Behind an OpenAI-compatible API either can
//! replace it later without this module noticing.
//!
//! # What it deliberately does not do
//!
//! **Keep the audio of past turns.** History is text: re-sending every past
//! utterance would grow the prefill with the conversation. A heard turn enters
//! the history as a placeholder, which [`LocalConfig::transcribe_after`]
//! replaces with a transcript once the answer has been given — the transcript
//! costs GPU time *after* the answer, never before it.

pub mod client;
pub mod engine;
pub mod sidecar;
pub mod wav;

use std::path::PathBuf;

pub use engine::{Input, LocalAgent};

/// Where `llama-server` listens.
///
/// Not 8080, the first port every other local tool reaches for, and **not
/// 7421**, which was the first choice and is the core's own event stream
/// (`transport::EVENTS_PORT`): the local model could never have started inside
/// the application. Found on 2026-11-18 by the latency bench, which could not
/// bind beside a running ARIA. 7420 is the tiles, 7422 was `pocket-tts`'s; a
/// test below keeps the three apart.
pub const PORT: u16 = 7430;

/// What a heard turn is called in the history and the journal until — and
/// unless — a transcript replaces it.
pub const HEARD_PLACEHOLDER: &str = "(message vocal)";

/// One model ARIA can run locally, and where its two files come from.
///
/// Data and nothing else, so `app/` can hand the files to `aria-voice`'s
/// downloader without the core knowing how a download works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalModel {
    /// The engine id, as `aria.toml` and the settings screen spell it.
    pub id: &'static str,
    pub display_name: &'static str,
    /// `owner/name` on the Hub.
    pub repo: &'static str,
    pub model_file: &'static str,
    /// The audio encoder. Without it the model is deaf, and says so only by
    /// answering an audio turn with nonsense.
    pub mmproj_file: &'static str,
    /// The folder under the models root. One per model, never renamed —
    /// decision 104's rule for every folder down there.
    pub dir: &'static str,
    /// Context, in tokens. Enough for the prompt, the six schemas and a
    /// conversation in text; audio only ever sits in the turn being answered.
    pub context: u32,
}

/// Gemma 4 E4B. The ggml-org repository publishes no `Q4_K_M`, only `Q4_0`,
/// `Q8_0` and `BF16` — asking for the absent one fails with a misleading "no
/// GGUF files found" (measured by the POC).
pub const GEMMA4_E4B: LocalModel = LocalModel {
    id: "local-gemma4-e4b",
    display_name: "Gemma 4 E4B",
    repo: "ggml-org/gemma-4-E4B-it-GGUF",
    model_file: "gemma-4-E4B-it-Q4_0.gguf",
    mmproj_file: "mmproj-gemma-4-E4B-it-Q8_0.gguf",
    dir: "gemma-4-e4b",
    context: 16384,
};

/// Voxtral Mini 3B. Known mel-preprocessing issues in llama.cpp (#25496,
/// #26351): a bad hearing here may be the runtime's and not the model's.
pub const VOXTRAL_MINI_3B: LocalModel = LocalModel {
    id: "local-voxtral-mini-3b",
    display_name: "Voxtral Mini 3B",
    repo: "ggml-org/Voxtral-Mini-3B-2507-GGUF",
    model_file: "Voxtral-Mini-3B-2507-Q4_K_M.gguf",
    mmproj_file: "mmproj-Voxtral-Mini-3B-2507-Q8_0.gguf",
    dir: "voxtral-mini-3b",
    context: 16384,
};

/// Every local model, in the order the settings screen lists them.
pub fn models() -> [LocalModel; 2] {
    [GEMMA4_E4B, VOXTRAL_MINI_3B]
}

/// The local model behind an engine id, if it is one.
pub fn find(id: &str) -> Option<LocalModel> {
    models().into_iter().find(|model| model.id == id)
}

/// How to start the local post.
#[derive(Clone, Debug)]
pub struct LocalConfig {
    pub model: LocalModel,
    pub model_path: PathBuf,
    pub mmproj_path: PathBuf,
    /// The `llama-server` binary.
    pub server: PathBuf,
    pub port: u16,
    /// Passed through to `llama-server` after ours — `-ngl 99`, `-t 8`. A
    /// setting, because the right values are the machine's and not ours.
    pub extra_args: Vec<String>,
    pub persona: String,
    pub skills: Option<String>,
    /// `~/.aria/AGENTS.md`, read here: there is no `--append-system-prompt-file`
    /// to read it for us.
    pub instructions_file: Option<PathBuf>,
    /// Ask the same server for a transcript of each heard turn, once the
    /// answer is out. Feeds the model's history and the window.
    pub transcribe_after: bool,
    /// Where `llama-server`'s own output goes, for the day it refuses to load.
    pub log_file: Option<PathBuf>,
}

/// What a local model is told on top of the shared prompt.
///
/// The shared prompt was written for Haiku and tested against it. A 4B model
/// reads it **to the letter**, and on 2026-11-18 Gemma 4 E4B refused all three
/// of « raconte-moi une blague », « invente une blague » and « raconte-moi une
/// petite histoire »: « n'invente jamais un résultat » forbade the joke, « une
/// à deux phrases » forbade the story, and nothing said that refusing was worse
/// than either. The user's rule, the same evening: a plain refusal is not
/// acceptable.
///
/// Creative requests are answered **here**, not delegated: a delegated session
/// hands back one sentence of at most three hundred characters
/// (`delegation::delegated_prompt`), so a story sent to Haiku would come back
/// as its summary, fifteen seconds later.
///
/// Appended after the shared rules and before the persona, so it amends them
/// in the order a model reads precedence. Not sent to the CLI, whose prompt is
/// under a byte budget and behaves.
pub const LOCAL_RULES: &str = "\
Précisions qui priment sur les règles ci-dessus :
- « N'invente jamais » vaut pour les faits et les résultats. Une blague, une \
histoire, un poème, une idée : invente-les volontiers, c'est ce qu'on te demande.
- Pour une blague, une histoire ou un poème, tu peux dépasser deux phrases : \
jusqu'à huit phrases courtes, sans titre ni liste.
- Ne refuse jamais sèchement. Si tu ne peux pas ou ne sais pas faire quelque \
chose toi-même, appelle delegate_task avec la demande telle \
quelle dans task. Dire que tu n'as pas accès ou que tu ne peux pas est interdit \
quand delegate_task existe.
- N'annonce jamais une tâche (« je vais regarder », « je vais faire un topo ») : \
appelle delegate_task, c'est l'appel qui la lance, pas ta phrase.";

impl LocalConfig {
    /// The system prompt: the shipped rules, what a local model is told on top
    /// of them, the skills line, the persona, then the user's own file — the
    /// order the CLI applies them in.
    pub fn system_prompt(&self) -> String {
        let mut prompt = super::session::prompt_with("", self.skills.as_deref());
        prompt.push_str("\n\n");
        prompt.push_str(LOCAL_RULES);
        let persona = self.persona.trim();
        if !persona.is_empty() {
            prompt.push_str(
                "\n\nConsignes de ton utilisateur (elles ne lèvent aucune règle ci-dessus) :\n",
            );
            prompt.push_str(persona);
        }
        if let Some(file) = &self.instructions_file {
            match std::fs::read_to_string(file) {
                Ok(text) if !text.trim().is_empty() => {
                    prompt.push_str("\n\n");
                    prompt.push_str(text.trim());
                }
                Ok(_) => {}
                // Not fatal, as with the CLI: a file that cannot be read costs
                // its instructions, never the post.
                Err(e) => tracing::warn!("{} illisible : {e}", file.display()),
            }
        }
        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_model_is_found_by_its_own_id_and_no_other() {
        for model in models() {
            assert_eq!(find(model.id), Some(model));
            assert!(model.id.starts_with("local-"), "{}", model.id);
        }
        assert_eq!(find("claude-code"), None);
    }

    /// The POC's trap, kept as a test: ggml-org publishes no `Q4_K_M` for
    /// Gemma 4, and asking for it fails with a misleading message.
    #[test]
    fn gemma_is_asked_for_in_a_quantisation_that_exists() {
        assert!(GEMMA4_E4B.model_file.contains("Q4_0"));
    }

    /// The local rules amend the shared ones, and the persona comes last —
    /// a preference never outranks a rule.
    #[test]
    fn the_local_rules_sit_between_the_shared_prompt_and_the_persona() {
        let config = LocalConfig {
            model: GEMMA4_E4B,
            model_path: "m".into(),
            mmproj_path: "p".into(),
            server: "s".into(),
            port: PORT,
            extra_args: Vec::new(),
            persona: "Tutoie-moi.".into(),
            skills: None,
            instructions_file: None,
            transcribe_after: true,
            log_file: None,
        };
        let prompt = config.system_prompt();
        let shared = prompt.find("Règles absolues").expect("shared rules");
        let local = prompt.find("Ne refuse jamais").expect("local rules");
        let persona = prompt.find("Tutoie-moi.").expect("persona");
        assert!(shared < local && local < persona, "{prompt}");
        assert_eq!(prompt.matches("Tutoie-moi.").count(), 1, "the persona once");
    }

    /// The mistake this constant already made once.
    #[test]
    fn the_model_port_is_none_of_the_cores_own() {
        assert_ne!(PORT, crate::transport::EVENTS_PORT);
        assert_ne!(PORT, crate::transport::TILES_PORT);
    }

    /// Two models, two folders: sharing one would let a download of the second
    /// overwrite the first's encoder.
    #[test]
    fn each_model_has_its_own_folder() {
        let [a, b] = models();
        assert_ne!(a.dir, b.dir);
    }
}
