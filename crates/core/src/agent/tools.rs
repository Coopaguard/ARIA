//! What the main agent is allowed to do, and how it is told about it.
//!
//! Two tools, and the shape of each is an argument that was had and settled.
//!
//! # `say_while_waiting` is not here, and that is deliberate
//!
//! The first version of epic 08 put a `say_while_waiting` field on
//! [`DELEGATE_TASK`], so the model could supply the phrase spoken while a task
//! ran. T0 measured that the model's **first text** arrives at 1113 ms median —
//! so by the time that field exists, the moment it was for is long gone. The
//! waiting phrase is local now ([`super::filler`]), and asking the model to
//! write one would cost tokens, latency and a chance to say something wrong.
//!
//! A test below asserts its absence, because a field like that creeps back.
//!
//! # Why the result is not in the tool's answer
//!
//! A tool call is a question that wants an answer now; a delegated task takes
//! seconds or minutes. The tool therefore answers [`journal::PENDING`]
//! immediately and the real result returns later as a user message — the whole
//! of [`super::journal`], and the reason it is tested rather than hoped for.
//!
//! The model has to be told this, or it will wait for the answer in the tool
//! result and say nothing. That is what the description below is for: it is not
//! documentation, it is the instruction that makes the mechanism work.

use serde::{Deserialize, Serialize};

use super::journal;

/// A tool as the MCP server declares it.
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema for the arguments.
    pub schema: serde_json::Value,
}

pub const DELEGATE_TASK: &str = "delegate_task";
pub const RUN_ACTION: &str = "run_action";
pub const RECALL: &str = "recall";
/// The deep half, on the voice post — which decision 89 had deferred.
///
/// Reopened by the user on 2026-11-12 (decision 105) and affordable for a
/// reason 89 did not have: what it priced as expensive was returning **whole
/// pages**, and [`crate::store::Memory::deep_recall`] returns resumes. So the
/// voice post gets the better ranking at the fast path's price, and whole
/// pages stay where they are affordable — a delegated session.
pub const DEEP_RECALL: &str = "deep_recall";
pub const REMEMBER: &str = "remember";
/// The verb from Anthropic's own memory tool, kept deliberately (decision 90):
/// one vocabulary for both readers, and a name models have already seen.
pub const STR_REPLACE: &str = "str_replace";
/// Decision 98 made a rename first-class — links rewritten, former id kept as
/// an alias, history moved. Until decision 104 nothing exposed it, so the one
/// operation with three obligations was the one no agent could perform.
pub const RENAME: &str = "rename_page";

/// Hand work to a session that can take its time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DelegateTask {
    /// What to do, in one sentence, as it would be said to a colleague.
    pub task: String,
    /// What the user said that the task would otherwise lose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Which tier of model should do it — `rapide`, `standard` or `expert`.
    ///
    /// # Why a loose string and not the enum
    ///
    /// Because the value is written by a language model. A
    /// `ModelTier` here would make a misspelling a **deserialisation
    /// failure**, which fails the whole call — so the user loses their answer
    /// to save a token. `crate::providers::ModelTier::from_word` falls back to
    /// standard instead, and the fallback is tested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The project this session belongs to, if it belongs to one.
    ///
    /// Epic 05's one decision about session lifetime, and it is the **agent**
    /// that takes it, at launch. Most delegations are throwaway — the weather
    /// and the news have nothing to do with each other and neither is worth
    /// keeping. What justifies keeping a session is a project: asking ARIA to
    /// prepare a new one opens a coding session that should still be there,
    /// with what it has learnt, when the conversation comes back to it.
    ///
    /// `None` means throwaway: the native id is never stored, and there is
    /// nothing to clean up behind it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// The task this one continues — `t3` — so it goes to **the same
    /// session** instead of a new one.
    ///
    /// Added on 2026-11-23, when one subject explained in several breaths
    /// opened three sessions on the same project, each reading it from
    /// scratch. A loose string for the same reason as `model`: an unknown id
    /// falls back to a new session rather than failing the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follows: Option<String>,
}

