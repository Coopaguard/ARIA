//! The voice: turning what ARIA says into something audible.
//!
//! Two pieces, and the split is the whole design:
//!
//! - a [`Voice`] **synthesises** text into samples and knows nothing about
//!   loudspeakers;
//! - a [`Player`] **plays** samples and knows nothing about text.
//!
//! Everything hard lives in the second one, which is why it is the one written
//! against the product's constraints rather than against a model's API.
//!
//! # Why a trait
//!
//! The original specification leaves the engine open on purpose — "les identifiants
//! de modèles vivent en configuration, jamais en dur". Two engines implement
//! it: [`phonon`], the one that speaks, and [`kyutai`], suspended since
//! 2026-11-19 but kept compiled. The Windows voice that used to be the default
//! and the fallback was removed that day: a machine whose model does not load
//! now answers in writing.
//!
//! # What the player owes the rest of the product
//!
//! | Owed to | What |
//! |---|---|
//! | decision 17 | playback **stops inside the frame** when the user speaks over it |
//! | the echo gate | an exact reference of what went to the speakers |
//! | the orb | whether sound is coming out, this instant |
//! | T0's budget | when the **first sample** left, not when synthesis finished |

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};

pub mod kyutai;
pub mod phonon;

/// Synthesised speech, ready to play.
#[derive(Clone, Debug)]
pub struct Utterance {
    /// Mono samples in `[-1, 1]`.
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl Utterance {
    pub fn seconds(&self) -> f32 {
        self.samples.len() as f32 / self.sample_rate as f32
    }
}

/// An engine that turns text into samples.
///
/// # Whole utterance, or a chunk at a time
///
/// The comment here used to say that chunked synthesis was the **caller's**
/// shape and would arrive by splitting sentences earlier, never by changing
/// this trait. That was a reasonable thing to believe with one engine whose
/// `SynthesizeTextToStreamAsync` hands back a finished buffer: nothing to
/// stream, so nothing to design for.
///
/// It stopped being true with the second real engine. Phonon decodes its
/// latents as it produces them and yields audio in six pieces for one
/// sentence, the first after about 70 ms — and no amount of splitting
/// sentences earlier recovers that, because the win is *inside* one sentence.
/// Splitting would also change what is said: a comma is not a full stop, and
/// the prosody of a clause depends on the clause that follows it.
///
/// So the trait carries both shapes, and [`Voice::speak`] stays the required
/// one. An engine that only has a finished buffer implements it and gets the
/// default [`Voice::stream`], which hands that buffer over as a single chunk —
/// so the caller has one code path and the engine owes nothing it cannot
/// provide.
pub trait Voice: Send + Sync {
    /// What this voice is called, for the settings screen and the log.
    fn name(&self) -> String;

    /// Synthesise one stretch of text, whole.
    fn speak(&self, text: &str) -> Result<Utterance>;

    /// Synthesise one stretch of text, handing over each piece as it exists.
    ///
    /// `on_chunk` is called **in order**, and an error from it stops the
    /// generation — which is what lets a player that has been silenced abandon
    /// the rest of a sentence rather than synthesise it for nobody.
    ///
    /// # Why the callback has to be `Send`, which it did not use to
    ///
    /// This said "on the calling thread", and it was true of both engines that
    /// existed when it was written. Kyutai's library breaks it: inside one
    /// sentence it runs the latent generation and the audio decoding
    /// concurrently, in a `std::thread::scope`, and the callback goes to the
    /// **decoder**. So the chunks still arrive in order and still all arrive
    /// before `stream` returns — the guarantee callers rely on — but not on
    /// the thread that asked.
    ///
    /// The bound is therefore the truth, and the alternative was worse: an
    /// `unsafe impl Send` wrapper promising on ARIA's side what the engine
    /// does not do. Nothing is lost, because every closure passed here already
    /// captures a queue behind an `Arc<Mutex<…>>` and two local numbers.
    ///
    /// The default is honest rather than fake: one chunk, once the whole thing
    /// is ready. An engine that cannot stream must not pretend to by cutting a
    /// finished buffer into pieces, because that moves no audio earlier and
    /// only makes the measurement lie.
    fn stream(
        &self,
        text: &str,
        on_chunk: &mut (dyn FnMut(Utterance) -> Result<()> + Send),
    ) -> Result<()> {
        on_chunk(self.speak(text)?)
    }

