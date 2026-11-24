//! Speaking to `llama-server`: one POST, a stream of server-sent events back.
//!
//! The OpenAI-compatible endpoint, so `llama-server` here and vLLM — or
//! `mistral.rs` — on another machine, without this module changing.
//!
//! A new connection per request. It is loopback, a handshake costs well under a
//! millisecond, and a pooled connection is a second thing that can be in a bad
//! state when the server restarts underneath it.

use std::collections::BTreeMap;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};

/// `127.0.0.1:<port>`.
#[derive(Clone, Debug)]
pub struct Client {
    authority: String,
}

/// One tool call as the stream delivers it, reassembled.
#[derive(Clone, Debug, PartialEq)]
pub struct Call {
    pub id: String,
    pub name: String,
    /// Raw: a small model writes broken JSON occasionally, and the tool's
    /// refusal of it is more useful to the model than a parse error here.
    pub arguments: String,
}

/// What one streamed completion produced.
#[derive(Clone, Debug, Default)]
pub struct Completion {
    pub text: String,
    pub calls: Vec<Call>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// From the request to the first byte of text or call. The figure the
    /// comparison is about.
    pub first_ms: Option<u128>,
    pub total_ms: u128,
}

impl Client {
    pub fn new(port: u16) -> Self {
        Self {
            authority: format!("127.0.0.1:{port}"),
        }
    }

    /// Whether the server answers `/health` with a success.
    ///
    /// `llama-server` answers 503 while the model loads, which is exactly the
    /// distinction wanted: up is not the same as ready.
    pub async fn healthy(&self) -> bool {
        match self.send("GET", "/health", None).await {
            Ok(response) => response.status().is_success(),
            Err(_) => false,
        }
    }

    /// A completion, streamed. `on_text` receives each fragment of text as it
    /// arrives — the pilot speaks sentences before the turn is over.
    pub async fn stream(
        &self,
        mut body: Value,
        mut on_text: impl FnMut(&str),
    ) -> Result<Completion> {
        body["stream"] = json!(true);
        body["stream_options"] = json!({ "include_usage": true });
        let started = Instant::now();
        let response = self
            .send("POST", "/v1/chat/completions", Some(body.to_string()))
            .await?;
        let status = response.status();
        let mut body = response.into_body();
        if !status.is_success() {
            let bytes = body.collect().await.map(|b| b.to_bytes()).unwrap_or_default();
            bail!(
                "llama-server a répondu {status} : {}",
                String::from_utf8_lossy(&bytes).chars().take(400).collect::<String>()
            );
        }

        let mut completion = Completion::default();
        let mut calls: BTreeMap<u64, Call> = BTreeMap::new();
        let mut buffer = Vec::<u8>::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.context("le flux de llama-server s'est interrompu")?;
            let Some(data) = frame.data_ref() else {
                continue;
            };
            buffer.extend_from_slice(data);
            while let Some(end) = buffer.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = buffer.drain(..=end).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(payload) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let payload = payload.trim();
                if payload == "[DONE]" {
                    break;
                }
                let Ok(chunk) = serde_json::from_str::<Value>(payload) else {
                    continue;
                };
                if let Some(error) = chunk.get("error") {
                    bail!("llama-server : {error}");
                }
                absorb(&chunk, &mut completion, &mut calls, &started, &mut on_text);
            }
        }
        completion.calls = calls.into_values().filter(|c| !c.name.is_empty()).collect();
        completion.total_ms = started.elapsed().as_millis();
        Ok(completion)
    }

    /// A completion, not streamed, for the transcript nobody is waiting on.
    pub async fn complete(&self, mut body: Value) -> Result<String> {
        body["stream"] = json!(false);
        let response = self
            .send("POST", "/v1/chat/completions", Some(body.to_string()))
            .await?;
        let status = response.status();
        let bytes = response.into_body().collect().await?.to_bytes();
        if !status.is_success() {
            bail!(
                "llama-server a répondu {status} : {}",
                String::from_utf8_lossy(&bytes).chars().take(400).collect::<String>()
            );
        }
        let value: Value = serde_json::from_slice(&bytes)?;
        Ok(value["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string())
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<String>,
    ) -> Result<hyper::Response<hyper::body::Incoming>> {
        let stream = tokio::net::TcpStream::connect(&self.authority)
            .await
            .with_context(|| format!("llama-server injoignable sur {}", self.authority))?;
        let (mut sender, connection) =
            hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
        tokio::spawn(async move {
            // Ends with the response body; an error here is the server going
            // away mid-stream, which the reader reports on its own.
            let _ = connection.await;
        });
        let request = hyper::Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, &self.authority)
            .header(hyper::header::CONTENT_TYPE, "application/json")
            .body(Full::new(Bytes::from(body.unwrap_or_default())))?;
        Ok(sender.send_request(request).await?)
    }
}

