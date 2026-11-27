//! `hook.toml`: what a hook declares about itself.
//!
//! Written by a session, read by the user, so it stays small and every field
//! has one meaning. Unknown fields are refused rather than ignored: a typo in
//! `max_per_day` that silently does nothing is a hook that wakes ARIA forty
//! times a day.
//!
//! The switch is **not** here. On, off and validated are held by the core
//! beside the fingerprint, so a session that rewrites this file cannot switch
//! a hook back on, and the tab never has to rewrite a file the user annotated.

use std::time::Duration;

use chrono::NaiveTime;
use serde::Deserialize;

/// Shortest rhythm accepted. A check every few seconds is polling, and a hook
/// is meant to be the opposite.
pub const MIN_EVERY: Duration = Duration::from_secs(60);
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(600);

/// The file every hook folder holds.
pub const FILE: &str = "hook.toml";

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    /// What the tab shows and what ARIA says when asked, in French.
    pub name: String,
    /// The script, a bare file name inside the hook's folder.
    pub script: String,
    pub every: Duration,
    pub timeout: Duration,
    /// Hours when a `normal` event waits. `urgent` is never held.
    pub quiet: Option<Quiet>,
    /// Spoken events per day. Past it, events are shown and not said.
    pub max_per_day: Option<u32>,
    /// Names of the secrets the script receives, as `ARIA_SECRET_<NAME>`.
    pub secrets: Vec<String>,
}

/// A window of the day, which may wrap past midnight (`22:00-08:00`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quiet {
    pub from: NaiveTime,
    pub to: NaiveTime,
}

impl Quiet {
    pub fn contains(&self, at: NaiveTime) -> bool {
        if self.from <= self.to {
            self.from <= at && at < self.to
        } else {
            at >= self.from || at < self.to
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ManifestError {
    #[error("hook.toml illisible : {0}")]
    Toml(String),
    #[error("rythme « {0} » invalide : une durée comme 30m, 6h ou 1d, d'une minute au moins")]
    Every(String),
    #[error("délai « {0} » invalide : une durée de dix minutes au plus")]
    Timeout(String),
    #[error("heures calmes « {0} » invalides : HH:MM-HH:MM")]
    Quiet(String),
    #[error("script « {0} » invalide : un nom de fichier du dossier du hook")]
    Script(String),
    #[error("secret « {0} » invalide : lettres minuscules, chiffres et _, 32 au plus")]
    Secret(String),
    #[error("le nom du hook est vide")]
    Name,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    name: String,
    script: String,
    every: String,
    #[serde(default)]
    timeout: Option<String>,
    #[serde(default)]
    quiet: Option<String>,
    #[serde(default)]
    max_per_day: Option<u32>,
    #[serde(default)]
    secrets: Vec<String>,
}

pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
    let raw: Raw = toml::from_str(text).map_err(|e| ManifestError::Toml(e.message().to_string()))?;
    let name = raw.name.trim().to_string();
    if name.is_empty() {
        return Err(ManifestError::Name);
    }
    let every = duration(&raw.every)
        .filter(|d| *d >= MIN_EVERY)
        .ok_or_else(|| ManifestError::Every(raw.every.clone()))?;
    let timeout = match &raw.timeout {
        None => DEFAULT_TIMEOUT,
        Some(text) => duration(text)
            .filter(|d| !d.is_zero() && *d <= MAX_TIMEOUT)
            .ok_or_else(|| ManifestError::Timeout(text.clone()))?,
    };
    let quiet = raw
        .quiet
        .as_deref()
        .map(|text| quiet(text).ok_or_else(|| ManifestError::Quiet(text.to_string())))
        .transpose()?;
    if !is_bare_file_name(&raw.script) {
        return Err(ManifestError::Script(raw.script));
    }
    if let Some(bad) = raw.secrets.iter().find(|s| !is_secret_name(s)) {
        return Err(ManifestError::Secret(bad.clone()));
    }
    Ok(Manifest {
        name,
        script: raw.script,
        every,
        timeout,
        quiet,
        max_per_day: raw.max_per_day,
        secrets: raw.secrets,
    })
}

/// `90s`, `30m`, `6h`, `1d`. One unit: a rhythm is said, not computed.
pub fn duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let unit = text.chars().last()?;
    let count: u64 = text[..text.len() - unit.len_utf8()].trim().parse().ok()?;
    let seconds = match unit {
        's' => count,
        'm' => count.checked_mul(60)?,
        'h' => count.checked_mul(3600)?,
        'd' => count.checked_mul(86_400)?,
        _ => return None,
    };
    Some(Duration::from_secs(seconds))
}

fn quiet(text: &str) -> Option<Quiet> {
    let (from, to) = text.split_once('-')?;
    let from = NaiveTime::parse_from_str(from.trim(), "%H:%M").ok()?;
    let to = NaiveTime::parse_from_str(to.trim(), "%H:%M").ok()?;
    (from != to).then_some(Quiet { from, to })
}

/// No separator, no `..`: the script stays in its folder, which is what the
/// fingerprint covers.
fn is_bare_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', ':'])
        && name != FILE
}

pub fn is_secret_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIL: &str = r#"
name = "Mails"
script = "check.ps1"
every = "5m"
quiet = "22:00-08:00"
max_per_day = 12
secrets = ["imap"]
"#;

    #[test]
    fn a_whole_manifest_reads() {
        let m = parse(MAIL).unwrap();
        assert_eq!(m.name, "Mails");
        assert_eq!(m.every, Duration::from_secs(300));
        assert_eq!(m.timeout, DEFAULT_TIMEOUT);
        assert_eq!(m.max_per_day, Some(12));
        assert_eq!(m.secrets, ["imap"]);
        let quiet = m.quiet.unwrap();
        assert!(quiet.contains(NaiveTime::from_hms_opt(23, 0, 0).unwrap()));
        assert!(quiet.contains(NaiveTime::from_hms_opt(7, 59, 0).unwrap()));
        assert!(!quiet.contains(NaiveTime::from_hms_opt(8, 0, 0).unwrap()));
        assert!(!quiet.contains(NaiveTime::from_hms_opt(12, 0, 0).unwrap()));
    }

    /// A typo that does nothing is the noisy hook nobody can explain.
    #[test]
    fn an_unknown_field_is_refused() {
        let text = MAIL.replace("max_per_day", "max_per_dya");
        assert!(matches!(parse(&text), Err(ManifestError::Toml(_))));
    }

    #[test]
    fn a_rhythm_under_a_minute_is_polling() {
        let text = MAIL.replace("\"5m\"", "\"20s\"");
        assert_eq!(parse(&text), Err(ManifestError::Every("20s".into())));
        assert_eq!(duration("1d"), Some(Duration::from_secs(86_400)));
        assert_eq!(duration("6h"), Some(Duration::from_secs(21_600)));
        assert_eq!(duration("6 heures"), None);
        assert_eq!(duration(""), None);
    }

    /// The fingerprint covers the folder; a script elsewhere would escape it.
    #[test]
    fn the_script_stays_in_its_folder() {
        for bad in ["../evil.ps1", "C:\\x.ps1", "sub/x.sh", "..", "hook.toml", ""] {
            let text = MAIL.replace("check.ps1", &bad.replace('\\', "\\\\"));
            assert!(matches!(parse(&text), Err(ManifestError::Script(_))), "{bad}");
        }
    }

    #[test]
    fn secret_names_become_environment_names() {
        assert!(is_secret_name("imap_pass2"));
        assert!(!is_secret_name("IMAP"));
        assert!(!is_secret_name("a-b"));
        assert!(!is_secret_name(""));
    }
}
