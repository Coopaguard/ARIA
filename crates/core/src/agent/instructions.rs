//! The two files of standing instructions the user may edit.
//!
//! Decision 106. ARIA runs two kinds of agent and they do not take the same
//! instructions, so there are two files and not one:
//!
//! | File | Who reads it | What it costs |
//! |---|---|---|
//! | `~/.aria/AGENTS.md` | the voice post | **every turn of speech** |
//! | `~/.aria/subagents/AGENTS.md` | every delegated session | once per task |
//!
//! Both are passed with `--append-system-prompt-file`, which was verified to
//! exist on CLI 2.1.312 on 2026-11-12 and to combine with `--system-prompt` —
//! the voice post replaces the harness prompt and still appends this file.
//! The flag takes a **path**: the CLI reads the file itself, so a long file
//! never reaches a command line that Windows bounds at 32 767 characters.
//!
//! # Why they live under `~/.aria`
//!
//! Beside `~/.aria/memories`, and for decision 94's argument rather than by
//! analogy: **someone will open this one**. A file nobody can find is a
//! setting that does not exist. `%LOCALAPPDATA%/aria` is the other root and it
//! is deliberately not this one — what goes there is derived, large and
//! re-downloadable (the models, the vector index); what goes here is written
//! by the user and is theirs.
//!
//! # Not the repository's own `AGENTS.md`
//!
//! The file at the root of this repository is a different thing with the same
//! name: it carries the follow-ups that depend on a third party. These two are
//! the user's instructions to their assistant, and nothing reads both.
//!
//! # The cost of the first one, said plainly
//!
//! `docs/cli/claude-code-cli.md` §8 measured a whole turn of the voice post at 345 to 438 input
//! tokens, which is why [`super::tools`] bounds its entire tool catalogue to
//! 1 600 bytes. A file appended to that prompt is paid at the same rate, on
//! every sentence ARIA speaks. So the **shipped default for the voice post is
//! nearly empty** — a heading and a hint — and a test below holds it there.
//! What the user then writes is their own trade-off to make; what ARIA ships
//! must not quietly double the prompt.
//!
//! The sub-agents' file has no such budget: a delegated session already
//! carries the CLI's own coding-agent prompt, and it has a handful of turns
//! rather than one per sentence.

use std::path::PathBuf;

/// Where the two files are expected, relative to the user's home.
///
/// Spelled here rather than in `app/` so the core and the code that resolves
/// `~` cannot disagree about the name of a file the user is told to edit.
pub const DIR: &str = ".aria";
pub const MAIN_FILE: &str = "AGENTS.md";
pub const SUBAGENTS_DIR: &str = "subagents";

/// What `~/.aria` holds for the agents, once `app/` has resolved it.
///
/// Four paths and one question each: **what is an agent told**, and **where
/// does it run**. They are one struct because they are one folder and one
/// resolution — `app/` reads `~` once, decision 94's rule being that the core
/// receives paths and never discovers them.
///
/// `None` anywhere is not a failure: it is the state before `app/` has looked,
/// and the state after a home directory that could not be written to. It costs
/// the path and never the session — a missing [`Self::dir`] leaves the main
/// agent in the throwaway directory it used before, and a missing
/// [`Self::subagents_dir`] leaves a delegated session wherever the application
/// was started, which is what both did until 2026-11-12.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Home {
    /// `~/.aria` — the main agent's working directory.
    ///
    /// Asked for by the user on 2026-11-12. It replaces a directory under
    /// `%TEMP%`, which was chosen for one property — no `CLAUDE.md` in it —
    /// and had nothing else to recommend it. `~/.aria` is where the user's own
    /// files for ARIA already live, so a path the agent mentions is a path the
    /// user recognises.
    pub dir: Option<PathBuf>,
    /// `~/.aria/AGENTS.md` — appended to the voice post's prompt.
    pub main: Option<PathBuf>,
    /// `~/.aria/subagents` — where a delegated session runs **by default**.
    ///
    /// By default, and not always: a task about a project belongs in that
    /// project's folder, and [`crate::providers::LaunchSpec::cwd`] is what
    /// says so per task. This is the floor, for a task that names nowhere —
    /// and the floor used to be "wherever ARIA was launched from", which in
    /// development is this repository.
    pub subagents_dir: Option<PathBuf>,
    /// `~/.aria/subagents/AGENTS.md` — appended to every delegated session's.
    pub subagents: Option<PathBuf>,
}