impl DelegateTask {
    /// The tier asked for, or standard — and never the fast one.
    ///
    /// Decision 126: a delegated session runs on the middle tier at least.
    /// The fastest model was given the weather on 2026-11-19 and came back
    /// with « pas de prévision fiable ». `rapide` is still read, because a
    /// model writes it from habit, and lands on standard.
    pub fn tier(&self) -> crate::providers::ModelTier {
        use crate::providers::ModelTier;
        match self.model.as_deref().map(ModelTier::from_word) {
            Some(ModelTier::Expert) => ModelTier::Expert,
            _ => ModelTier::Standard,
        }
    }
}

/// Run something the core already knows how to do, here and now.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunAction {
    /// The action's identifier, from the catalogue the core publishes.
    pub action: String,
    /// Its parameters, shaped by that action.
    #[serde(default)]
    pub parameters: serde_json::Value,
}

/// Everything the main agent may call.
pub fn catalogue() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: DELEGATE_TASK,
            // Written at the model, not at a reader. Three things it must know
            // and would otherwise get wrong: that the result comes back later,
            // that it should not announce it is about to work, and that it must
            // not invent the answer in the meantime.
            description: "Confie une tâche à une session qui travaille en arrière-plan : \
                 chercher une information, lire ou modifier des fichiers, lancer une \
                 commande. Le résultat N'EST PAS dans la réponse de cet outil — il \
                 arrivera plus tard sous forme de message. N'annonce pas que tu vas \
                 chercher : c'est déjà dit. N'invente jamais le résultat. \
                 Si la réponse se montre mieux qu'elle ne se dit — un relevé, un \
                 tableau, des chiffres à comparer — demande-le DANS la tâche : \
                 « ... et affiche le résultat dans une tuile ». La session sait le \
                 faire ; toi non. \
                 Modèle : standard par défaut, expert pour du code ou une analyse \
                 longue.",
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "task": {
                        "type": "string",
                        "description": "Ce qu'il faut faire, en une phrase."
                    },
                    "context": {
                        "type": "string",
                        "description": "Ce que l'utilisateur a dit et que la tâche perdrait."
                    },
                    "model": {
                        "type": "string",
                        "enum": ["standard", "expert"],
                        "default": "standard"
                    },
                    "project": {
                        "type": "string",
                        "description": "Nomme le projet si la session doit être reprenable plus tard. Sinon elle est jetable."
                    },
                    "follows": {
                        "type": "string",
                        "description": "Id d'une tâche (t2) que ceci complète : même session."
                    }
                },
                "required": ["task"],
                "additionalProperties": false
            }),
        },
        // The three memory tools. Decision 91 put them in the voice agent's
        // own catalogue rather than behind a delegation, and accepted the
        // seconds that costs — `agent::filler` speaks while they run.
        //
        // Terse on purpose: these descriptions ship on **every turn**, and the
        // guard below bounds their sum rather than each one, which is what
        // actually leaves the machine.
        ToolSpec {
            name: RECALL,
            description: "Retrouve ce qu'ARIA sait de son utilisateur, par mots-clés \
                 ou par identifiant de page. Réponse immédiate. Si rien ne revient, \
                 dis-le : n'invente jamais un souvenir.",
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Quelques mots, ou l'identifiant d'une page."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: DEEP_RECALL,
            // As short as it can be said, because the whole argument for
            // having it here at all is that it costs a hundred bytes a turn
            // rather than a delegation.
            description: "Cherche par le sens plutôt que par mots-clés : plusieurs \
                 pages, un peu plus lent. Utilise-le quand recall n'a rien rendu.",
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Ce que tu cherches, en quelques mots."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: REMEMBER,
            // The nudge of decision 95, in its short form: what qualifies, and
            // its negative half. The long form lives in the memory folder's
            // own `AGENTS.md`, where it costs nothing.
            description: "Retient un fait durable : une préférence, une décision, une \
                 habitude — ce dont l'utilisateur pourrait avoir besoin plus tard. \
                 Donne `page` si tu sais où il va, l'identifiant rendu par recall, \
                 sinon laisse vide. Jamais un secret, jamais ce qui a été entendu \
                 sans être un fait sur lui.",
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "fact": {
                        "type": "string",
                        "description": "Le fait, en une phrase."
                    },
                    "page": {
                        "type": "string",
                        "description": "L'identifiant de la page, si tu le connais."
                    }
                },
                "required": ["fact"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: STR_REPLACE,
            // Substitution rather than rewriting the page, and decision 91 says
            // why in one line: the model never sees the header, so it cannot
            // lose `sources` or `verified`.
            description: "Corrige un souvenir : remplace un fragment exact d'une page \
                 par un autre. Échoue si le fragment n'est pas unique — donne-en \
                 alors un plus long.",
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "page": { "type": "string" },
                    "from": {
                        "type": "string",
                        "description": "Le texte exact à remplacer."
                    },
                    "to": { "type": "string" }
                },
                "required": ["page", "from", "to"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: RENAME,
            // Why a tool of its own rather than letting `str_replace` touch the
            // title: the three obligations of decision 98 are the core's, not
            // the model's. It says the new title; the core rewrites the inbound
            // links, keeps the former id as an alias and moves the history.
            description: "Renomme une page de la mémoire. Les liens qui la citent et \
                 son ancien identifiant continuent de fonctionner. Échoue si une page \
                 porte déjà ce titre.",
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "page": {
                        "type": "string",
                        "description": "L'identifiant actuel, rendu par recall."
                    },
                    "title": {
                        "type": "string",
                        "description": "Le nouveau titre, en clair."
                    }
                },
                "required": ["page", "title"],
                "additionalProperties": false
            }),
        },
        ToolSpec {
            name: RUN_ACTION,
            description: "Exécute une action que le poste sait faire immédiatement. \
                 Le résultat est dans la réponse de cet outil.",
            schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string" },
                    "parameters": { "type": "object" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        },
    ]
}