    /// Change the speaker without reloading the model.
    ///
    /// `Ok(false)` when this engine cannot, and the caller reopens it whole.
    /// Phonon can: a voice is an embedding registered on a loaded model, so a
    /// change costs one small file instead of a second copy of the weights.
    fn revoice(&self, _voice: &str) -> Result<bool> {
        Ok(false)
    }
}

/// What the player is doing, as the thing that feeds it needs to know.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Playing {
    Silent,
    Speaking,
}

/// The queue between synthesis and the loudspeakers.
///
/// One output stream for the life of the player, not one per utterance: opening
/// a WASAPI stream costs tens of milliseconds and the budget for a first
/// syllable is 1.2 s end to end. The stream runs continuously and reads
/// silence when there is nothing to say.
pub struct Player {
    shared: Arc<Mutex<Queue>>,
    /// Held so the stream stays alive. Never read.
    _stream: cpal::Stream,
    output_rate: u32,
    voice: Box<dyn Voice>,
    /// Phrases synthesised before anything asked for them, at the **output**
    /// rate so a hit is a memcpy into the queue and nothing else.
    ///
    /// See [`Player::prime`]. Keyed on the trimmed text, which is what
    /// [`Player::say`] looks up — so a phrase only ever hits its own entry.
    primed: Mutex<HashMap<String, Arc<Vec<f32>>>>,
}

/// What the audio callback reads, and the only thing the two threads share.
#[derive(Default)]
struct Queue {
    /// Samples at the **output** device's rate, already mixed down to mono.
    pending: Vec<f32>,
    /// Read position in `pending`.
    at: usize,
    /// Everything handed to the speakers since the last [`Player::silence`],
    /// kept for the echo gate to lock onto.
    ///
    /// Bounded, because a long answer must not grow without limit: the gate
    /// only ever looks at a recent window.
    reference: Vec<f32>,
    /// Which utterance the queue belongs to, bumped **only** by a barge-in.
    ///
    /// # Why a counter and not the length of `pending`
    ///
    /// [`Player::say`] has to know whether the queue it is filling is still
    /// its own, and it used to ask by remembering `pending.len()`. That was
    /// sound for as long as the length moved for exactly two reasons — our own
    /// append, and [`Player::silence`].
    ///
    /// It is not: the audio callback also clears `pending` when it has played
    /// it out, to keep a long session from carrying every sentence it ever
    /// said. **Spending the queue and being interrupted looked identical**, so
    /// a streaming engine whose next chunk arrived after the previous one had
    /// finished playing abandoned the rest of the sentence and logged a
    /// barge-in that never happened. Heard as ARIA saying the first few words
    /// and going quiet, or nothing at all when the first chunk was short.
    ///
    /// The counter separates the two: reclaiming is bookkeeping, interrupting
    /// is a decision.
    generation: u64,
    /// The 20 ms RMS envelope of `reference`, maintained as it is written.
    ///
    /// Kept here rather than reduced on demand, because the demand is **every
    /// microphone frame**: eight seconds of samples re-squared fifty times a
    /// second is nineteen million multiplications a second for a number the
    /// audio callback can accumulate for nothing.
    ///
    /// In the echo gate's frames, not the player's samples. Framing it here,
    /// where the output rate is known, is what stops the microphone side and
    /// this side from each cutting 20 ms their own way.
    envelope: Vec<f32>,
    /// Sum of squares for the frame being accumulated, and how many samples
    /// have gone into it.
    square_sum: f32,
    counted: usize,
}

/// How much played audio is kept as an echo reference.
///
/// Eight seconds, which is the window the turn detector already works in, and
/// far more than the coupling delay the gate measures in milliseconds.
const REFERENCE_SECONDS: usize = 8;