/// What `~/.aria/AGENTS.md` is created with when it is not there.
///
/// Deliberately almost nothing — see the module header. The hint is addressed
/// to the user and is harmless if the model reads it, which it will.
pub const DEFAULT_MAIN: &str = "\
# Consignes pour ARIA

Ces lignes sont ajoutées à la prompte d'ARIA **à chaque tour de parole** : garde-les
courtes, et souviens-toi que tout ce qu'ARIA écrit est prononcé à voix haute.

Les consignes pour les tâches de fond, qui peuvent être longues, vont dans
`subagents/AGENTS.md`, à côté de ce fichier.
";

/// What `~/.aria/subagents/AGENTS.md` is created with when it is not there.
///
/// This one may say what it needs. Three things a delegated session gets
/// wrong without being told, each observed rather than imagined:
///
/// 1. it does not know who it is working for, so it guesses — and the voice
///    post is forbidden from guessing for exactly that reason (decision 87);
/// 2. it does not know the memory is there, so it never looks;
/// 3. it answers like a coding agent, in Markdown, to a user who will hear it.
pub const DEFAULT_SUBAGENTS: &str = "\
# Consignes pour les tâches de fond d'ARIA

Tu travailles pour ARIA, l'assistant vocal de cet utilisateur. Il t'a confié cette
tâche en parlant, et il entendra la fin de ta réponse à voix haute.

## Ce qu'ARIA sait de lui, tu peux le lire

Ses préférences, ses décisions et ses habitudes sont dans sa mémoire personnelle.
Deux outils : `recall` cherche et rend des résumés, `read_page` ouvre une page en
entier. **Consulte-la avant de supposer quoi que ce soit sur lui** — son matériel,
ses horaires, ses goûts, la ville où il vit, les noms qu'il emploie.

Tu n'y écris pas toi-même. Si tu apprends un fait durable à son sujet, propose-le
avec `propose_memory` : ARIA lit tes propositions avec ton résultat et garde ce
qui en vaut la peine.

## Ce que tu ne dois pas déduire

Si la mémoire ne dit rien, tu ne sais rien. N'utilise jamais un nom, une adresse
ni une ville trouvés ailleurs — un résultat d'outil, un chemin de fichier, un
compte connecté — et n'en déduis rien.

## Chercher des chiffres

Pour une donnée chiffrée et datée — météo, horaires, cours —, une recherche web
rend des pages qu'on lit mal. Va chercher la donnée elle-même. Pour la météo,
Open-Meteo, gratuit et sans clé, avec `WebFetch` :

- la ville : `https://geocoding-api.open-meteo.com/v1/search?name=Montpellier&count=1&language=fr`
- la prévision : `https://api.open-meteo.com/v1/forecast?latitude=…&longitude=…&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,precipitation_probability_max,wind_speed_10m_max&timezone=auto&forecast_days=3`

Une source qui échoue n'est pas une réponse : essaie-en une autre avant de dire
que tu n'as pas trouvé.

## Comment répondre

En français. Ce qui suit « À DIRE : » est lu à voix haute, jusqu'à la fin de ta
réponse : sans Markdown ni liste, les nombres écrits tels qu'ils se disent. Ce qui
précède n'est pas lu et peut être aussi détaillé qu'il le faut.

Vise moins de deux cents mots : au-delà, on n'écoute plus. Le programme d'un
cinéma, ce sont les films et leurs horaires, pas leurs résumés.

Quand tu as plus à dire — un plan, ses conséquences, plusieurs questions à lui
poser —, ne lui lis pas tout :

- le détail va dans une tuile, avec `show_tile` ; tu ne dis que l'essentiel, et
  qu'elle est à l'écran ;
- s'il te faut ses réponses, pose **une seule question**, la plus structurante,
  et arrête-toi. Sa réponse te reviendra dans cette même session : pose alors la
  suivante.

Ce qui se montre mieux qu'il ne se dit — un relevé, un tableau, des chiffres à
comparer — va aussi dans une tuile.
";