/// What the main agent is told it is, replacing the CLI's coding-agent prompt.
///
/// Replaced rather than appended: `--system-prompt` swaps the harness prompt
/// out entirely, and `docs/cli/claude-code-cli.md` §8 measured the result at 345 to 438 input
/// tokens for a whole turn. Appending would leave an agent that believes it is
/// editing a repository and answers in Markdown about files.
///
/// Every line is here because its absence produced a specific fault out loud.
pub const SYSTEM_PROMPT: &str = "\
Tu es ARIA, un assistant vocal. Tout ce que tu écris sera PRONONCÉ à voix haute.

Règles absolues :
- Réponds en français, en tutoyant, en une à deux phrases courtes. Ni listes, ni Markdown, ni \
astérisques, ni tirets : ils s'entendent.
- Écris les nombres comme ils se disent.
- Si une information t'est rendue, répète-la telle quelle. Ne la reformule pas, et \
surtout ne change aucun chiffre.
- Si tu ne sais pas, dis-le en une phrase. N'invente jamais un résultat.
- Chaque message commence par la date et l'heure entre crochets : ne les prononce \
jamais, mais sers-t'en pour savoir l'heure et calculer un temps écoulé.
- Tu ne connais de ton interlocuteur que ce que la mémoire d'ARIA te rend. N'utilise \
jamais un nom ni une adresse trouvés ailleurs — un résultat d'outil, un chemin de \
fichier — et n'en déduis rien.
- Réponds toi-même, sans outil, dès que tu sais déjà : l'heure, ce qui a été dit dans \
cette conversation — résultats de tâches compris —, ce que la mémoire t'a rendu.
- Si une phrase ne t'est visiblement pas adressée — des gens qui parlent entre eux, \
une télévision —, réponds seulement « … » : rien ne sera prononcé.
- Toute tâche qui demande de chercher au-dehors, lire, écrire ou exécuter quelque \
chose, même s'il manque un détail qu'elle peut trouver : delegate_task, puis NE DIS RIEN. Pas un mot : une phrase d'attente a \
déjà été prononcée, et une seconde annonce s'entend comme un bégaiement.
- Précision sur une tâche lancée ou sa tuile : follows. Autre sujet, même dans la même phrase : une \
autre tâche.";

