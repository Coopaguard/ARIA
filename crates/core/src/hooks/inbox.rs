//! When a hook's event is said: the timing half of the noise rules.
//!
//! The script decides **what** is news; this decides **when** it reaches the
//! user. One pending entry per hook, merged when the hook fires again before
//! it was delivered, so a mailbox checked every five minutes during a long
//! exchange gives one announcement and not six.
//!
//! The caller says what the floor is doing ([`Floor`]). The rule that matters
//! most is the one in the user's own example: an event that arrives while ARIA
//! is answering **waits for the end of that answer** and follows it, « Au
//! fait, … ». It never cuts the answer, and never takes the floor from an
//! answer the user owes.

use std::collections::{BTreeMap, HashMap};

use chrono::{NaiveDate, NaiveDateTime};
use serde_json::Value;

use super::manifest::Quiet;
use super::output::{Event, Level};

/// Events kept per hook while one waits. Past it the oldest go: what is
/// announced is the latest news and a count.
const MAX_MERGED: usize = 20;

/// What the conversation is doing, as the caller sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Floor {
    /// Nobody is exchanging. ARIA may open: the orb lights, then she speaks.
    Free,
    /// An exchange is going on, the agent's turn has ended and its speech is
    /// out, and it did not end on a question. The event follows it.
    Between,
    /// The agent is answering, ARIA is speaking, the user is speaking, or the
    /// user owes an answer to a question. Nothing but `urgent` goes.
    Busy,
    /// ARIA is asleep. Only `urgent` wakes her.
    Asleep,
}

/// How a delivery reaches the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum How {
    /// Shown in the window, not said.
    Notify,
    /// The orb lights for two seconds, then ARIA says it and listens.
    Lead,
    /// Said right after the answer that just ended, with a link.
    Follow,
    /// `urgent` in the middle of something: at the next sentence boundary.
    Interject,
}

impl How {
    pub fn is_spoken(self) -> bool {
        self != How::Notify
    }
}

/// What a hook's rules say about timing, read from its manifest.
#[derive(Clone, Copy, Debug, Default)]
pub struct Rules {
    pub quiet: Option<Quiet>,
    pub max_per_day: Option<u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Delivery {
    pub hook: String,
    /// The events merged into one, the latest last.
    pub event: Event,
    /// How many runs raised it: 1 when nothing was merged.
    pub count: usize,
    pub how: How,
    /// When the first of them was raised.
    pub since: NaiveDateTime,
}

impl Delivery {
    /// The words for the voice, or `None` when the main agent words it.
    pub fn spoken(&self) -> Option<String> {
        if !self.how.is_spoken() {
            return None;
        }
        let say = self.event.say.as_deref()?;
        Some(match self.how {
            How::Follow => format!("Au fait, {}", lower_opening(say)),
            _ => say.to_string(),
        })
    }
}

#[derive(Debug, Default)]
pub struct Inbox {
    pending: BTreeMap<String, Pending>,
    /// Spoken deliveries per hook, on the day they were spoken.
    spoken: HashMap<String, (NaiveDate, u32)>,
}

#[derive(Debug)]
struct Pending {
    events: Vec<Event>,
    count: usize,
    since: NaiveDateTime,
}

impl Inbox {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, hook: &str, event: Event, at: NaiveDateTime) {
        let pending = self.pending.entry(hook.to_string()).or_insert_with(|| Pending {
            events: Vec::new(),
            count: 0,
            since: at,
        });
        pending.events.push(event);
        pending.count += 1;
        if pending.events.len() > MAX_MERGED {
            pending.events.remove(0);
        }
    }

    /// What may go now. What may not stays, to be asked again.
    pub fn take(
        &mut self,
        now: NaiveDateTime,
        floor: Floor,
        rules: impl Fn(&str) -> Rules,
    ) -> Vec<Delivery> {
        let mut out = Vec::new();
        let hooks: Vec<String> = self.pending.keys().cloned().collect();
        for hook in hooks {
            let rules = rules(&hook);
            let level = self.pending[&hook]
                .events
                .iter()
                .map(|e| e.level)
                .max()
                .unwrap_or_default();
            let over = rules
                .max_per_day
                .is_some_and(|max| self.spoken_today(&hook, now.date()) >= max);
            let quiet = rules.quiet.is_some_and(|q| q.contains(now.time()));
            let how = match (level, floor) {
                (Level::Info, _) => Some(How::Notify),
                // Past the day's allowance it is still news, so it is shown.
                _ if over => Some(How::Notify),
                (Level::Urgent, Floor::Busy) => Some(How::Interject),
                (Level::Urgent, Floor::Between) => Some(How::Follow),
                (Level::Urgent, Floor::Free | Floor::Asleep) => Some(How::Lead),
                (Level::Normal, _) if quiet => None,
                (Level::Normal, Floor::Busy | Floor::Asleep) => None,
                (Level::Normal, Floor::Between) => Some(How::Follow),
                (Level::Normal, Floor::Free) => Some(How::Lead),
            };
            let Some(how) = how else { continue };
            let pending = self.pending.remove(&hook).expect("listed above");
            if how.is_spoken() {
                let entry = self.spoken.entry(hook.clone()).or_insert((now.date(), 0));
                if entry.0 != now.date() {
                    *entry = (now.date(), 0);
                }
                entry.1 += 1;
            }
            out.push(Delivery {
                event: merge(pending.events),
                count: pending.count,
                hook,
                how,
                since: pending.since,
            });
        }
        out
    }