impl Queue {
    /// Give back what has been played. Bookkeeping, not a decision: whoever is
    /// still filling this queue keeps the right to extend it.
    fn reclaim(&mut self) {
        self.pending.clear();
        self.at = 0;
    }

    /// Decision 17: drop the queue, and make it a *different* queue.
    fn interrupt(&mut self) {
        self.reclaim();
        self.generation += 1;
    }

    fn is_speaking(&self) -> bool {
        self.at < self.pending.len()
    }
}

impl Player {
    /// Open the default output device and start the stream.
    pub fn open(voice: Box<dyn Voice>) -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| anyhow!("aucune sortie audio : rien pour parler"))?;
        let supported = device
            .default_output_config()
            .map_err(|e| anyhow!("impossible de lire la configuration de sortie : {e}"))?;
        let output_rate = supported.sample_rate();
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        let shared = Arc::new(Mutex::new(Queue::default()));
        let stream = match format {
            cpal::SampleFormat::F32 => stream::<f32>(&device, config, Arc::clone(&shared)),
            cpal::SampleFormat::I16 => stream::<i16>(&device, config, Arc::clone(&shared)),
            cpal::SampleFormat::U16 => stream::<u16>(&device, config, Arc::clone(&shared)),
            other => return Err(anyhow!("format de sortie non géré : {other}")),
        }?;
        stream
            .play()
            .map_err(|e| anyhow!("impossible de démarrer la sortie : {e}"))?;

        tracing::info!("voix {} — sortie à {output_rate} Hz", voice.name());
        Ok(Self {
            shared,
            _stream: stream,
            output_rate,
            voice,
            primed: Mutex::new(HashMap::new()),
        })
    }

    pub fn voice_name(&self) -> String {
        self.voice.name()
    }

    /// Switch to another voice of the loaded model, if the engine can.
    ///
    /// The phrases primed in the old voice are dropped: kept, the waiting
    /// phrases would go on in the voice the user just left. The caller primes
    /// again.
    pub fn revoice(&mut self, voice: &str) -> Result<bool> {
        let done = self.voice.revoice(voice)?;
        if done {
            self.primed.lock().expect("primed phrases poisoned").clear();
        }
        Ok(done)
    }

    /// Put another engine behind the same loudspeaker stream.
    ///
    /// The stream, the queue and every [`Silencer`] handed out stay valid: the
    /// microphone thread keeps its authority to cut without being told that
    /// anything changed. Primed phrases go with the old voice.
    pub fn replace_voice(&mut self, voice: Box<dyn Voice>) {
        tracing::info!("voix remplacée : {} → {}", self.voice.name(), voice.name());
        self.voice = voice;
        self.primed.lock().expect("primed phrases poisoned").clear();
    }

    /// Synthesise a fixed set of phrases now, so that saying them later costs
    /// nothing.
    ///
    /// # The wait this removes, and the one it reveals
    ///
    /// The waiting phrases are the worst possible thing to synthesise on
    /// demand. There are five of them, they never change, and they are said at
    /// the exact moment the turn has nothing else to offer — so the 272 ms this
    /// engine takes to a first sample (decision 63) was being added to the wait
    /// it was there to cover. Five short syntheses at load time delete that.
    ///
    /// It is also **the warm-up the voice never had**. `aria_voice::engine`
    /// warms the ASR and the turn detector for a reason its own comment spells
    /// out: loading builds the graph, it does not run it, and the first `run`
    /// still has an arena to allocate and weights to fault in. The TTS was
    /// loaded the same way and warmed by nobody, so the bill was paid by the
    /// first thing ARIA ever said. Here it is paid behind `Load::Voice`, where
    /// there is already a bar on screen.
    ///
    /// Failures are logged and skipped, never fatal: a phrase that cannot be
    /// primed is a phrase that will be synthesised on demand, which is exactly
    /// what happened before this existed.
    ///
    /// Returns how many are ready, which is what the caller logs.
    pub fn prime(&self, texts: &[&str]) -> usize {
        let started = Instant::now();
        let mut ready = 0usize;
        for text in texts {
            let text = text.trim();
            if text.is_empty() || self.ready(text).is_some() {
                continue;
            }
            // `speak` and not `stream`: there is nobody to play the pieces to
            // yet, so the only thing streaming would buy here is a longer
            // function.
            match self.voice.speak(text) {
                Ok(utterance) => {
                    let samples =
                        resample(&utterance.samples, utterance.sample_rate, self.output_rate);
                    self.primed
                        .lock()
                        .expect("primed phrases poisoned")
                        .insert(text.to_string(), Arc::new(samples));
                    ready += 1;
                }
                Err(e) => tracing::warn!("« {text} » n'a pas pu être préparée : {e:#}"),
            }
        }
        tracing::info!(
            ready,
            asked = texts.len(),
            ms = started.elapsed().as_millis(),
            "phrases d'attente prêtes, et le moteur est préchauffé"
        );
        ready
    }

    /// Samples for a phrase [`Player::prime`] already made, if it made this one.
    fn ready(&self, text: &str) -> Option<Arc<Vec<f32>>> {
        self.primed
            .lock()
            .expect("primed phrases poisoned")
            .get(text)
            .cloned()
    }

    /// Synthesise and queue one stretch of text.
    ///
    /// Returns the delay before the **first sample reached the queue**, which
    /// is the number T0's budget is about. It used to be the whole synthesis,
    /// and the two were the same thing for as long as nothing was queued until
    /// the whole utterance existed — the doc here said so. With a streaming
    /// engine they are not: the first piece of a sentence can be audible while
    /// the last is still being generated, and reporting the total would hide
    /// exactly the improvement that matters.
    ///
    /// The queue needs no changes for this, which is the pleasant part: the
    /// audio callback already reads `pending` as it goes and plays silence past
    /// its end. Extending it from here while playback has started is therefore
    /// seamless — provided nothing cut the queue in between, which is the one
    /// case handled below.
    pub fn say(&self, text: &str) -> Result<Duration> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(Duration::ZERO);
        }
        let started = Instant::now();
        let mut first: Option<Duration> = None;
        // Which queue this sentence belongs to. Decision 17 drops the queue on
        // a barge-in; a later chunk appended after that would be heard as a
        // fragment of a sentence the user already interrupted. So each chunk
        // checks the queue is still the one it started filling, and abandons
        // the rest when it is not.
        //
        // The queue being *empty* is not that: the audio callback empties it
        // every time it catches up, which happens between chunks as soon as
        // generation is slower than playback. See `Queue::generation`.
        let mine = self.shared.lock().expect("queue poisoned").generation;

        // A phrase that was primed never reaches the engine. The queue is the
        // only thing that has to happen, and it happens now rather than in
        // 272 ms — which for the waiting phrases is the whole point, since they
        // are said at the one moment in a turn that has nothing to cover them.
        //
        // The generation is still checked, and for the same reason the
        // streaming path checks it: a barge-in between the lookup and the
        // append would make this a fragment of a sentence the user already
        // interrupted. Cheap here, but "cheap" is not "exempt".
        if let Some(samples) = self.ready(text) {
            let mut queue = self.shared.lock().expect("queue poisoned");
            if queue.generation == mine {
                queue.pending.extend_from_slice(&samples);
            } else {
                tracing::debug!(
                    "phrase prête abandonnée : la file a été coupée avant la mise en file"
                );
            }
            return Ok(started.elapsed());
        }

        let outcome = self.voice.stream(text, &mut |utterance: Utterance| {
            let resampled = resample(&utterance.samples, utterance.sample_rate, self.output_rate);
            let mut queue = self.shared.lock().expect("queue poisoned");
            if queue.generation != mine {
                // Not an error the caller should see as a failure: being
                // interrupted is a normal outcome, and the pilot already knows.
                anyhow::bail!("interrupted");
            }
            queue.pending.extend_from_slice(&resampled);
            drop(queue);
            if first.is_none() {
                first = Some(started.elapsed());
            }
            Ok(())
        });

        match outcome {
            Ok(()) => {}
            Err(e) if e.to_string() == "interrupted" => {
                tracing::debug!("synthèse abandonnée : la file a été coupée en cours de phrase");
            }
            Err(e) => return Err(e),
        }
        Ok(first.unwrap_or_else(|| started.elapsed()))
    }

    /// Stop immediately, dropping whatever was queued.
    ///
    /// Decision 17: the answer to being interrupted is to go quiet. Not to fade,
    /// not to finish the sentence — the queue is dropped and the next callback
    /// reads silence, which is within one buffer. The echo is gone 20 ms later
    /// and the microphone is clean for what follows.
    pub fn silence(&self) {
        let mut queue = self.shared.lock().expect("queue poisoned");
        queue.interrupt();
    }

    /// How many times the queue has been cut.
    ///
    /// For a caller holding text that was meant for the queue **before** a
    /// cut: if the number moved since, that text belongs to an answer the user
    /// spoke over, and saying it now would be ARIA picking up where it was
    /// told to stop.
    pub fn generation(&self) -> u64 {
        self.shared.lock().expect("queue poisoned").generation
    }

    /// Hand the authority to silence to another thread.
    ///
    /// See [`Silencer`] for why that authority has to leave this one.
    pub fn silencer(&self) -> Silencer {
        Silencer {
            shared: Arc::clone(&self.shared),
        }
    }

    pub fn state(&self) -> Playing {
        playing(&self.shared)
    }

    /// What recently went to the speakers, at the output rate.
    ///
    /// The echo gate's reference. Exact rather than reconstructed, which is the
    /// point: a gate fed what we *think* we played would be a guess dressed up
    /// as a measurement.
    pub fn reference(&self) -> Vec<f32> {
        self.shared
            .lock()
            .expect("queue poisoned")
            .reference
            .clone()
    }

    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }
}

