//! The conversation, held by a local model: history, tool loop, speech.
//!
//! One task owns the history and processes what it is told **in order**, the
//! way the CLI does: a second utterance arriving mid-answer waits in the
//! channel. What comes out is the [`Speech`] stream the pilot already reads.
//!
//! # The tool loop is ours
//!
//! With the CLI, a tool call went out over MCP and came back inside the CLI's
//! own loop. Here the loop is this module: the model answers with calls, each
//! is run through [`VoiceTools::call`] — the very handlers the MCP server uses,
//! so the two posts cannot behave differently — and the model is asked again
//! with the results, up to [`MAX_ROUNDS`] times.
//!
//! `delegate_task` still answers `PENDING` at once, and the result still comes
//! back later as a user message ([`Told::Returned`]). The POC verified that
//! history shape against `llama-server`: `user → assistant+tool_calls → tool →
//! user → assistant`.

use std::sync::Arc;

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use super::client::{Call, Client};
use super::sidecar::Sidecar;
use super::{HEARD_PLACEHOLDER, LocalConfig, LocalModel, wav};
use crate::Core;
use crate::agent::clock;
use crate::agent::conversation::Told;
use crate::agent::session::Speech;
use crate::mcp::voice::VoiceTools;
use crate::providers::Usage;

/// Rounds of tool calls in one turn before the model is cut off. `recall`
/// then `deep_recall` then an answer is three; a fourth is a loop.
const MAX_ROUNDS: usize = 4;

/// Enough for a spoken answer. A voice that reads four hundred tokens aloud
/// has already lost its listener.
const MAX_TOKENS: u32 = 400;

/// How many history messages are kept. Cut on a user message, so no
/// `tool_call_id` is left orphaned — which most servers reject.
const HISTORY_MESSAGES: usize = 80;

/// The slot the conversation lives in, and the one transcripts use. Two, so a
/// transcript never evicts the conversation's cached prefix: with one slot,
/// every transcript would make the next turn re-read the whole prompt.
const CONVERSATION_SLOT: u32 = 0;
const TRANSCRIPT_SLOT: u32 = 1;

/// What the model is told after announcing work it did not start. Written as
/// the system talking, so the history does not read as the user insisting.
const NUDGE: &str = "[Note d'ARIA, pas de l'utilisateur] Tu viens d'annoncer ou de refuser \
                     une tâche sans appeler delegate_task : rien n'a été lancé. Appelle \
                     delegate_task maintenant, avec la demande de l'utilisateur telle quelle \
                     dans task. Si ce n'était pas une tâche, réponds seulement « … ».";

/// Whether an answer promises or refuses work instead of doing it.
///
/// Short answers only: a story or a joke may well start with « je vais », and
/// is the answer, not a promise. « Je vais bien » is small talk.
fn announces_work(text: &str) -> bool {
    let text = text.trim().to_lowercase().replace('’', "'");
    if text.is_empty() || text.chars().count() > 220 {
        return false;
    }
    if text.starts_with("je vais bien") || text.starts_with("je vais très bien") {
        return false;
    }
    const PROMISES: [&str; 4] = ["je vais ", "je m'en occupe", "je lance ", "je cherche "];
    const REFUSALS: [&str; 4] = [
        "je ne peux pas",
        "je n'ai pas accès",
        "je ne suis pas en mesure",
        "je n'ai pas la possibilité",
    ];
    PROMISES.iter().any(|p| text.starts_with(p)) || REFUSALS.iter().any(|r| text.contains(r))
}

/// What the model is told when asked for a transcript.
const TRANSCRIBE: &str = "Transcris mot pour mot ce message audio, dans sa langue, \
                          sans rien ajouter ni répondre.";

/// A loaded local model, before it is wired in.
pub struct LocalAgent {
    sidecar: Sidecar,
    brain: Brain,
}

/// The half the pilot speaks through. Dropping it kills `llama-server` and
/// ends the conversation task.
pub struct Input {
    tx: mpsc::Sender<Told>,
    sidecar: Sidecar,
}

impl Input {
    pub async fn say(&self, told: &Told) -> Result<()> {
        self.tx
            .send(told.clone())
            .await
            .map_err(|_| anyhow!("la conversation locale est arrêtée"))
    }

    /// What `llama-server` last wrote, for an error message.
    pub async fn stderr(&self) -> String {
        self.sidecar.tail().await
    }
}

struct Brain {
    client: Client,
    model: LocalModel,
    system: String,
    tools: Vec<Value>,
    history: Vec<Value>,
    core: Arc<Core>,
    voice_tools: VoiceTools,
    transcribe_after: bool,
}

impl LocalAgent {
    /// Start `llama-server` and wait until its model is in memory.
    pub async fn start(core: Arc<Core>, config: LocalConfig) -> Result<Self> {
        let sidecar = Sidecar::start(&config).await?;
        let brain = Brain {
            client: Client::new(config.port),
            model: config.model,
            system: config.system_prompt(),
            tools: openai_tools(),
            history: Vec::new(),
            voice_tools: VoiceTools::new(Arc::clone(&core)),
            core,
            transcribe_after: config.transcribe_after,
        };
        Ok(Self { sidecar, brain })
    }