    /// What is waiting, for the tab: hook, merged event, count.
    pub fn waiting(&self) -> Vec<(String, Event, usize)> {
        self.pending
            .iter()
            .map(|(hook, p)| (hook.clone(), merge(p.events.clone()), p.count))
            .collect()
    }

    /// A hook switched off or deleted takes its pending news with it.
    pub fn forget(&mut self, hook: &str) {
        self.pending.remove(hook);
    }

    fn spoken_today(&self, hook: &str, today: NaiveDate) -> u32 {
        match self.spoken.get(hook) {
            Some((day, n)) if *day == today => *n,
            _ => 0,
        }
    }
}

/// Several runs' events as one.
///
/// The sentences are said one after the other only if every event has one;
/// otherwise the main agent words them all from `data`, which holds each.
fn merge(mut events: Vec<Event>) -> Event {
    if events.len() == 1 {
        return events.pop().expect("one");
    }
    let level = events.iter().map(|e| e.level).max().unwrap_or_default();
    let why = events.last().map(|e| e.why.clone()).unwrap_or_default();
    let say = events
        .iter()
        .map(|e| e.say.clone())
        .collect::<Option<Vec<_>>>()
        .map(|all| all.join(" "));
    let data = Value::Array(
        events
            .iter()
            .map(|e| e.data.clone().unwrap_or(Value::Null))
            .collect(),
    );
    Event { why, say, data: Some(data), level }
}