/// What 0.1.2 and 0.1.3 wrote, when one sentence was said and a tile waited to
/// be asked for.
const SUBAGENTS_0_1_2: &str = "\
# Consignes pour les tâches de fond d'ARIA

Tu travailles pour ARIA, l'assistant vocal de cet utilisateur. Il t'a confié cette
tâche en parlant, et il entendra ta dernière phrase à voix haute.

## Ce qu'ARIA sait de lui, tu peux le lire

Ses préférences, ses décisions et ses habitudes sont dans sa mémoire personnelle.
Deux outils : `recall` cherche et rend des résumés, `read_page` ouvre une page en
entier. **Consulte-la avant de supposer quoi que ce soit sur lui** — son matériel,
ses horaires, ses goûts, la ville où il vit, les noms qu'il emploie.

Tu n'y écris pas toi-même. Si tu apprends un fait durable à son sujet, propose-le
avec `propose_memory` : ARIA lit tes propositions avec ton résultat et garde ce
qui en vaut la peine.

## Ce que tu ne dois pas déduire

Si la mémoire ne dit rien, tu ne sais rien. N'utilise jamais un nom, une adresse
ni une ville trouvés ailleurs — un résultat d'outil, un chemin de fichier, un
compte connecté — et n'en déduis rien.

## Chercher des chiffres

Pour une donnée chiffrée et datée — météo, horaires, cours —, une recherche web
rend des pages qu'on lit mal. Va chercher la donnée elle-même. Pour la météo,
Open-Meteo, gratuit et sans clé, avec `WebFetch` :

- la ville : `https://geocoding-api.open-meteo.com/v1/search?name=Montpellier&count=1&language=fr`
- la prévision : `https://api.open-meteo.com/v1/forecast?latitude=…&longitude=…&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,precipitation_probability_max,wind_speed_10m_max&timezone=auto&forecast_days=3`

Une source qui échoue n'est pas une réponse : essaie-en une autre avant de dire
que tu n'as pas trouvé.

## Comment répondre

En français. Ta dernière ligne sera prononcée : une phrase, qui se suffit à
elle-même, sans Markdown ni liste, les nombres écrits tels qu'ils se disent. Le
reste de ta réponse n'est pas lu à voix haute et peut être aussi détaillé qu'il
le faut.

Si la réponse se montre mieux qu'elle ne se dit — un relevé, un tableau, des
chiffres à comparer — et qu'ARIA t'a demandé une tuile, utilise `show_tile`.
";

/// What 0.1.0 to 0.1.2 wrote, before sessions could propose a memory.
const SUBAGENTS_0_1: &str = "\
# Consignes pour les tâches de fond d'ARIA

Tu travailles pour ARIA, l'assistant vocal de cet utilisateur. Il t'a confié cette
tâche en parlant, et il entendra ta dernière phrase à voix haute.

## Ce qu'ARIA sait de lui, tu peux le lire

Ses préférences, ses décisions et ses habitudes sont dans sa mémoire personnelle.
Deux outils : `recall` cherche et rend des résumés, `read_page` ouvre une page en
entier. **Consulte-la avant de supposer quoi que ce soit sur lui** — son matériel,
ses horaires, ses goûts, la ville où il vit, les noms qu'il emploie.

Tu ne peux rien y écrire, et c'est voulu : ce qu'ARIA retient se décide avec lui,
dans la conversation. Si tu apprends un fait durable à son sujet, dis-le dans ta
réponse et ARIA lui demandera.

## Ce que tu ne dois pas déduire

Si la mémoire ne dit rien, tu ne sais rien. N'utilise jamais un nom, une adresse
ni une ville trouvés ailleurs — un résultat d'outil, un chemin de fichier, un
compte connecté — et n'en déduis rien.

## Chercher des chiffres

Pour une donnée chiffrée et datée — météo, horaires, cours —, une recherche web
rend des pages qu'on lit mal. Va chercher la donnée elle-même. Pour la météo,
Open-Meteo, gratuit et sans clé, avec `WebFetch` :

- la ville : `https://geocoding-api.open-meteo.com/v1/search?name=Montpellier&count=1&language=fr`
- la prévision : `https://api.open-meteo.com/v1/forecast?latitude=…&longitude=…&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_sum,precipitation_probability_max,wind_speed_10m_max&timezone=auto&forecast_days=3`