    /// One real request before anyone speaks: the prompt, the six schemas and
    /// a second of silence as audio.
    ///
    /// Loaded is not warm. The first request pays for the audio encoder's
    /// first run and for reading the prompt and the schemas — about a thousand
    /// tokens. Paid here, behind the loading bar, that prefix then stays in the
    /// slot's cache and the user's first sentence only pays for itself. The
    /// same reasoning as `aria_voice::engine::Engine::warm_up` and
    /// `MainAgent::warm_up`, one floor up.
    pub async fn warm_up(&mut self) -> Result<()> {
        let started = std::time::Instant::now();
        let silence = vec![0f32; wav::SAMPLE_RATE as usize];
        let messages = vec![
            self.brain.system_message(),
            json!({
                "role": "user",
                "content": audio_content(&clock::stamp(clock::now()), &silence),
            }),
        ];
        let completion = self
            .brain
            .client
            .stream(self.brain.request(&messages, 1), |_| {})
            .await
            .map_err(|e| anyhow!("le tour de chauffe a échoué : {e:#}"))?;
        tracing::info!(
            modele = self.brain.model.display_name,
            ms = started.elapsed().as_millis(),
            prompt_tokens = completion.prompt_tokens,
            "modèle local préchauffé : la première phrase ne paiera ni le prompt ni l'encodeur audio"
        );
        Ok(())
    }

    /// Hand the stream out and start the conversation task.
    pub fn split(self) -> (Input, mpsc::Receiver<Speech>, tokio::task::JoinHandle<()>) {
        let (tx, rx) = mpsc::channel(64);
        // Wide: text fragments are pushed from inside the stream reader, which
        // cannot wait, and a dropped fragment is a word missing out loud.
        let (out, speech) = mpsc::channel(4096);
        let task = tokio::spawn(self.brain.run(rx, out));
        (
            Input {
                tx,
                sidecar: self.sidecar,
            },
            speech,
            task,
        )
    }
}

impl Brain {
    async fn run(mut self, mut told: mpsc::Receiver<Told>, out: mpsc::Sender<Speech>) {
        while let Some(message) = told.recv().await {
            self.turn(message, &out).await;
        }
        tracing::info!("plus rien à dire au modèle local, arrêt");
    }

    fn system_message(&self) -> Value {
        json!({ "role": "system", "content": self.system })
    }

    fn request(&self, messages: &[Value], max_tokens: u32) -> Value {
        json!({
            "model": self.model.id,
            "messages": messages,
            "tools": self.tools,
            "tool_choice": "auto",
            "temperature": 0.2,
            "max_tokens": max_tokens,
            "cache_prompt": true,
            "id_slot": CONVERSATION_SLOT,
        })
    }