/// Permission to stop the voice, and nothing else.
///
/// # Why this is a separate handle and not just the player
///
/// Decision 17 wants playback to stop *inside the frame* when the user speaks
/// over ARIA. The thread that synthesises cannot do it: it is inside
/// [`Voice::stream`] for as long as the sentence takes, which with a local
/// model is seconds, and a barge-in it only reads afterwards is a barge-in
/// that did not happen. So the cut has to come from somewhere else.
///
/// [`Player`] itself cannot go to that somewhere else — it owns a
/// `cpal::Stream`, which is not `Send` — and it should not: nothing but the
/// speech thread has any business *queueing* audio. What crosses is this, the
/// authority to silence: a share of the same queue, and two methods that
/// cannot add to it.
pub struct Silencer {
    shared: Arc<Mutex<Queue>>,
}

impl Silencer {
    /// The same cut as [`Player::silence`], from a thread that is not busy
    /// synthesising. The sentence being generated notices at its next chunk
    /// and abandons the rest — that is what [`Queue::generation`] is for.
    pub fn silence(&self) {
        self.shared.lock().expect("queue poisoned").interrupt();
    }

    /// How many times the queue has been cut. See [`Player::generation`].
    pub fn generation(&self) -> u64 {
        self.shared.lock().expect("queue poisoned").generation
    }

