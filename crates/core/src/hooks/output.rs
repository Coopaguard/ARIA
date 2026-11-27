//! What a hook's script prints, and what it means.
//!
//! The whole contract is two cases. **Nothing** on stdout: nothing new, and
//! nothing is recorded — the run that found nothing is the normal run. **One
//! JSON line**: an event. Anything else is a fault of the script, reported as
//! such, never guessed at: a hook that prints a progress line before its
//! event would otherwise wake ARIA with « Connexion au serveur… ».

use serde::Deserialize;
use serde_json::Value;

/// Longest `why`: a reason, for the tab and for the main agent.
pub const WHY_MAX: usize = 200;
/// Longest `say`: about fifteen seconds aloud. A hook announces; detail is
/// what the user asks for next.
pub const SAY_MAX: usize = 400;
/// Largest `data`, serialised. Ids and links, never whole contents.
pub const DATA_MAX: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// Shown, never said.
    Info,
    #[default]
    Normal,
    /// Said at the next sentence boundary, even in quiet hours or asleep.
    Urgent,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Event {
    pub why: String,
    /// Spoken verbatim. Without it the main agent words the event from `data`.
    pub say: Option<String>,
    pub data: Option<Value>,
    pub level: Level,
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Nothing,
    Event(Event),
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum OutputError {
    #[error("la sortie n'est pas un objet JSON : {0}")]
    NotJson(String),
    #[error("plusieurs lignes en sortie : un seul événement JSON, ou rien")]
    Several,
    #[error("« why » est vide")]
    NoWhy,
    #[error("« {0} » est trop long ({1} caractères)")]
    TooLong(&'static str, usize),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    why: String,
    #[serde(default)]
    say: Option<String>,
    #[serde(default)]
    data: Option<Value>,
    #[serde(default)]
    level: Level,
}

pub fn parse(stdout: &str) -> Result<Outcome, OutputError> {
    // PowerShell may open its output with a byte-order mark.
    let stdout = stdout.trim_start_matches('\u{feff}');
    let lines: Vec<&str> = stdout.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let line = match lines.as_slice() {
        [] => return Ok(Outcome::Nothing),
        [line] => *line,
        _ => return Err(OutputError::Several),
    };
    let raw: Raw = serde_json::from_str(line).map_err(|e| OutputError::NotJson(e.to_string()))?;
    let why = raw.why.trim().to_string();
    if why.is_empty() {
        return Err(OutputError::NoWhy);
    }
    check("why", &why, WHY_MAX)?;
    let say = raw.say.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if let Some(say) = &say {
        check("say", say, SAY_MAX)?;
    }
    let data = raw.data.filter(|d| !d.is_null());
    if let Some(data) = &data {
        let size = data.to_string().len();
        if size > DATA_MAX {
            return Err(OutputError::TooLong("data", size));
        }
    }
    Ok(Outcome::Event(Event { why, say, data, level: raw.level }))
}

fn check(field: &'static str, text: &str, max: usize) -> Result<(), OutputError> {
    let count = text.chars().count();
    if count > max {
        return Err(OutputError::TooLong(field, count));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_the_normal_run() {
        assert_eq!(parse(""), Ok(Outcome::Nothing));
        assert_eq!(parse("\r\n  \n"), Ok(Outcome::Nothing));
        assert_eq!(parse("\u{feff}"), Ok(Outcome::Nothing));
    }

    #[test]
    fn one_line_is_an_event() {
        let out = parse(
            "\u{feff}{\"why\":\"nouveau mail\",\"say\":\"Margaux t'a écrit pour le pique-nique.\",\
             \"data\":{\"id\":\"18c\"}}\r\n",
        )
        .unwrap();
        let Outcome::Event(event) = out else { panic!() };
        assert_eq!(event.say.as_deref(), Some("Margaux t'a écrit pour le pique-nique."));
        assert_eq!(event.level, Level::Normal);
        assert_eq!(event.data.unwrap()["id"], "18c");
    }

    /// A progress line before the event is a broken script, not an event.
    #[test]
    fn noise_is_refused_not_guessed() {
        assert_eq!(parse("Connexion…\n{\"why\":\"x\"}"), Err(OutputError::Several));
        assert!(matches!(parse("Rien de nouveau"), Err(OutputError::NotJson(_))));
        assert_eq!(parse("{\"why\":\"  \"}"), Err(OutputError::NoWhy));
        assert!(matches!(parse("{\"why\":\"x\",\"urgence\":1}"), Err(OutputError::NotJson(_))));
    }

    #[test]
    fn whole_contents_do_not_fit() {
        let body = "x".repeat(DATA_MAX);
        let line = format!("{{\"why\":\"x\",\"data\":{{\"body\":\"{body}\"}}}}");
        assert!(matches!(parse(&line), Err(OutputError::TooLong("data", _))));
        let line = format!("{{\"why\":\"x\",\"say\":\"{}\"}}", "a".repeat(SAY_MAX + 1));
        assert!(matches!(parse(&line), Err(OutputError::TooLong("say", _))));
    }

    #[test]
    fn levels_order_by_urgency() {
        assert!(Level::Info < Level::Normal && Level::Normal < Level::Urgent);
        let Ok(Outcome::Event(e)) = parse("{\"why\":\"x\",\"level\":\"urgent\",\"say\":\"\"}") else {
            panic!()
        };
        assert_eq!(e.level, Level::Urgent);
        assert_eq!(e.say, None, "an empty say is no say");
    }
}