/// The immediate answer to a `delegate_task` call.
pub fn pending_answer() -> &'static str {
    journal::PENDING
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The field that cost the first version of this epic its design. It creeps
    /// back, so it is asserted against.
    #[test]
    fn nothing_asks_the_model_for_a_waiting_phrase() {
        for tool in catalogue() {
            let schema = tool.schema.to_string();
            assert!(!schema.contains("say_while_waiting"), "{}", tool.name);
            assert!(!schema.contains("waiting"), "{}", tool.name);
        }
    }

    /// The model has to know the result is not in the tool's answer, or it
    /// waits for it there and says nothing.
    #[test]
    fn delegation_says_the_result_comes_later() {
        let delegate = catalogue()
            .into_iter()
            .find(|t| t.name == DELEGATE_TASK)
            .expect("the catalogue carries delegate_task");
        assert!(delegate.description.contains("plus tard"));
        assert!(delegate.description.contains("N'invente"));
    }

    /// Decided with the user on 2026-10-29: a tile is pushed by the **session**
    /// that did the work, never by the main agent — so the main agent's only
    /// way to get one is to ask for it in the sentence it delegates.
    ///
    /// Asserted here because the request travels in prose and nothing else
    /// carries it: there is no field for it, on purpose, and no system prompt
    /// on the session's side to put it in. If this line goes, ARIA loses the
    /// ability to show anything and nothing else in the code changes.
    #[test]
    fn delegation_is_how_the_main_agent_asks_for_a_tile() {
        let delegate = catalogue()
            .into_iter()
            .find(|t| t.name == DELEGATE_TASK)
            .expect("the catalogue carries delegate_task");
        assert!(delegate.description.contains("tuile"));
        // And it must be told it cannot do it itself, or it will try: the tool
        // is absent from its `--tools`, so the attempt is a refusal read out
        // loud.
        assert!(delegate.description.contains("toi non"));
    }

    /// A tool description is sent on **every turn**, so its length is a
    /// running cost and not a documentation budget.
    ///
    /// `docs/cli/claude-code-cli.md` §8 measured a whole turn at 345–438 input tokens with the
    /// system prompt in place, which is the scale these few hundred characters
    /// are being added to. The bound is here so the next good idea for this
    /// text has to be weighed rather than simply appended.
    /// Revised by decision 91, which found the hole: bounding **each**
    /// description at 800 bytes allows a catalogue of 4 000, and what leaves
    /// the machine every turn is the **sum**. The per-tool bound is kept
    /// because it names which one grew; the sum is what is actually spent.
    ///
    /// 1 600 bytes is roughly four hundred tokens of French, against the
    /// 345–438 that `docs/cli/claude-code-cli.md` §8 measured for a whole turn. That ratio is the
    /// reason these descriptions are terse and the long guidance lives in the
    /// memory folder's `AGENTS.md`, where it is free. The figure wants
    /// re-measuring against the real CLI, which costs subscription quota —
    /// hence a byte budget here and an `#[ignore]` test for the real count.
    #[test]
    fn the_catalogue_stays_small_enough_to_send_every_turn() {
        let mut descriptions = 0;
        let mut shipped = 0;
        let published = crate::mcp::voice::allowed_tools();
        for tool in catalogue() {
            let schema = tool.schema.to_string().len();
            let on_the_wire = published
                .iter()
                .any(|name| name.ends_with(tool.name));
            println!(
                "{} : {} octets de description + {schema} de schéma{}",
                tool.name,
                tool.description.len(),
                if on_the_wire { "" } else { "  (non publié)" }
            );
            descriptions += tool.description.len();
            if on_the_wire {
                shipped += tool.description.len() + schema;
            }
            assert!(
                tool.description.len() < 800,
                "{} fait {} octets",
                tool.name,
                tool.description.len()
            );
        }
        println!("descriptions : {descriptions} octets");
        println!("réellement envoyé : {shipped} octets");

        // **This bound replaced one that measured about half the cost.**
        //
        // Until 2026-11-12 the gate was `descriptions < 1600`, and it was wrong
        // in two directions at once. It counted `run_action`, which `unbacked`
        // keeps out of `--tools` so it never leaves the machine. And it ignored
        // the **schemas**, which do leave it and are nearly as large as the
        // descriptions — measured the day it was fixed: 1640 bytes of
        // description against 1494 of schema, 2886 actually sent.
        //
        // So the number here is bigger than the old one and the budget is
        // *tighter*, not looser: it is now the real figure. `docs/cli/claude-code-cli.md` §8
        // measured a whole turn of this post at 345–438 input tokens, which was
        // always measuring this amount rather than the 1537 the test printed.
        //
        // The margin is deliberately thin. Every tool here ships on every turn
        // of speech, so the next good idea has to take something out.
        //
        // **Moved once since, on 2026-11-13, and the number is the argument.**
        // Epic 05 needs the agent to say at launch whether a session belongs to
        // a project — that is the one decision about session lifetime, and it
        // cannot be taken anywhere else. `project` cost **131 bytes**, all of
        // it schema, taking the catalogue from 2886 to 3017. The ceiling went
        // to 3150, so the margin is 133 bytes: about one more parameter, and
        // then something has to come out. That is the intended reading.
        assert!(
            shipped < 3_150,
            "{shipped} octets partent à chaque tour de parole"
        );
    }

    /// The model tier, which is the whole of the user's second request on
    /// 2026-11-12: let the voice agent choose what does the work.
    ///
    /// Three short words rather than model identifiers, and the reasoning is in
    /// `crate::providers::ModelTier` — a tier maps onto every engine, costs a
    /// few bytes, and does not go stale when a model is renamed.
    #[test]
    fn the_delegation_lets_the_model_choose_a_tier_by_word() {
        let delegate = catalogue()
            .into_iter()
            .find(|t| t.name == DELEGATE_TASK)
            .expect("the catalogue carries delegate_task");

        // The words the schema offers are exactly the words the core reads
        // back. A disagreement here is a tier silently becoming standard.
        // Fast is deliberately absent (decision 126): a delegated session runs
        // on the middle tier at least.
        use crate::providers::ModelTier;
        let schema = delegate.schema.to_string();
        for tier in [ModelTier::Standard, ModelTier::Expert] {
            assert!(schema.contains(tier.word()), "{schema}");
        }
        assert!(!schema.contains(ModelTier::Fast.word()), "{schema}");
        // And the description says *when*, because an enum without a reason to
        // choose gets the default every time.
        assert!(delegate.description.contains("expert pour"));
    }

    #[test]
    fn a_delegated_session_never_runs_on_the_fast_tier() {
        use crate::providers::ModelTier;
        let asked = |word: Option<&str>| DelegateTask {
            model: word.map(str::to_string),
            ..serde_json::from_value(serde_json::json!({ "task": "t" })).unwrap()
        };
        assert_eq!(asked(Some("rapide")).tier(), ModelTier::Standard);
        assert_eq!(asked(None).tier(), ModelTier::Standard);
        assert_eq!(asked(Some("expert")).tier(), ModelTier::Expert);
    }

    /// A misspelled tier must cost nothing. The value is written by a language
    /// model, and failing the call would lose the user their answer to save a
    /// token.
    #[test]
    fn a_delegation_survives_a_tier_nobody_recognises() {
        let call: DelegateTask =
            serde_json::from_str(r#"{"task":"chercher","model":"costaud"}"#).unwrap();
        assert_eq!(call.tier(), crate::providers::ModelTier::Standard);

        let asked: DelegateTask =
            serde_json::from_str(r#"{"task":"corriger le bug","model":"expert"}"#).unwrap();
        assert_eq!(asked.tier(), crate::providers::ModelTier::Expert);

        // And no tier at all is standard, which is what a dispatcher that does
        // not think about it should get.
        let plain: DelegateTask = serde_json::from_str(r#"{"task":"chercher"}"#).unwrap();
        assert_eq!(plain.tier(), crate::providers::ModelTier::Standard);
    }

    /// Decision 87, both halves, and they only mean something together.
    ///
    /// The original line — "tu ne connais pas l'identité de ton interlocuteur"
    /// — was there because its absence produced a fault out loud. Memory
    /// contradicts it head-on, so it was **tightened** rather than deleted:
    /// what ARIA knows comes from the memory and from nowhere else.
    #[test]
    fn the_prompt_allows_a_name_from_memory_and_still_forbids_one_from_anywhere_else() {
        // The permitted half.
        assert!(SYSTEM_PROMPT.contains("que ce que la mémoire d'ARIA te rend"));
        // The forbidden half, with the two places a name has actually leaked
        // from named so the model has something concrete to refuse.
        assert!(SYSTEM_PROMPT.contains("trouvés ailleurs"));
        assert!(SYSTEM_PROMPT.contains("résultat d'outil"));
        assert!(SYSTEM_PROMPT.contains("chemin de"));
        assert!(SYSTEM_PROMPT.contains("n'en déduis rien"));
    }

    /// `recall` has to say what to do when nothing comes back, or a model with
    /// an empty memory fills the silence itself.
    #[test]
    fn recall_tells_the_model_not_to_invent_a_memory() {
        let recall = catalogue()
            .into_iter()
            .find(|t| t.name == RECALL)
            .expect("the catalogue carries recall");
        assert!(recall.description.contains("n'invente"));
    }

    /// The negative half of decision 95's nudge. A disposition to remember with
    /// only a positive half produces a dossier, not a memory — and ARIA hears
    /// a room.
    #[test]
    fn remember_carries_the_half_that_says_what_not_to_keep() {
        let remember = catalogue()
            .into_iter()
            .find(|t| t.name == REMEMBER)
            .expect("the catalogue carries remember");
        assert!(remember.description.contains("Jamais un secret"));
        assert!(remember.description.contains("sans être un fait sur lui"));
    }

    /// The other half of the same decision: the main agent is served
    /// `delegate_task` and nothing else, so `show_tile` does not exist for it.
    ///
    /// `--tools` is what makes a tool exist (`docs/cli/claude-code-cli.md` §9), and this is the
    /// list that feeds it.
    #[test]
    fn the_main_agent_is_not_given_the_tile_tool() {
        for tool in catalogue() {
            assert_ne!(
                tool.name,
                crate::tiles::SHOW_TILE,
                "une tuile se pousse depuis une session, pas depuis le poste vocal"
            );
        }
    }

    /// Everything written here is spoken, and these are the faults that are
    /// audible.
    #[test]
    fn the_prompt_forbids_what_cannot_be_heard() {
        assert!(SYSTEM_PROMPT.contains("Markdown"));
        assert!(SYSTEM_PROMPT.contains("répète-la telle quelle"));
        assert!(SYSTEM_PROMPT.contains("ne change aucun chiffre"));
        // Measured on 2026-10-26: the CLI puts the account's e-mail into the
        // session context whatever `--system-prompt` says, and the model
        // greeted the user by a first name inferred from it — one that does not
        // match the account's own `displayName`. Who ARIA is talking to is
        // ARIA's setting, not a guess from an address.
        //
        // Still asserted, in the tightened wording decision 87 asked for: the
        // prohibition now covers everything **except** the memory, and
        // `the_prompt_allows_a_name_from_memory_and_still_forbids_one_from_anywhere_else`
        // below checks both halves. The fault this line prevents was a name
        // inferred from the e-mail, and the e-mail is still "ailleurs".
        assert!(SYSTEM_PROMPT.contains("ton interlocuteur"));
        assert!(SYSTEM_PROMPT.contains("trouvés ailleurs"));
        // Measured out loud on 2026-10-27, the first run with a voice: the
        // local waiting phrase said "Je regarde." and the model added "Je
        // cherche la météo de demain, le résultat arrivera dans un instant."
        // Two announcements for one departure, and a second and a half of it.
        // The first wording — "ne décris pas ce que tu vas faire" — was not
        // an instruction a model obeys; "NE DIS RIEN" is.
        assert!(SYSTEM_PROMPT.contains("NE DIS RIEN"));
        // The two lines added on 2026-11-12, and both are about one thing: the
        // post has to be able to answer on its own, and to know when things
        // happened. The bracket is what gives it a clock (`super::clock`), and
        // it would otherwise be **read out loud** — a guaranteed audible fault,
        // which is this prompt's own standard for earning a line.
        assert!(SYSTEM_PROMPT.contains("entre crochets"));
        assert!(SYSTEM_PROMPT.contains("Réponds toi-même"));
        // Decision 119, and the line earns its place by the fault decision 47
        // heard: twelve answers to people moving a wardrobe. In a wake by the
        // name everything heard reaches the agent, so it must be able to stay
        // out — and the mark it is asked for is the one the pilot mutes.
        assert!(SYSTEM_PROMPT.contains(&format!("« {} »", super::super::sentences::SILENCE)));
        // It is sent on every turn, so its size is a running cost. `docs/cli/claude-code-cli.md` §8
        // measured 345–438 input tokens for a whole turn, at the size this was
        // before 2026-11-12.
        //
        // The ceiling moved from 1200 to 1500 on 2026-11-12, and that is a cost
        // taken knowingly rather than a number nudged to make a test pass.
        // Measured: the two lines above are **377 bytes** — 160 for the clock,
        // 207 for answering alone, 10 for "au-dehors" — and the whole prompt
        // went from 913 to **1290**, roughly a hundred extra tokens a turn.
        //
        // What they buy is the one thing a tool could not: a clock as a tool
        // would cost its schema in *every* turn's input **and** a round trip per
        // question, where a stamp rides on a message that was leaving anyway.
        //
        // A third line was written and then dropped against this budget. It
        // told the model not to relaunch a task in order to re-answer from its
        // result; the line above covers it, and nothing has been heard doing
        // that. This file's rule is that a line earns its place by a fault
        // somebody heard, not by a worry.
        //
        // **Moved to 1600 on 2026-11-23, for one line that a fault earned**:
        // one subject explained in several breaths opened three sessions on
        // the same project. `follows` is the mechanism; the line is what tells
        // the model to reach for it. Kept to one clause.
        //
        // 2026-11-26, within the budget: asked to fill in a timesheet, the
        // model asked the user for the ticket and the hours instead of
        // delegating, and had to be told its sessions could find them. The
        // line on lists was shortened to pay for it.
        //
        // 2026-11-27, within the budget (1595): a cinema and a drive to Lyon,
        // asked in one breath, went as one `follows` because the line said
        // « un ajout » goes there. It now says a precision does, and another
        // subject is another task.
        assert!(SYSTEM_PROMPT.len() < 1600, "{} bytes", SYSTEM_PROMPT.len());
        eprintln!("prompte systeme : {} octets", SYSTEM_PROMPT.len());
    }

    #[test]
    fn a_delegation_round_trips_without_inventing_fields() {
        let call: DelegateTask =
            serde_json::from_str(r#"{"task":"chercher la météo de demain à Lyon"}"#).unwrap();
        assert_eq!(call.context, None);
        assert_eq!(
            serde_json::to_string(&call).unwrap(),
            r#"{"task":"chercher la météo de demain à Lyon"}"#
        );
    }
}