    /// Whether sound is coming out this instant.
    ///
    /// The guard on a barge-in, and it must stay *audible output* rather than
    /// "a sentence is being synthesised". The reply to what the user just said
    /// is generated **after** they stopped speaking, so a `Listening` still in
    /// flight would otherwise cancel the very answer it asked for.
    /// `Event::Spoken` carries no turn to tell those two apart, so the rule is
    /// the one the user can hear: what is playing is what can be spoken over.
    pub fn state(&self) -> Playing {
        playing(&self.shared)
    }

    /// What recently went to the speakers, as a 20 ms RMS envelope.
    ///
    /// The echo gate's input, and the other half of what it takes to decide an
    /// interruption: the gate compares a microphone frame against the echo this
    /// predicts, so without it the only honest verdict is "no idea" and nothing
    /// can be cut on energy at all.
    ///
    /// Exact rather than reconstructed, which is the same reason
    /// [`Player::reference`] exists: a gate fed what we *think* we played would
    /// be a guess dressed up as a measurement.
    ///
    /// **It ends now and begins wherever it begins.** The player keeps a
    /// rolling eight seconds, so index zero slides as the session goes on —
    /// which is why [`crate::echo::Live`] aligns the two envelopes by their
    /// ends and never by their starts.
    pub fn reference_envelope(&self) -> Vec<f32> {
        self.shared.lock().expect("queue poisoned").envelope.clone()
    }
}