    async fn turn(&mut self, told: Told, out: &mpsc::Sender<Speech>) {
        // What goes to the model now, and what the history keeps. They differ
        // only for a heard turn: the audio is sent once, then replaced by
        // text — see the module header of `super`.
        let (now, kept, audio) = match &told {
            Told::Heard { at, audio } => (
                json!({ "role": "user", "content": audio_content(&clock::stamp(*at), audio) }),
                json!({ "role": "user", "content": told.for_agent() }),
                Some((*at, audio.clone())),
            ),
            _ => {
                let message = json!({ "role": "user", "content": told.for_agent() });
                (message.clone(), message, None)
            }
        };

        let mut messages = Vec::with_capacity(self.history.len() + 2);
        messages.push(self.system_message());
        messages.extend(self.history.iter().cloned());
        messages.push(now);
        let heard_at = self.history.len();
        let mut added = vec![kept];

        let mut spoken = String::new();
        let mut usage = Usage::default();
        // Set once the model has been reminded to delegate: from then on
        // nothing it writes is said, because its announcement already was.
        let mut silent = false;
        let mut called = false;
        let from_user = matches!(told, Told::Heard { .. } | Told::Said { .. });
        for round in 0..MAX_ROUNDS {
            let mut first_of_round = true;
            let mut unstamp = Unstamp::default();
            let completion = self
                .client
                .stream(self.request(&messages, MAX_TOKENS), |fragment| {
                    if let Some(clean) = unstamp.push(fragment)
                        && !silent
                    {
                        emit(&mut spoken, &mut first_of_round, out, &clean);
                    }
                })
                .await;
            if let Some(rest) = unstamp.finish()
                && !silent
            {
                emit(&mut spoken, &mut first_of_round, out, &rest);
            }
            let native = unstamp.native_call();
            let swallowed = unstamp.state == Head::Call;
            let mut completion = match completion {
                Ok(completion) => completion,
                Err(e) => {
                    let _ = out.send(Speech::Failed(format!("{e:#}"))).await;
                    return;
                }
            };
            if swallowed {
                // The call came as text: never spoken, and either recovered
                // or dropped — a JSON-ish string read aloud is the one outcome
                // worse than losing the call.
                match native {
                    Some(call) if completion.calls.is_empty() => {
                        tracing::info!(outil = %call.name, "appel natif rattrapé dans le texte");
                        completion.calls = vec![call];
                    }
                    Some(_) => {}
                    None => tracing::warn!(
                        brut = %unstamp.held.chars().take(400).collect::<String>(),
                        "le modèle a écrit un appel d'outil illisible — ni prononcé ni exécuté"
                    ),
                }
                completion.text.clear();
            }
            tracing::info!(
                modele = self.model.display_name,
                tour = round,
                premier_ms = ?completion.first_ms,
                total_ms = completion.total_ms,
                prompt_tokens = completion.prompt_tokens,
                appels = completion.calls.len(),
                "modèle local"
            );
            usage.input_tokens += completion.prompt_tokens;
            usage.output_tokens += completion.completion_tokens;

            // Kept without the stamp too: a history full of answers that start
            // with one teaches the model to keep doing it.
            let text = strip_stamp(&completion.text).to_string();
            if completion.calls.is_empty() {
                let answer = json!({ "role": "assistant", "content": text });
                messages.push(answer.clone());
                added.push(answer);
                // Measured on 2026-11-19 with Gemma 4 E4B: « un topo complet
                // sur la météo à Montpellier demain » got « Je vais faire un
                // topo… » and no call at all, then « Je ne peux pas… » when
                // asked again. The announcement is a fine waiting phrase; the
                // missing call is the failure. One silent round to make it.
                if from_user && !called && !silent && announces_work(&text) {
                    tracing::warn!(
                        reponse = %text,
                        "le modèle local annonce une tâche sans l'avoir lancée : rappel"
                    );
                    let nudge = json!({ "role": "user", "content": NUDGE });
                    messages.push(nudge.clone());
                    added.push(nudge);
                    silent = true;
                    continue;
                }
                break;
            }
            called = true;

            let calls: Vec<Call> = completion
                .calls
                .into_iter()
                .enumerate()
                .map(|(index, mut call)| {
                    if call.id.is_empty() {
                        call.id = format!("call_{round}_{index}");
                    }
                    call
                })
                .collect();
            let assistant = json!({
                "role": "assistant",
                "content": if text.is_empty() { Value::Null } else { json!(text) },
                "tool_calls": calls.iter().map(|call| json!({
                    "id": call.id,
                    "type": "function",
                    "function": { "name": call.name, "arguments": call.arguments },
                })).collect::<Vec<_>>(),
            });
            messages.push(assistant.clone());
            added.push(assistant);

            for call in &calls {
                let name = bare(&call.name);
                let _ = out
                    .send(Speech::ToolUse {
                        name: name.to_string(),
                        input: serde_json::from_str(&call.arguments)
                            .unwrap_or_else(|_| Value::String(call.arguments.clone())),
                    })
                    .await;
                let result = self.execute(name, &call.arguments).await;
                let message = json!({
                    "role": "tool",
                    "tool_call_id": call.id,
                    "content": result,
                });
                messages.push(message.clone());
                added.push(message);
            }
            if round + 1 == MAX_ROUNDS {
                tracing::warn!("le modèle local appelle encore des outils après {MAX_ROUNDS} tours : coupé");
            }
        }

        let _ = out
            .send(Speech::Finished {
                text: spoken,
                usage,
            })
            .await;
        self.history.extend(added);

        if let Some((at, audio)) = audio
            && self.transcribe_after
        {
            self.transcribe(heard_at, at, &audio).await;
        }
        trim(&mut self.history);
    }

    /// Replace a heard turn's placeholder with what was actually said.
    ///
    /// After the answer, on its own slot: it costs the next turn nothing but
    /// the GPU time, and the turn after that has the words rather than
    /// « (message vocal) » to go on — which is what « et après-demain ? » needs.
    async fn transcribe(&mut self, index: usize, at: clock::Stamp, audio: &[f32]) {
        let body = json!({
            "model": self.model.id,
            "messages": [{ "role": "user", "content": [
                { "type": "input_audio", "input_audio": { "data": wav::base64(audio), "format": "wav" } },
                { "type": "text", "text": TRANSCRIBE },
            ]}],
            "temperature": 0.0,
            "max_tokens": 256,
            "cache_prompt": false,
            "id_slot": TRANSCRIPT_SLOT,
        });
        let started = std::time::Instant::now();
        match self.client.complete(body).await {
            Ok(text) if !text.is_empty() => {
                tracing::info!(ms = started.elapsed().as_millis(), "transcription a posteriori");
                tracing::trace!("entendu : {text}");
                if let Some(message) = self.history.get_mut(index) {
                    message["content"] = json!(clock::stamped(at, &text));
                }
                self.core.push_transcript(text, true);
            }
            Ok(_) => tracing::debug!("transcription a posteriori vide"),
            Err(e) => tracing::warn!("transcription a posteriori impossible : {e:#}"),
        }
    }

    async fn execute(&self, name: &str, arguments: &str) -> String {
        let parsed = if arguments.trim().is_empty() {
            Ok(Value::Object(Default::default()))
        } else {
            serde_json::from_str::<Value>(arguments)
        };
        let arguments = match parsed {
            Ok(Value::Object(map)) => map,
            // Said to the model, which can retry; a 4B model writes broken
            // JSON now and then, and silence would end its turn.
            _ => return "Les arguments de l'appel ne sont pas un objet JSON valide.".to_string(),
        };
        tool_text(&self.voice_tools.call(name, arguments).await)
    }
}