/// « Au fait, tu as reçu… », not « Au fait, Tu as reçu… ». Only the words a
/// sentence commonly opens with: a name keeps its capital.
fn lower_opening(say: &str) -> String {
    const OPENERS: &[&str] = &[
        "tu", "ton", "ta", "tes", "il", "elle", "ils", "elles", "on", "un", "une", "le", "la",
        "les", "l'", "des", "deux", "trois", "quatre", "cinq", "nouveau", "nouvelle", "ce", "cet",
        "cette", "ces", "j'ai", "je", "rien", "aucun", "aucune",
    ];
    let first: String = say
        .chars()
        .take_while(|c| c.is_alphabetic() || *c == '\'' || *c == '’')
        .collect::<String>()
        .to_lowercase()
        .replace('’', "'");
    let opens = OPENERS.contains(&first.as_str())
        || OPENERS.iter().any(|o| o.ends_with('\'') && first.starts_with(o));
    if !opens {
        return say.to_string();
    }
    let mut chars = say.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;

    fn at(h: u32, m: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 27)
            .unwrap()
            .and_hms_opt(h, m, 0)
            .unwrap()
    }

    fn event(say: &str, level: Level) -> Event {
        Event {
            why: "nouveau mail".into(),
            say: Some(say.into()),
            data: Some(serde_json::json!({ "id": say.len() })),
            level,
        }
    }

    fn none(_: &str) -> Rules {
        Rules::default()
    }

    /// The user's example, 2026-11-27: the mail waits for the answer to end,
    /// then follows it.
    #[test]
    fn news_waits_for_the_answer_then_follows_it() {
        let mut inbox = Inbox::new();
        inbox.push("mails", event("Tu as reçu un mail de Margaux pour le pique-nique.", Level::Normal), at(14, 0));
        assert!(inbox.take(at(14, 0), Floor::Busy, none).is_empty(), "never mid-answer");
        let out = inbox.take(at(14, 1), Floor::Between, none);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].how, How::Follow);
        assert_eq!(
            out[0].spoken().as_deref(),
            Some("Au fait, tu as reçu un mail de Margaux pour le pique-nique.")
        );
        assert!(inbox.take(at(14, 2), Floor::Free, none).is_empty(), "said once");
    }

    #[test]
    fn with_nobody_talking_aria_opens() {
        let mut inbox = Inbox::new();
        inbox.push("maj", event("Obsidian 1.9 est sortie.", Level::Normal), at(10, 0));
        let out = inbox.take(at(10, 0), Floor::Free, none);
        assert_eq!(out[0].how, How::Lead);
        assert_eq!(out[0].spoken().as_deref(), Some("Obsidian 1.9 est sortie."));
    }

    /// Six checks during a long exchange are one announcement.
    #[test]
    fn runs_that_fire_while_waiting_are_merged() {
        let mut inbox = Inbox::new();
        inbox.push("mails", event("Un mail de Margaux.", Level::Normal), at(14, 0));
        inbox.push("mails", event("Un mail de la banque.", Level::Urgent), at(14, 5));
        assert_eq!(inbox.waiting().len(), 1);
        let out = inbox.take(at(14, 6), Floor::Between, none);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].count, 2);
        assert_eq!(out[0].event.level, Level::Urgent);
        assert_eq!(out[0].since, at(14, 0));
        assert_eq!(out[0].spoken().as_deref(), Some("Au fait, un mail de Margaux. Un mail de la banque."));
        assert_eq!(out[0].event.data.as_ref().unwrap().as_array().unwrap().len(), 2);
    }

    #[test]
    fn a_merge_with_one_unworded_event_is_worded_by_the_agent() {
        let mut inbox = Inbox::new();
        inbox.push("mails", event("Un mail de Margaux.", Level::Normal), at(14, 0));
        inbox.push("mails", Event { say: None, ..event("", Level::Normal) }, at(14, 5));
        let out = inbox.take(at(14, 6), Floor::Free, none);
        assert_eq!(out[0].spoken(), None);
    }

    #[test]
    fn quiet_hours_hold_normal_not_urgent() {
        let rules = |_: &str| Rules {
            quiet: Some(Quiet {
                from: NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
                to: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
            }),
            max_per_day: None,
        };
        let mut inbox = Inbox::new();
        inbox.push("mails", event("Un mail.", Level::Normal), at(23, 0));
        inbox.push("serveur", event("Le serveur ne répond plus.", Level::Urgent), at(23, 0));
        let out = inbox.take(at(23, 0), Floor::Free, rules);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].hook, "serveur");
        let out = inbox.take(at(8, 0), Floor::Free, rules);
        assert_eq!(out[0].hook, "mails", "the window opened");
    }

    #[test]
    fn asleep_only_urgent_wakes_her() {
        let mut inbox = Inbox::new();
        inbox.push("mails", event("Un mail.", Level::Normal), at(9, 0));
        assert!(inbox.take(at(9, 0), Floor::Asleep, none).is_empty());
        inbox.push("serveur", event("Panne.", Level::Urgent), at(9, 1));
        let out = inbox.take(at(9, 1), Floor::Asleep, none);
        assert_eq!((out[0].hook.as_str(), out[0].how), ("serveur", How::Lead));
        let out = inbox.take(at(9, 2), Floor::Busy, none);
        assert!(out.is_empty());
        inbox.push("serveur", event("Panne.", Level::Urgent), at(9, 3));
        assert_eq!(inbox.take(at(9, 3), Floor::Busy, none)[0].how, How::Interject);
    }

    #[test]
    fn past_the_allowance_it_is_shown_not_said() {
        let rules = |_: &str| Rules { quiet: None, max_per_day: Some(1) };
        let mut inbox = Inbox::new();
        inbox.push("maj", event("Une.", Level::Normal), at(9, 0));
        assert_eq!(inbox.take(at(9, 0), Floor::Free, rules)[0].how, How::Lead);
        inbox.push("maj", event("Deux.", Level::Normal), at(10, 0));
        let out = inbox.take(at(10, 0), Floor::Busy, rules);
        assert_eq!(out[0].how, How::Notify, "shown at once, even mid-answer");
        assert_eq!(out[0].spoken(), None);
        // The next day starts again.
        let tomorrow = at(9, 0) + chrono::Duration::days(1);
        inbox.push("maj", event("Trois.", Level::Normal), tomorrow);
        assert_eq!(inbox.take(tomorrow, Floor::Free, rules)[0].how, How::Lead);
    }

    #[test]
    fn info_is_never_said() {
        let mut inbox = Inbox::new();
        inbox.push("disque", event("Disque à 80 %.", Level::Info), at(9, 0));
        let out = inbox.take(at(9, 0), Floor::Busy, none);
        assert_eq!(out[0].how, How::Notify);
    }

    #[test]
    fn a_name_keeps_its_capital_after_the_link() {
        assert_eq!(lower_opening("Margaux t'a écrit."), "Margaux t'a écrit.");
        assert_eq!(lower_opening("Tu as reçu deux mails."), "tu as reçu deux mails.");
        assert_eq!(lower_opening("L'imprimante n'a plus d'encre."), "l'imprimante n'a plus d'encre.");
        assert_eq!(lower_opening("Obsidian 1.9 est sortie."), "Obsidian 1.9 est sortie.");
    }

    #[test]
    fn forgetting_a_hook_drops_its_news() {
        let mut inbox = Inbox::new();
        inbox.push("mails", event("Un mail.", Level::Normal), at(9, 0));
        inbox.forget("mails");
        assert!(inbox.take(at(9, 0), Floor::Free, none).is_empty());
    }
}