fn playing(shared: &Mutex<Queue>) -> Playing {
    if shared.lock().expect("queue poisoned").is_speaking() {
        Playing::Speaking
    } else {
        Playing::Silent
    }
}

fn stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: Arc<Mutex<Queue>>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let keep = REFERENCE_SECONDS * config.sample_rate as usize;
    // 20 ms at the output rate, so the envelope this builds is in the same
    // frames `crate::echo` counts in. One number, one meaning.
    let per_frame = (config.sample_rate as usize / 50).max(1);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                // The lock is held for one buffer and never across anything
                // that can block. A poisoned lock in an audio callback would
                // be a panic on the audio thread, so it reads as silence
                // instead — the one place in this crate where swallowing an
                // error is the right answer.
                let Ok(mut queue) = shared.lock() else {
                    for sample in data.iter_mut() {
                        *sample = T::from_sample(0.0);
                    }
                    return;
                };
                for frame in data.chunks_mut(channels) {
                    let value = queue.pending.get(queue.at).copied().unwrap_or(0.0);
                    if queue.at < queue.pending.len() {
                        queue.at += 1;
                    }
                    queue.reference.push(value);
                    // The envelope, accumulated as the samples go out. A few
                    // flops per sample, against a reduction over eight
                    // seconds on every read from the microphone thread.
                    queue.square_sum += value * value;
                    queue.counted += 1;
                    if queue.counted >= per_frame {
                        let mean = queue.square_sum / queue.counted as f32;
                        queue.envelope.push(mean.sqrt());
                        queue.square_sum = 0.0;
                        queue.counted = 0;
                        if queue.envelope.len() > crate::echo::WINDOW_FRAMES {
                            let excess = queue.envelope.len() - crate::echo::WINDOW_FRAMES;
                            queue.envelope.drain(..excess);
                        }
                    }
                    for sample in frame.iter_mut() {
                        *sample = T::from_sample(value);
                    }
                }
                if queue.reference.len() > keep {
                    let excess = queue.reference.len() - keep;
                    queue.reference.drain(..excess);
                }
                // Reclaim once the queue is spent, so a long session does not
                // carry every sentence it ever said. `reclaim` and not
                // `interrupt`: a sentence still being generated is allowed to
                // keep extending this queue, and an engine slower than real
                // time gets here between two of its own chunks.
                if queue.at >= queue.pending.len() && !queue.pending.is_empty() {
                    queue.reclaim();
                }
            },
            |error| tracing::error!("le flux de sortie a lâché : {error}"),
            None,
        )
        .map_err(|e| anyhow!("impossible d'ouvrir la sortie : {e}"))
}