/// Send one cleaned fragment to the pilot, and keep it for the final text.
///
/// Two rounds are one answer out loud: a space between them, or « Je
/// regarde.Il fait 23 °C » reaches the TTS.
fn emit(spoken: &mut String, first_of_round: &mut bool, out: &mpsc::Sender<Speech>, fragment: &str) {
    let mut fragment = fragment.to_string();
    if *first_of_round
        && !spoken.is_empty()
        && !spoken.ends_with(char::is_whitespace)
        && !fragment.starts_with(char::is_whitespace)
    {
        fragment.insert(0, ' ');
    }
    *first_of_round = false;
    spoken.push_str(&fragment);
    if out.try_send(Speech::Delta(fragment)).is_err() {
        tracing::warn!("un fragment de réponse n'a pas pu être transmis");
    }
}

/// What the start of an answer must not be read aloud as.
///
/// Two things a local model writes at the head of its text, both measured on
/// 2026-11-18 with Gemma 4 E4B:
///
/// - **the stamp**, copied: every message it receives starts with one
///   (decision 107) and it imitates the pattern — « [2026-11-18 18:57] Sept
///   fois huit font cinquante-six. » The CLI's Haiku never did;
/// - **a tool call in its own format**, now and then, when it drops the token
///   that opens one: `delegate_task{model:<|"|>rapide<|"|>,…}`. `llama-server`
///   then cannot recognise it, it arrives as text, and the TTS would read it;
/// - **the same call written as Python**, seen on 2026-11-19:
///   `delegate_task(model="rapide", task="…")`, sometimes inside a code fence
///   or a `print(…)` — Gemma's `tool_code` habit. Shown on screen and read
///   aloud until then, and the task never ran.
///
/// Text is held back only while its start could still be one of these, so an
/// ordinary answer is not delayed by more than its first word.
struct Unstamp {
    held: String,
    state: Head,
    /// The tool names a native call may start with.
    names: Vec<&'static str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Head {
    Undecided,
    Speech,
    /// A native call: held to the end of the round, never spoken.
    Call,
}

/// What opens a Gemma-format call when the model does write its opener.
const NATIVE_OPENERS: [&str; 2] = ["<|tool_call>", "call:"];

/// What a call written as code may be wrapped in, outermost first. A spoken
/// answer never starts with any of these, so holding them costs nothing.
const CODE_OPENERS: [&str; 6] = ["```tool_code", "```python", "```json", "```", "`", "print("];

impl Default for Unstamp {
    fn default() -> Self {
        Self {
            held: String::new(),
            state: Head::Undecided,
            names: crate::mcp::voice::published().iter().map(|s| s.name).collect(),
        }
    }
}

impl Unstamp {
    /// What may be spoken of this fragment, if anything yet.
    fn push(&mut self, fragment: &str) -> Option<String> {
        match self.state {
            Head::Speech => return Some(fragment.to_string()),
            Head::Call => {
                self.held.push_str(fragment);
                return None;
            }
            Head::Undecided => {}
        }
        self.held.push_str(fragment);
        let mut start = self.held.trim_start();
        if start.is_empty() {
            return None;
        }
        // A stamp first — it may be followed by anything, a call included.
        if start.starts_with('[') {
            match start.find(']') {
                Some(end) if is_stamp(&start[1..end]) => {
                    self.held = start[end + 1..].trim_start().to_string();
                    start = self.held.as_str();
                    if start.is_empty() {
                        return None;
                    }
                }
                Some(_) => return self.release(),
                // Longer than any stamp and still open: not one.
                None if start.chars().count() > 20 => return self.release(),
                None => return None,
            }
        }
        match self.call_head(start) {
            Some(true) => {
                self.state = Head::Call;
                self.held = start.to_string();
                None
            }
            // Could still become one: wait for the next fragment.
            None => None,
            Some(false) => self.release(),
        }
    }

    /// `Some(true)` for a native call, `Some(false)` for speech, `None` while
    /// the text so far is a prefix of either.
    fn call_head(&self, start: &str) -> Option<bool> {
        let openers = NATIVE_OPENERS
            .iter()
            .chain(CODE_OPENERS.iter())
            .map(|o| o.to_string())
            .chain(self.names.iter().map(|name| format!("{name}{{")))
            .chain(self.names.iter().map(|name| format!("{name}(")));
        let mut undecided = false;
        for opener in openers {
            if start.starts_with(&opener) {
                return Some(true);
            }
            if opener.starts_with(start) {
                undecided = true;
            }
        }
        if undecided { None } else { Some(false) }
    }

    /// The end of the round: what to say of what was held, if anything.
    fn finish(&mut self) -> Option<String> {
        match self.state {
            Head::Undecided if !self.held.is_empty() => self.release(),
            _ => None,
        }
    }