Une source qui échoue n'est pas une réponse : essaie-en une autre avant de dire
que tu n'as pas trouvé.

## Comment répondre

En français. Ta dernière ligne sera prononcée : une phrase, qui se suffit à
elle-même, sans Markdown ni liste, les nombres écrits tels qu'ils se disent. Le
reste de ta réponse n'est pas lu à voix haute et peut être aussi détaillé qu'il
le faut.

Si la réponse se montre mieux qu'elle ne se dit — un relevé, un tableau, des
chiffres à comparer — et qu'ARIA t'a demandé une tuile, utilise `show_tile`.
";

/// Shipped texts that were never edited are brought up to date: a file still
/// byte for byte what ARIA wrote is not the user's, and keeping it would leave
/// the sessions told the opposite of what their tools now do.
pub const PREVIOUS_SUBAGENTS: &[&str] = &[SUBAGENTS_0_1, SUBAGENTS_0_1_2];

#[cfg(test)]
mod tests {
    use super::*;

    /// The budget the module header argues for, as a number.
    ///
    /// The voice post's whole turn is 345–438 input tokens (`docs/cli/claude-code-cli.md` §8) and
    /// its entire tool catalogue is bounded at 1 600 bytes. A shipped default
    /// larger than this would be ARIA charging the user for a file they never
    /// wrote.
    #[test]
    fn the_shipped_default_for_the_voice_post_is_nearly_free() {
        println!("AGENTS.md du poste vocal : {} octets", DEFAULT_MAIN.len());
        assert!(
            DEFAULT_MAIN.len() < 500,
            "{} octets, envoyés à chaque tour de parole",
            DEFAULT_MAIN.len()
        );
        // And it must point the user at the other file, or they will write
        // their long instructions here and pay for them on every sentence.
        assert!(DEFAULT_MAIN.contains("subagents/AGENTS.md"));
    }

    /// The sub-agents' file has no budget, and the three faults it exists to
    /// prevent have to be in it.
    #[test]
    fn the_subagents_file_says_the_three_things_a_session_gets_wrong() {
        // It does not know whose assistant it is working for.
        assert!(DEFAULT_SUBAGENTS.contains("assistant vocal"));
        // It does not know the memory is there.
        assert!(DEFAULT_SUBAGENTS.contains("recall"));
        assert!(DEFAULT_SUBAGENTS.contains("read_page"));
        assert!(DEFAULT_SUBAGENTS.contains("propose_memory"));
        assert!(!PREVIOUS_SUBAGENTS.contains(&DEFAULT_SUBAGENTS));
        // Decision 87's prohibition, in the same words the voice post's prompt
        // uses, because the fault is the same one: a name inferred from an
        // e-mail address the CLI put in the context.
        assert!(DEFAULT_SUBAGENTS.contains("trouvés ailleurs"));
        assert!(DEFAULT_SUBAGENTS.contains("n'en déduis rien"));
        // And what is read aloud, and how long it may run (2026-11-27): a
        // target, a tile for the detail, one question at a time.
        assert!(DEFAULT_SUBAGENTS.contains("lu à voix haute"));
        assert!(DEFAULT_SUBAGENTS.contains("deux cents mots"));
        assert!(DEFAULT_SUBAGENTS.contains("show_tile"));
        assert!(DEFAULT_SUBAGENTS.contains("une seule question"));
    }

    /// The session does not write: it is told so, with what to do instead.
    ///
    /// The post has no write ([`crate::mcp::subagent`]), which a model
    /// discovers as a missing tool and may read as a failure.
    #[test]
    fn the_subagents_file_says_to_propose_rather_than_write() {
        assert!(DEFAULT_SUBAGENTS.contains("Tu n'y écris pas toi-même"));
        assert!(DEFAULT_SUBAGENTS.contains("propose_memory"));
    }

    /// Nothing resolved is the honest state at start-up, and it must be the
    /// cheap one: no file, no flag, a session that launches exactly as before.
    #[test]
    fn nothing_is_resolved_until_app_says_so() {
        let found = Home::default();
        assert_eq!(found.dir, None);
        assert_eq!(found.main, None);
        assert_eq!(found.subagents_dir, None);
        assert_eq!(found.subagents, None);
    }
}