/// Linear interpolation between two rates.
///
/// Below what a good voice deserves and adequate here for a reason that is not
/// laziness: the sample rates in play are 22 050 or 24 000 against 48 000, so
/// the ratio is near-integral and the artefacts are above the band speech
/// occupies. When a chosen engine deserves better, this is one function.
fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || samples.is_empty() {
        return samples.to_vec();
    }
    let step = from as f32 / to as f32;
    let out_len = (samples.len() as f32 / step) as usize;
    let mut out = Vec::with_capacity(out_len);
    let mut position = 0f32;
    for _ in 0..out_len {
        let index = position as usize;
        let value = match (samples.get(index), samples.get(index + 1)) {
            (Some(&a), Some(&b)) => a + (b - a) * (position - index as f32),
            (Some(&a), None) => a,
            _ => 0.0,
        };
        out.push(value);
        position += step;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_up_keeps_the_duration() {
        let source: Vec<f32> = (0..2400).map(|i| (i as f32 / 100.0).sin()).collect();
        let out = resample(&source, 24_000, 48_000);
        assert_eq!(out.len(), 4800, "0.1 s stays 0.1 s");
        // And the shape survives: the first and last samples bracket the source.
        assert!((out[0] - source[0]).abs() < 1e-6);
    }

    #[test]
    fn resampling_to_the_same_rate_is_a_copy() {
        let source = vec![0.1, 0.2, 0.3];
        assert_eq!(resample(&source, 48_000, 48_000), source);
    }

    #[test]
    fn an_empty_utterance_is_not_a_division_by_zero() {
        assert!(resample(&[], 24_000, 48_000).is_empty());
    }

    /// Decision 17 in the one place it is mechanical: the queue goes, and what
    /// is read next is silence.
    #[test]
    fn silencing_drops_everything_queued() {
        let mut queue = Queue {
            pending: vec![0.5; 1000],
            at: 10,
            reference: vec![0.5; 100],
            generation: 0,
            ..Default::default()
        };
        assert!(queue.is_speaking());
        queue.interrupt();
        assert!(!queue.is_speaking());
        assert!(queue.pending.is_empty());
        assert_eq!(
            queue.reference.len(),
            100,
            "the reference survives the cut — the echo it caused has not"
        );
    }

    /// The regression that cost a sentence: a streaming engine slower than
    /// playback found an empty queue between two of its own chunks, read that
    /// as a barge-in, and dropped the rest of what ARIA was saying.
    ///
    /// Written against the queue rather than the player because [`Player`]
    /// needs an output device, and this is the part that was wrong.
    #[test]
    fn a_spent_queue_is_not_an_interrupted_one() {
        let mut queue = Queue::default();
        // `say` remembers which queue it is filling, then hands over a chunk.
        let mine = queue.generation;
        queue.pending.extend_from_slice(&[0.5; 480]);

        // The audio callback plays it out and gives the memory back.
        queue.at = queue.pending.len();
        queue.reclaim();
        assert!(!queue.is_speaking());
        assert_eq!(
            queue.generation, mine,
            "reclaiming is bookkeeping: the sentence is still the same one"
        );

        // So the next chunk of the same sentence is still welcome, and is
        // played from the start of what is now in the queue.
        queue.pending.extend_from_slice(&[0.25; 480]);
        assert!(queue.is_speaking());
        assert_eq!(queue.at, 0);

        // A real barge-in, on the other hand, is a different queue.
        queue.interrupt();
        assert_ne!(
            queue.generation, mine,
            "decision 17: what comes after this does not belong to that sentence"
        );
    }

    /// What [`Silencer`] exists for: the cut comes from a thread that is *not*
    /// the one inside `Voice::stream`, and the sentence has to find out.
    ///
    /// The shell's own thread is not tested here — a [`Player`] needs an output
    /// device — but the crossing is, which is the part that was missing.
    #[test]
    fn a_barge_in_from_another_thread_abandons_the_sentence() {
        let shared = Arc::new(Mutex::new(Queue::default()));
        let watcher = Silencer {
            shared: Arc::clone(&shared),
        };

        // A sentence already being heard: one chunk queued, and the generation
        // the synthesising thread is holding on to.
        shared
            .lock()
            .expect("lock")
            .pending
            .extend_from_slice(&[0.5; 480]);
        let mine = shared.lock().expect("lock").generation;
        assert_eq!(watcher.state(), Playing::Speaking);

        std::thread::spawn(move || watcher.silence())
            .join()
            .expect("the barge-in thread");

        let queue = shared.lock().expect("lock");
        assert!(queue.pending.is_empty(), "silence is within one buffer");
        assert_ne!(
            queue.generation, mine,
            "the next chunk of that sentence will know it was interrupted"
        );
    }
}
