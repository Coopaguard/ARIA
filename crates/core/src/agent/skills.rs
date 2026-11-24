//! What the voice post is told its delegations can reach.
//!
//! Decision 117, asked for by the user on 2026-11-13: the main agent holds a
//! **synthetic** list of what its sub-agents can do — "mail, Drive", not five
//! connector names and their URLs — so that "envoie ça par mail à Louis" is
//! delegated instead of answered with "je ne peux pas envoyer de mail".
//!
//! # Why a line of the system prompt, and not a tool or a message
//!
//! A tool would cost its schema on every turn *and* a round trip for a fact
//! that does not change during a conversation. A message would be lost in the
//! thread. The prompt is frozen at the first request
//! (`--system-prompt-snapshot`, decision 107) — which is exactly right for a
//! list that is read once at start-up: a connector added later is seen at the
//! next start, the same asymmetry as changing the voice post's engine.
//!
//! # Why the line is bounded
//!
//! It ships on **every turn of speech**. A user with thirty connectors must not
//! double the prompt in silence, so at most [`MAX_SKILLS`] are named and the
//! rest are summed up in three words.

use crate::providers::Connector;

/// Most skills named before the rest is summed up.
pub const MAX_SKILLS: usize = 6;

/// The prefix claude.ai puts on the connectors it manages. Says where the
/// connector comes from, not what it does.
const HOSTED_PREFIX: &str = "claude.ai ";

/// Names that do not say what the connector is for, and the word that does.
///
/// Matched on the lowercased name, by substring. Only the cases where the
/// product name hides the skill: "Notion" or "GitHub" already say it, and an
/// unlisted connector keeps its own name — which is never wrong, merely less
/// helpful.
const PLAIN_WORDS: &[(&str, &str)] = &[
    ("gmail", "mail (lire, rédiger, envoyer)"),
    ("outlook", "mail (lire, rédiger, envoyer)"),
    ("calendar", "agenda"),
    ("agenda", "agenda"),
    ("drive", "Google Drive"),
    ("context7", "documentation de code"),
];

/// One skill per connected connector, deduplicated, in the engine's order.
pub fn skills(connectors: &[Connector]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for connector in connectors.iter().filter(|c| c.connected) {
        let name = connector.name.trim();
        let lower = name.to_lowercase();
        let skill = PLAIN_WORDS
            .iter()
            .find(|(key, _)| lower.contains(key))
            .map(|(_, word)| word.to_string())
            .unwrap_or_else(|| name.strip_prefix(HOSTED_PREFIX).unwrap_or(name).to_string());
        if !skill.is_empty() && !out.contains(&skill) {
            out.push(skill);
        }
    }
    out
}

/// The line added to the voice post's prompt, or `None` when there is
/// nothing to say.
///
/// Its second sentence is the point of the whole decision: knowing a skill
/// exists is not the same as knowing it is to be delegated.
pub fn prompt_line(connectors: &[Connector]) -> Option<String> {
    let skills = skills(connectors);
    if skills.is_empty() {
        return None;
    }
    let mut named = skills
        .iter()
        .take(MAX_SKILLS)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if skills.len() > MAX_SKILLS {
        named.push_str(", et d'autres services");
    }
    Some(format!(
        "Tes tâches déléguées ont aussi accès à : {named}. Ce qui les touche se délègue aussi."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(name: &str) -> Connector {
        Connector {
            name: name.into(),
            connected: true,
        }
    }

    /// The machine this was written on, 2026-11-13.
    #[test]
    fn the_user_s_connectors_become_plain_words() {
        let line = prompt_line(&[
            on("claude.ai Claude Docs"),
            on("claude.ai Google Drive"),
            on("claude.ai Context7"),
            on("claude.ai Gmail"),
            on("claude.ai Google Calendar"),
        ])
        .expect("five connectors say something");
        assert!(line.contains("mail (lire, rédiger, envoyer)"), "{line}");
        assert!(line.contains("agenda"), "{line}");
        assert!(line.contains("Google Drive"), "{line}");
        // Where a connector comes from is not what it does.
        assert!(!line.contains("claude.ai"), "{line}");
        assert!(line.contains("se délègue"), "{line}");
        eprintln!("ligne des compétences : {} octets", line.len());
        assert!(line.len() < 300, "{} octets", line.len());
    }

    /// A connector that cannot be reached must not be promised.
    #[test]
    fn an_unreachable_connector_is_not_named() {
        let line = prompt_line(&[
            on("claude.ai Gmail"),
            Connector {
                name: "claude.ai Google Calendar".into(),
                connected: false,
            },
        ])
        .unwrap();
        assert!(!line.contains("agenda"), "{line}");
    }

    #[test]
    fn nothing_connected_says_nothing() {
        assert_eq!(prompt_line(&[]), None);
        assert_eq!(
            prompt_line(&[Connector {
                name: "claude.ai Gmail".into(),
                connected: false
            }]),
            None
        );
    }

    #[test]
    fn two_mail_connectors_are_one_skill() {
        assert_eq!(
            skills(&[on("claude.ai Gmail"), on("Outlook")]),
            vec!["mail (lire, rédiger, envoyer)"]
        );
    }

    /// Every turn pays for this line, so thirty connectors cost six names.
    #[test]
    fn a_long_list_is_bounded() {
        let many: Vec<Connector> = (0..30).map(|i| on(&format!("service{i}"))).collect();
        let line = prompt_line(&many).unwrap();
        assert!(line.contains("service5"));
        assert!(!line.contains("service6"));
        assert!(line.contains("et d'autres"));
    }
}