    /// The native call written as text, if that is what the round was.
    fn native_call(&self) -> Option<Call> {
        (self.state == Head::Call)
            .then(|| parse_native_call(&self.held, &self.names))
            .flatten()
    }

    fn release(&mut self) -> Option<String> {
        self.state = Head::Speech;
        let text = std::mem::take(&mut self.held);
        let text = text.trim_start().to_string();
        (!text.is_empty()).then_some(text)
    }
}

/// `delegate_task{model:<|"|>rapide<|"|>,task:<|"|>…<|"|>}`, as a call.
///
/// Gemma 4's own syntax: bare keys, strings between `<|"|>`. Brought back to
/// JSON by quoting the keys and swapping the delimiters — enough for the flat
/// arguments the six tools take, and anything it cannot read is dropped
/// rather than spoken.
fn parse_native_call(text: &str, names: &[&'static str]) -> Option<Call> {
    let mut s = text.trim();
    // The code wrapping first: a fence, a backtick, then `print(…)`.
    for fence in ["```tool_code", "```python", "```json", "```", "`"] {
        if let Some(inner) = s.strip_prefix(fence) {
            s = inner.trim();
            s = s.strip_suffix(if fence == "`" { "`" } else { "```" }).unwrap_or(s).trim();
            break;
        }
    }
    if let Some(inner) = s.strip_prefix("print(").and_then(|i| i.trim_end().strip_suffix(')')) {
        s = inner.trim();
    }
    for opener in NATIVE_OPENERS {
        s = s.strip_prefix(opener).unwrap_or(s).trim_start();
    }
    let s = s.trim_end().strip_suffix("<tool_call|>").unwrap_or(s).trim_end();
    // `name(key="value", …)`, the Python form.
    if let Some(paren) = s.find('(')
        && s[..paren].trim().chars().all(|c| c.is_alphanumeric() || c == '_')
    {
        let name = s[..paren].trim();
        if !names.contains(&name) {
            return None;
        }
        let arguments = parse_keywords(s[paren + 1..].strip_suffix(')')?)?;
        return Some(Call {
            id: String::new(),
            name: name.to_string(),
            arguments: Value::Object(arguments).to_string(),
        });
    }
    let brace = s.find('{')?;
    let name = s[..brace].trim();
    if !names.contains(&name) || !s.ends_with('}') {
        return None;
    }
    let body = s[brace..].replace("<|\"|>", "\u{1}");
    // Quote the bare keys: an identifier right after `{` or `,`, outside a
    // string, and followed by `:`.
    let mut json = String::with_capacity(body.len() + 16);
    let mut in_string = false;
    let mut chars = body.chars().peekable();
    let mut expect_key = false;
    while let Some(c) = chars.next() {
        match c {
            '\u{1}' => {
                in_string = !in_string;
                json.push('"');
            }
            // Gemma's strings are raw: `D:\Code\ARIA` is three backslashes
            // meant literally, and `\P` is not a JSON escape. Measured on
            // 2026-11-18 — the bench's first Gemma delegation was lost to it.
            '"' if in_string => json.push_str("\\\""),
            '\\' if in_string => json.push_str("\\\\"),
            '\n' if in_string => json.push_str("\\n"),
            '\r' if in_string => json.push_str("\\r"),
            '\t' if in_string => json.push_str("\\t"),
            '{' | ',' if !in_string => {
                json.push(c);
                expect_key = true;
            }
            c if expect_key && !in_string && (c.is_alphanumeric() || c == '_') => {
                let mut key = String::from(c);
                while let Some(&next) = chars.peek() {
                    if next.is_alphanumeric() || next == '_' {
                        key.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                json.push('"');
                json.push_str(&key);
                json.push('"');
                expect_key = false;
            }
            c => {
                if !c.is_whitespace() {
                    expect_key = false;
                }
                json.push(c);
            }
        }
    }
    let arguments: Value = serde_json::from_str(&json).ok()?;
    arguments.is_object().then(|| Call {
        id: String::new(),
        name: name.to_string(),
        arguments: arguments.to_string(),
    })
}

/// `model="rapide", task='…', n=3` — Python keyword arguments, as JSON.
///
/// Strings in either quote with backslash escapes; anything unquoted is read
/// as JSON (`3`, `true`) or as Python's `True`, `False`, `None`. Whatever does
/// not fit returns `None`, and the call is dropped rather than spoken.
fn parse_keywords(body: &str) -> Option<serde_json::Map<String, Value>> {
    let mut map = serde_json::Map::new();
    let mut chars = body.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace() || *c == ',') {
            chars.next();
        }
        if chars.peek().is_none() {
            return Some(map);
        }
        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_alphanumeric() || c == '_' {
                key.push(c);
                chars.next();
            } else {
                break;
            }
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if key.is_empty() || chars.next() != Some('=') {
            return None;
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let value = match chars.peek().copied() {
            Some(quote @ ('"' | '\'')) => {
                chars.next();
                let mut text = String::new();
                loop {
                    match chars.next()? {
                        '\\' => match chars.next()? {
                            'n' => text.push('\n'),
                            't' => text.push('\t'),
                            c @ ('"' | '\'' | '\\') => text.push(c),
                            // A Windows path, meant literally.
                            c => {
                                text.push('\\');
                                text.push(c);
                            }
                        },
                        c if c == quote => break,
                        c => text.push(c),
                    }
                }
                Value::String(text)
            }
            _ => {
                let mut raw = String::new();
                while let Some(&c) = chars.peek() {
                    if c == ',' {
                        break;
                    }
                    raw.push(c);
                    chars.next();
                }
                match raw.trim() {
                    "True" => Value::Bool(true),
                    "False" => Value::Bool(false),
                    "None" => Value::Null,
                    other => serde_json::from_str(other).ok()?,
                }
            }
        };
        map.insert(key, value);
    }
}

/// `2026-11-18 18:57`, and nothing a sentence would contain.
fn is_stamp(inside: &str) -> bool {
    inside.len() >= 10
        && inside.chars().next().is_some_and(|c| c.is_ascii_digit())
        && inside
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '-' | ':' | ' '))
}