/// Fold one SSE chunk into the completion.
///
/// Separate so it can be tested on chunks as `llama-server` writes them,
/// without a server.
fn absorb(
    chunk: &Value,
    completion: &mut Completion,
    calls: &mut BTreeMap<u64, Call>,
    started: &Instant,
    on_text: &mut impl FnMut(&str),
) {
    if let Some(usage) = chunk.get("usage").filter(|u| !u.is_null()) {
        completion.prompt_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0);
        completion.completion_tokens = usage["completion_tokens"].as_u64().unwrap_or(0);
    }
    let Some(choices) = chunk["choices"].as_array() else {
        return;
    };
    for choice in choices {
        let delta = &choice["delta"];
        if let Some(text) = delta["content"].as_str().filter(|t| !t.is_empty()) {
            completion.first_ms.get_or_insert_with(|| started.elapsed().as_millis());
            completion.text.push_str(text);
            on_text(text);
        }
        if let Some(parts) = delta["tool_calls"].as_array() {
            completion.first_ms.get_or_insert_with(|| started.elapsed().as_millis());
            for part in parts {
                // Calls arrive in pieces, keyed by index: the name once, the
                // arguments a few characters at a time.
                let index = part["index"].as_u64().unwrap_or(0);
                let slot = calls.entry(index).or_insert_with(|| Call {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });
                if let Some(id) = part["id"].as_str().filter(|s| !s.is_empty()) {
                    slot.id = id.to_string();
                }
                if let Some(name) = part["function"]["name"].as_str().filter(|s| !s.is_empty()) {
                    slot.name = name.to_string();
                }
                if let Some(arguments) = part["function"]["arguments"].as_str() {
                    slot.arguments.push_str(arguments);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fold(chunks: &[Value]) -> (Completion, String) {
        let mut completion = Completion::default();
        let mut calls = BTreeMap::new();
        let mut spoken = String::new();
        let started = Instant::now();
        for chunk in chunks {
            absorb(chunk, &mut completion, &mut calls, &started, &mut |t| {
                spoken.push_str(t)
            });
        }
        completion.calls = calls.into_values().collect();
        (completion, spoken)
    }

    /// Text and a call in the same turn — Gemma 4's behaviour in the POC: it
    /// says « Je vais vérifier ça » and delegates in one answer.
    #[test]
    fn a_call_is_reassembled_from_its_pieces_and_the_text_streams() {
        let (completion, spoken) = fold(&[
            json!({"choices":[{"delta":{"content":"Je vais "}}]}),
            json!({"choices":[{"delta":{"content":"vérifier."}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"delegate_task","arguments":"{\"task\":"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"compter\"}"}}]}}]}),
            json!({"choices":[],"usage":{"prompt_tokens":1187,"completion_tokens":20}}),
        ]);
        assert_eq!(spoken, "Je vais vérifier.");
        assert_eq!(completion.text, spoken);
        assert_eq!(
            completion.calls,
            vec![Call {
                id: "c1".into(),
                name: "delegate_task".into(),
                arguments: "{\"task\":\"compter\"}".into(),
            }]
        );
        assert_eq!(completion.prompt_tokens, 1187);
        assert!(completion.first_ms.is_some());
    }

    /// A `usage: null` on every chunk but the last is what llama-server
    /// writes; it must not zero the count the last one carries.
    #[test]
    fn a_null_usage_does_not_erase_the_real_one() {
        let (completion, _) = fold(&[
            json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2}}),
            json!({"choices":[{"delta":{"content":"x"}}],"usage":null}),
        ]);
        assert_eq!(completion.prompt_tokens, 10);
    }
}