/// The same, on a whole text.
fn strip_stamp(text: &str) -> &str {
    let start = text.trim_start();
    if let Some(inner) = start.strip_prefix('[')
        && let Some(end) = inner.find(']')
        && is_stamp(&inner[..end])
    {
        return inner[end + 1..].trim_start();
    }
    text
}

/// The content of a heard turn: the stamp, then the audio.
fn audio_content(stamp: &str, audio: &[f32]) -> Value {
    json!([
        { "type": "text", "text": stamp },
        { "type": "input_audio", "input_audio": { "data": wav::base64(audio), "format": "wav" } },
    ])
}

/// A tool name without the MCP prefix a model may have seen somewhere.
fn bare(name: &str) -> &str {
    name.rsplit("__").next().unwrap_or(name)
}

/// The text a tool answered, flattened.
fn tool_text(result: &rmcp::model::CallToolResult) -> String {
    let value = serde_json::to_value(result).unwrap_or_default();
    let text: Vec<&str> = value["content"]
        .as_array()
        .map(|parts| parts.iter().filter_map(|p| p["text"].as_str()).collect())
        .unwrap_or_default();
    text.join("\n")
}

/// The six tools, as the chat API wants them. Derived from the one catalogue,
/// like the MCP routes — a second copy is the copy that drifts, which is what
/// happened to the POC's `aria-tools.json`.
pub fn openai_tools() -> Vec<Value> {
    crate::mcp::voice::published()
        .into_iter()
        .map(|spec| {
            json!({
                "type": "function",
                "function": {
                    "name": spec.name,
                    "description": spec.description,
                    "parameters": spec.schema,
                },
            })
        })
        .collect()
}

/// Bound the history, cutting at a user message.
fn trim(history: &mut Vec<Value>) {
    if history.len() <= HISTORY_MESSAGES {
        return;
    }
    let mut cut = history.len() - HISTORY_MESSAGES;
    while cut < history.len() && history[cut]["role"] != "user" {
        cut += 1;
    }
    history.drain(..cut);
}

/// A heard turn as the history keeps it until a transcript arrives.
pub fn heard_placeholder(at: clock::Stamp) -> String {
    clock::stamped(at, HEARD_PLACEHOLDER)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tools_are_the_published_ones_and_nothing_else() {
        let names: Vec<String> = openai_tools()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"delegate_task".to_string()));
        assert!(names.contains(&"recall".to_string()));
        assert!(
            !names.contains(&"run_action".to_string()),
            "unbacked on the CLI post, unbacked here"
        );
        assert!(!names.contains(&"approve".to_string()));
    }

    /// Gemma 4's habit, measured: the stamp copied in front of the answer,
    /// split across fragments the way a stream splits it.
    #[test]
    fn a_copied_stamp_is_not_spoken() {
        let mut unstamp = Unstamp::default();
        let said: String = ["[2026-", "11-18 18", ":57] Sept", " fois huit."]
            .iter()
            .filter_map(|f| unstamp.push(f))
            .collect();
        assert_eq!(said, "Sept fois huit.");
        assert_eq!(strip_stamp("[2026-11-18 18:57] Sept."), "Sept.");
    }

    /// And nothing else is touched — a bracket that is not a stamp, or an
    /// answer that starts with a word, go through as they came.
    #[test]
    fn only_a_stamp_is_dropped() {
        let mut plain = Unstamp::default();
        assert_eq!(plain.push("Bonjour").as_deref(), Some("Bonjour"));
        assert_eq!(plain.push(" !").as_deref(), Some(" !"));

        let mut aside = Unstamp::default();
        assert_eq!(aside.push("[rire] ah").as_deref(), Some("[rire] ah"));

        let mut short = Unstamp::default();
        assert_eq!(short.push("[1"), None, "could still be a stamp");
        assert_eq!(short.finish().as_deref(), Some("[1"), "said at the end after all");
        assert_eq!(strip_stamp("[rire] ah"), "[rire] ah");
    }

    /// The fault measured on 2026-11-18: Gemma 4 dropped the token that opens
    /// a call, and the call came as text. Held, never spoken, and recovered.
    #[test]
    fn a_native_call_written_as_text_is_recovered_and_not_spoken() {
        let mut head = Unstamp::default();
        let said: Vec<String> = [
            "[2026-11-18 18:57] ",
            "delegate",
            "_task{model:<|\"|>rapide<|\"|>,",
            "task:<|\"|>compter les lignes du crate « core »<|\"|>}",
        ]
        .iter()
        .filter_map(|f| head.push(f))
        .collect();
        assert!(said.is_empty(), "{said:?}");
        assert_eq!(head.finish(), None);

        let call = head.native_call().expect("rattrapé");
        assert_eq!(call.name, "delegate_task");
        let arguments: Value = serde_json::from_str(&call.arguments).unwrap();
        assert_eq!(arguments["model"], "rapide");
        assert_eq!(arguments["task"], "compter les lignes du crate « core »");
    }

    /// The call that was lost on 2026-11-18, verbatim: a Windows path, whose
    /// backslashes are literal in Gemma's strings.
    #[test]
    fn a_windows_path_survives_the_native_format() {
        let raw = r#"delegate_task{model:<|"|>rapide<|"|>,project:<|"|>ARIA<|"|>,task:<|"|>Compter le nombre de membres et lister leurs noms dans le fichier D:\Code\ARIA\Cargo.toml<|"|>}"#;
        let call = parse_native_call(raw, &["delegate_task"]).expect("lu");
        let arguments: Value = serde_json::from_str(&call.arguments).unwrap();
        assert_eq!(
            arguments["task"],
            r"Compter le nombre de membres et lister leurs noms dans le fichier D:\Code\ARIA\Cargo.toml"
        );
        assert_eq!(arguments["project"], "ARIA");
    }

    /// With its opener, too — the form the server would normally have parsed.
    #[test]
    fn a_native_call_with_its_opener_is_read_the_same() {
        let names = ["recall"];
        let call = parse_native_call("<|tool_call>call:recall{query:<|\"|>café<|\"|>}<tool_call|>", &names)
            .expect("lu");
        assert_eq!(call.name, "recall");
        assert_eq!(call.arguments, r#"{"query":"café"}"#);
        assert!(parse_native_call("bonjour{x:1}", &names).is_none(), "not a tool");
    }

    /// The fault of 2026-11-19, verbatim: the call written as Python, shown on
    /// screen and read aloud, and the weather never asked for.
    #[test]
    fn a_call_written_as_python_is_recovered_and_not_spoken() {
        let mut head = Unstamp::default();
        let said: Vec<String> = [
            "[2026-11-19 10:02] ",
            "delegate_task(",
            "model=\"rapide\", task=\"Quel est le temps à Montpellier aujourd'hui ?\")",
        ]
        .iter()
        .filter_map(|f| head.push(f))
        .collect();
        assert!(said.is_empty(), "{said:?}");
        assert_eq!(head.finish(), None);
        let call = head.native_call().expect("rattrapé");
        assert_eq!(call.name, "delegate_task");
        let arguments: Value = serde_json::from_str(&call.arguments).unwrap();
        assert_eq!(arguments["model"], "rapide");
        assert_eq!(arguments["task"], "Quel est le temps à Montpellier aujourd'hui ?");
    }

    /// Gemma's `tool_code` habit: a fence and a `print`, around the same call.
    #[test]
    fn a_python_call_in_a_fence_or_a_print_is_read_the_same() {
        let names = ["delegate_task", "recall"];
        for raw in [
            "```tool_code\nprint(recall(query='café', limit=3))\n```",
            "`recall(query=\"café\", limit=3)`",
            "print(recall(query=\"café\", limit=3))",
        ] {
            let call = parse_native_call(raw, &names).unwrap_or_else(|| panic!("lu : {raw}"));
            assert_eq!(call.name, "recall");
            let arguments: Value = serde_json::from_str(&call.arguments).unwrap();
            assert_eq!(arguments["query"], "café", "{raw}");
            assert_eq!(arguments["limit"], 3, "{raw}");
        }
        assert!(parse_native_call("bonjour(x=1)", &names).is_none(), "not a tool");
        assert!(
            parse_native_call(r#"delegate_task(task="D:\Code\ARIA", project=None)"#, &names)
                .is_some_and(|c| c.arguments.contains(r#"D:\\Code\\ARIA"#)),
            "a Windows path stays literal"
        );
    }

    /// An answer whose first word is a tool's name with a parenthesis after a
    /// space is speech, not a call.
    #[test]
    fn a_sentence_that_mentions_a_tool_is_still_said() {
        let mut head = Unstamp::default();
        assert_eq!(head.push("Bonjour (enfin)."), Some("Bonjour (enfin).".into()));
    }

    /// A word that merely starts like a tool name is held for one fragment at
    /// most, then said whole.
    #[test]
    fn an_answer_that_starts_like_a_tool_is_said_whole() {
        let mut head = Unstamp::default();
        assert_eq!(head.push("re"), None);
        assert_eq!(head.push("tiens bien."), Some("retiens bien.".into()));
        assert_eq!(head.push(" Voilà"), Some(" Voilà".into()));
        assert!(head.native_call().is_none());
    }

    #[test]
    fn a_promise_or_a_refusal_without_a_call_is_caught() {
        // The two answers measured on 2026-11-19.
        assert!(announces_work(
            "Je vais faire un topo sur la météo à Montpellier demain complète avec aperçu \
             hydrométrique et le kit complet."
        ));
        assert!(announces_work(
            "Je ne peux pas faire un topo sur la météo à Montpellier demain."
        ));
        assert!(announces_work("Je n’ai pas accès à internet."));
    }

    #[test]
    fn small_talk_and_stories_are_left_alone() {
        assert!(!announces_work("Je vais bien, merci !"));
        assert!(!announces_work("Il est quinze heures."));
        assert!(!announces_work("Je ne sais pas ce que vous voulez dire."));
        let story = format!("Je vais te raconter une histoire. {}", "Il était une fois. ".repeat(15));
        assert!(!announces_work(&story));
    }

    #[test]
    fn a_prefixed_name_is_brought_back_to_the_catalogue() {
        assert_eq!(bare("mcp__aria__delegate_task"), "delegate_task");
        assert_eq!(bare("recall"), "recall");
    }

    /// Cutting inside an assistant/tool sequence would orphan a
    /// `tool_call_id`.
    #[test]
    fn the_history_is_cut_on_a_user_message() {
        let mut history = Vec::new();
        for i in 0..50 {
            history.push(json!({"role": "user", "content": i}));
            history.push(json!({"role": "assistant", "tool_calls": []}));
            history.push(json!({"role": "tool", "content": "ok"}));
        }
        trim(&mut history);
        assert!(history.len() <= HISTORY_MESSAGES);
        assert_eq!(history[0]["role"], "user");
    }

    /// A whole turn against a fake `llama-server`: the model calls `recall`,
    /// is asked again with the result, then answers. What the pilot sees and
    /// what the history keeps are both checked — the history shape is the one
    /// the POC verified against the real server.
    #[tokio::test]
    async fn a_turn_with_a_tool_call_runs_the_loop_and_keeps_its_shape() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let asked = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let app = {
            let asked = Arc::clone(&asked);
            let seen = Arc::clone(&seen);
            axum::Router::new().route(
                "/v1/chat/completions",
                axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                    let round = asked.fetch_add(1, Ordering::SeqCst);
                    seen.lock().unwrap().push(body);
                    async move {
                        let chunks: Vec<Value> = if round == 0 {
                            vec![
                                json!({"choices":[{"delta":{"content":"Je regarde."}}]}),
                                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"r1","function":{"name":"recall","arguments":"{\"query\":\"café\"}"}}]}}]}),
                            ]
                        } else {
                            vec![json!({"choices":[{"delta":{"content":"Rien sur le café."}}]})]
                        };
                        let mut body: String = chunks
                            .iter()
                            .map(|c| format!("data: {c}\n\n"))
                            .collect();
                        body.push_str("data: [DONE]\n\n");
                        ([(axum::http::header::CONTENT_TYPE, "text/event-stream")], body)
                    }
                }),
            )
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let core = Core::new();
        let mut brain = Brain {
            client: Client::new(port),
            model: super::super::GEMMA4_E4B,
            system: "système".into(),
            tools: openai_tools(),
            history: Vec::new(),
            voice_tools: VoiceTools::new(Arc::clone(&core)),
            core,
            transcribe_after: false,
        };
        let (out, mut speech) = mpsc::channel(64);
        brain.turn(Told::said("Tu sais quoi sur le café ?"), &out).await;
        drop(out);

        let mut deltas = String::new();
        let mut tools = Vec::new();
        let mut finished = None;
        while let Some(event) = speech.recv().await {
            match event {
                Speech::Delta(text) => deltas.push_str(&text),
                Speech::ToolUse { name, .. } => tools.push(name),
                Speech::Finished { text, .. } => finished = Some(text),
                other => panic!("inattendu : {other:?}"),
            }
        }
        assert_eq!(asked.load(Ordering::SeqCst), 2, "asked again after the tool");
        assert_eq!(tools, ["recall"]);
        assert_eq!(deltas, "Je regarde. Rien sur le café.", "two rounds, one answer");
        assert_eq!(finished.as_deref(), Some(deltas.as_str()));

        let roles: Vec<&str> = brain
            .history
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["user", "assistant", "tool", "assistant"]);
        assert!(brain.history[0]["content"].as_str().unwrap().starts_with('['), "stamped");
        assert_eq!(brain.history[2]["tool_call_id"], "r1");

        // The second request carried the tool's answer, on the conversation's
        // own slot.
        let second = &seen.lock().unwrap()[1];
        assert_eq!(second["id_slot"], CONVERSATION_SLOT);
        let last = second["messages"].as_array().unwrap().last().unwrap().clone();
        assert_eq!(last["role"], "tool");
    }

    #[test]
    fn a_tool_result_is_read_as_its_text() {
        let result = rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(
            "page 3 : café".to_string(),
        )]);
        assert_eq!(tool_text(&result), "page 3 : café");
    }
}
