/**
 * The core's event contract, mirrored.
 *
 * Hand-written on purpose, and kept in one file so the mirror is checkable in
 * one place. The names below are not a translation of the Rust types — they are
 * the **wire names**, which is why they are dotted and snake_cased: `serde`
 * renames the Rust enum variants, so this file has to match the JSON, not the
 * Rust.
 *
 * Source of truth: `crates/core/src/events/mod.rs`. When that file changes,
 * this one changes with it, and `npm run check` is what catches the half that
 * got forgotten.
 */

export type Seq = number;

/** `crates/core/src/permissions/mod.rs` — decision 5. */
export type Profile = "plan" | "strict" | "normal" | "autonomous";

/** Decision 6. `read` never reaches the window: reads do not hit the relay. */
export type Risk = "read" | "low" | "medium" | "high";

/** Decision 7: which way an answer came in. */
/**
 * Which way an answer came in.
 *
 * `registry` is the pattern registry answering without asking anybody.
 * Decision 6 removes the confirmation, not the trace, so an auto-approved
 * action has to read as *auto-approved* in the journal rather than be
 * indistinguishable from a human yes.
 */
export type Channel = "voice" | "window" | "registry";

/** Decision 19 — the listening policy. */
export type AudioMode = "off" | "hybrid" | "active";

/** The audio pipeline right now. A different quantity from `AudioMode`. */
export type VoiceState =
  | "idle"
  /** The audio models are being read off disk — about 3.8 s, measured in T1. */
  | "loading"
  | "listening"
  | "thinking"
  | "speaking"
  | "interrupted";


/**
 * A model being read into memory.
 *
 * There are two, and `voice.state` covers **both** since 2026-10-28. It used
 * to cover only the hearing side, which was true while the only voice was the
 * one Windows already has — that one is ready before it is asked. A chosen
 * engine is not: Kyutai's holds a hundred megabytes, downloaded the first
 * time. Whichever model finished first used to end the wait for both, so the
 * bar completed, the orb lit up, and ARIA was listening with nothing to answer
 * with.
 *
 * A third since epic 10: `agent`, a local main agent — its download if
 * needed, `llama-server` reading the model, and a warm-up request. The bar
 * ends only once that model answers at full speed.
 */
export type Load = "hearing" | "voice" | "agent";

/**
 * How far the models are from being in memory.
 *
 * Counts and not a percentage, because a count is what the core actually
 * knows: neither model reports its own progress. The bar eases from `done` to
 * `done + pending.length` rather than claiming a figure.
 */
export interface Loading {
  done: number;
  total: number;
  pending: Load[];
}

export const NOT_LOADING: Loading = { done: 0, total: 0, pending: [] };

/** One model file coming down from the Hub — the first-run screen shows it
 *  (T12). `size` is 0 until the Hub has answered. */
export interface Download {
  file: string;
  index: number;
  count: number;
  received: number;
  size: number;
}

/** What moved the mode, so the interface can say *why*. */
export type ModeTrigger =
  | "interface"
  | "shortcut"
  | "name_heard"
  | "spoken_sleep"
  | "idle_timeout"
  /** The policy switch, which is a different gesture from the ear switch. */
  | "listen_to_all"
  | "named_only";

export type SessionState =
  | "idle"
  | "thinking"
  | "executing"
  | "awaiting_permission"
  | "unresponsive"
  | "done"
  | "failed"
  | "expired";

export type StepStatus = "running" | "ok" | "failed" | "denied";

export interface ActivityStep {
  label: string;
  /**
   * The command itself. Carried from the V0 because a pattern registry that
   * only ever sees tool names can express no pattern — epic 02.
   */
  command?: string;
  status: StepStatus;
}

export interface Session {
  id: string;
  /** The engine's own id. Needed to resume, and it outlives the process. */
  native_id?: string;
  title: string;
  /** The profile this session runs under, which epic 04 insists is displayed. */
  profile: Profile;
  state: SessionState;
  opened_at: number;
}

export interface PermissionRequest {
  id: string;
  session: string;
  tool: string;
  command?: string;
  detail: string;
  risk: Risk;
  /** When it was asked. There is no field for when it lapses: it does not. */
  asked_at: number;
}

export type Decision =
  | { decision: "allow" }
  | { decision: "allow_always"; scope: string }
  | { decision: "deny" };

/** One event, as the stream carries it. */
export type AriaEvent =
  /** The live thread changed — a fresh one at startup, or one the user
   *  clicked in the left-hand menu. */
  | { type: "conversation.opened"; id: string }
  | { type: "session.opened"; session: string; title: string; profile: Profile }
  | { type: "session.state"; session: string; state: SessionState }
  | { type: "message.delta"; session: string; text: string }
  | { type: "transcript.partial"; text: string; final: boolean }
  // What ARIA says out loud. No session, and that is the point: this is the
  // main agent's own voice, not a task reporting to the screen.
  | { type: "spoken"; text: string }
  | { type: "session.engaged"; session: string; native_id: string }
  | { type: "activity.step"; session: string; step: ActivityStep }
  | { type: "permission.request"; request: PermissionRequest }
  | {
      type: "permission.resolved";
      id: string;
      decision: Decision;
      by: Channel;
    }
  /** A tile already on screen has new contents, or its files changed. */
  | { type: "tile.updated"; uri: string }
  | {
      type: "tile.ready";
      session: string;
      uri: string;
      title: string;
      /** Where to frame it. Built by the core: a `ui://` URI is not a path. */
      source: string;
      /** Fields to ask for, declared by the session and drawn by the window. */
      form?: TileForm | null;
      /**
       * PDFs to frame **beside** the tile, without `sandbox`. Built by the
       * core: Chromium refuses its PDF viewer inside a sandboxed frame.
       */
      documents?: string[];
    }
  | { type: "voice.state"; state: VoiceState }
  | { type: "voice.loading"; loading: Loading }
  | { type: "models.download"; download: Download | null }
  | { type: "audio.mode"; mode: AudioMode; trigger: ModeTrigger }
  | { type: "task.done"; session: string; summary: string }
  | { type: "task.error"; session: string; message: string };

/** An event with its position in the stream. `serde` flattens it, so the event's
 *  own fields sit alongside `seq` and `at`. */
export type Envelope = { seq: Seq; at: number } & AriaEvent;

/**
 * What the stream answers a subscription with.
 *
 * The gap case is not an error to swallow: it means the client was away longer
 * than the replay ring holds, and the honest recovery is to ask for a fresh
 * snapshot rather than render a history with a hole in it.
 */
export type Resume =
  | { type: "resume.replay"; from: Seq; events: Envelope[] }
  | { type: "resume.gap"; asked: Seq; oldest: Seq };

/** Everything needed to draw the interface from nothing. */
export interface Snapshot {
  seq: Seq;
  mode: AudioMode;
  voice: VoiceState;
  /** How far the models are, so a window connecting mid-load shows the bar. */
  loading: Loading;
  profile: Profile;
  sessions: Session[];
  pending: PermissionRequest[];
  /**
   * Whether the tiles' sandbox origin is actually up.
   *
   * The window cannot find out by itself: its CSP lets it reach the event
   * stream and nothing else, so it may not probe `/health`. Without this, a
   * port that was taken shows as the browser's « refused to connect » page
   * inside every tile — which reads as broken tiles.
   */
  tiles_served: boolean;
}

/** How the window opens the stream. Handed over IPC and nowhere else. */
export interface Connection {
  port: number;
  token: string;
}

/**
 * The settings, as `app/src/config.rs` writes them.
 *
 * Keys are snake_case because they are TOML keys, read back through `serde`
 * without renaming — see that file for why the keys are English and the file is
 * TOML rather than the YAML sketched in the original specification.
 */
export interface Config {
  profile: Profile;
  /** Unticked, closing the window **stops ARIA**. */
  stay_in_background: boolean;
  /** Frees the memory, at about 3.8 s to wake. */
  unload_audio_on_sleep: boolean;
  /** The local model and Parakeet on the graphics card; off moves both to the CPU. */
  gpu: boolean;
  /** Never on the Windows key by default: Windows 11 reassigns Win+C. */
  global_shortcut: string;
  /** What ARIA answers to. The aliases are learned, not typed. */
  name: { canonical: string; aliases: string[]; address_within_words: number };
  /** The wake-word detector, `aria_voice::wake::Settings`. Not on screen: carried through. */
  wake: {
    enabled: boolean;
    model: string | null;
    threshold: number;
    consecutive: number;
    refractory: number;
    confirm: boolean;
  };
  agent: Agent;
  delegation: Delegation;
  voice: VoiceSetting;
  /** Per input device, keyed by its identifier. */
  calibration: Record<string, Calibration>;
  /** The first-run screen has been through (T12). False on a fresh install. */
  welcomed: boolean;
}

/** A maker's compute stack, or DirectML which runs on any card. */
export type GpuStack = "cuda" | "rocm" | "oneapi" | "directml";

/** The graphics card found, as `aria_voice::gpu::Card` reports it. */
export interface GpuCard {
  vendor: "nvidia" | "amd" | "intel" | null;
  /** What the driver calls it. Empty when nothing was recognised. */
  name: string;
  /** The maker's own stack, which may not be available yet. */
  native: GpuStack | null;
  /** What runs once the maker's stack is there: that stack, or DirectML. */
  runs: GpuStack;
  /** The runtime this process loaded — DirectML until the next start after a download. */
  loaded: GpuStack;
  /** The maker's own runtime is downloaded. */
  installed: boolean;
}

export const GPU_STACK_LABEL: Record<GpuStack, string> = {
  cuda: "CUDA",
  rocm: "ROCm",
  oneapi: "oneAPI",
  directml: "DirectML",
};

/** The post that holds the conversation. */
export interface Agent {
  /** Which engine holds the conversation. An id from the voice list. */
  engine: string;
  /** The binary, or a path to it. */
  command: string;
  /** In full, never an alias: an alias resolves a generation behind, silently. */
  model: string;
  /** Added to the shipped prompt, never in its place. */
  persona: string;
  /** The `llama-server` behind a local engine (epic 10). Edited in `aria.toml`. */
  local: {
    server: string;
    args: string[];
    no_transcript: boolean;
  };
}

/**
 * The engine delegated sessions run on.
 *
 * A section of its own, and that is the point: the two posts are not the same
 * job. The voice post is a dispatcher whose every word is spoken, under a byte
 * budget; a delegated session is the one doing the work, and may be given reach
 * the voice post never has. So each keeps its own engine, its own binary and
 * no version check: the CLI is called, and a failure says why.
 */
export interface Delegation {
  engine: string;
  command: string;
}

/**
 * Which synthesis engine speaks.
 *
 * The original specification asked for the engine to be a setting before either
 * one existed — *les identifiants de modèles vivent en configuration, jamais
 * en dur*.
 *
 * Phonon only since 2026-11-19: the Windows voice is removed, Kyutai is
 * suspended — still a valid value, never offered (`ENGINES` in Settings.vue),
 * and resolved to Phonon by the core.
 */
export type VoiceEngine = "kyutai" | "phonon";

/** What ARIA sounds like. */
export interface VoiceSetting {
  /** Unticked, ARIA answers in writing. */
  enabled: boolean;
  /** Which engine does the speaking. */
  engine: VoiceEngine;
  language: string;
  /** Each engine keeps its own voice, so switching never mixes name spaces. */
  kyutai: KyutaiSetting;
  phonon: PhononSetting;
}

/** Where Phonon's model and voice are chosen. */
export interface PhononSetting {
  /** A repository from `phononModels()`, or empty for the language's default:
   *  the French model in French (decision 121), the open English one otherwise. */
  model: string;
  /** A `voice_id` from the catalogue, or empty for the model's own voice. */
  voice: string;
}

/** One Phonon model, with what choosing it costs. */
export interface PhononModel {
  repo: string;
  language: string;
  /** Whether its licence allows commercial use. Every French one is false. */
  commercial: boolean;
  /** Whether it needs an accepted licence and a Hugging Face token. */
  gated: boolean;
  /** Whether its files are already on this machine. */
  cached: boolean;
  /** The languages of the voices offered with it. */
  voices: string[];
}

/** One Phonon voice. All `apache-2.0`, so none has to be flagged. */
export interface PhononVoice {
  id: string;
  /** What a person reads — `Noémie`, `Augustin`. */
  name: string;
  /** `fr`, `en`, `es`… */
  language: string;
  gender: string;
  description: string;
}

/**
 * How Pocket TTS should speak.
 *
 * It carried a `binary` path and a `port` while the engine was a subprocess.
 * Decision 56 linked the model instead, which deleted both questions rather
 * than answering them.
 */
export interface KyutaiSetting {
  /** A catalogued voice, or empty for the language's default — `estelle` in French. */
  voice: string;
  /** int8 weights: less memory, slightly different sound. */
  quantize: boolean;
}

/**
 * One voice Kyutai can speak with.
 *
 * Carries its licence: these come from several corpora under different
 * licences and nothing in the name says which.
 */
export interface KyutaiVoice {
  id: string;
  commercial: boolean;
}

/**
 * What Kyutai costs on this machine right now.
 *
 * Four fields fewer than it had: where the binary was, whether it was there,
 * where to download it, and whether a server answered. Decision 56 linked the
 * model, so none of the four is a question any more.
 */
export interface KyutaiStatus {
  /** Where the weights live — ARIA decides it rather than inheriting it. */
  cache: string;
  /** Whether that cache already holds a model. */
  weights: boolean;
  /**
   * Whether the weights are in memory, this instant — so whether « écouter »
   * answers in a moment or takes a few seconds. Not a gate: a preview loads
   * what it needs.
   */
  loaded: boolean;
}

/** Which of the three states the shell is in. */
export type ShellState = "sleeping" | "overview" | "main";

// ---------------------------------------------------------------- French labels
//
// Decision 12: the code is English, what the user reads is French. These are the
// only place the two meet, and they are tables rather than scattered ternaries
// so that a missing label is a type error.

export const PROFILE_LABEL: Record<Profile, string> = {
  plan: "plan",
  strict: "strict",
  normal: "normal",
  autonomous: "autonome",
};

export const SESSION_LABEL: Record<SessionState, string> = {
  idle: "en attente",
  thinking: "réflexion",
  executing: "exécution",
  awaiting_permission: "autorisation requise",
  unresponsive: "sans réponse",
  done: "terminée",
  failed: "échec",
  expired: "plus reprenable",
};

export const RISK_LABEL: Record<Risk, string> = {
  read: "lecture",
  low: "faible",
  medium: "moyen",
  high: "élevé",
};

export const MODE_LABEL: Record<AudioMode, string> = {
  off: "coupé",
  hybrid: "nommé seulement",
  active: "à l'écoute",
};

/**
 * Why the mode moved, as a sentence rather than a word.
 *
 * `spoken_sleep` and `idle_timeout` both end up in hybrid, and showing the same
 * text for both would lose the one thing the user wants to know: whether ARIA
 * stopped because they asked, or because it gave up waiting.
 */
export const TRIGGER_LABEL: Record<ModeTrigger, string> = {
  interface: "à la demande",
  shortcut: "au raccourci",
  name_heard: "nom reconnu",
  spoken_sleep: "mise en veille demandée",
  idle_timeout: "deux minutes sans échange",
  listen_to_all: "tu as demandé l'écoute complète",
  named_only: "tu es repassé en écoute nommée",
};

/**
 * What each listening policy does, for the label that appears on hover.
 *
 * Long on purpose. The difference between the two is a **privacy guarantee**
 * (decision 19), not a convenience, and a switch whose two positions are two
 * words is a switch people flip without knowing what they gave up.
 */
export const MODE_EXPLAINED: Record<AudioMode, string> = {
  off: "Micro coupé, modèles déchargés. Rien n'est écouté. Le réveil coûte le rechargement des modèles — environ 3,8 secondes.",
  hybrid:
    "ARIA écoute, mais rien ne sort de la machine tant que son nom n'a pas été reconnu. C'est le mode de repos, et c'est ce qui fait du « nommé seulement » une garantie et pas un simple silence.",
  active:
    "ARIA répond à tout ce qu'elle entend, sans avoir à être nommée. Tout ce qui est dit dans la pièce lui parvient. Elle revient d'elle-même en mode nommé après deux minutes sans échange.",
};

/** What each engine is called on screen. */
export const ENGINE_LABEL: Record<VoiceEngine, string> = {
  kyutai: "Kyutai Pocket TTS",
  phonon: "Phonon (dans le processus)",
};

/** What choosing one actually means. */
export const ENGINE_EXPLAINED: Record<VoiceEngine, string> = {
  phonon:
    "Le moteur léger : le modèle monte en un quart de seconde et la synthèse arrive en flux, premier morceau en 70 à 140 ms. Chargé dans le processus d'ARIA — ni port, ni sous-processus. En échange : seul son modèle anglais est sous licence ouverte, tous ses modèles français sont en CC-BY-NC et demandent un jeton Hugging Face.",
  kyutai:
    "Cent millions de paramètres sur le processeur, environ 200 ms avant le premier son, poids en CC-BY-4.0, et une voix française livrable — « estelle », en CC0. Elle ne prend aucune VRAM : la voix ne dispute plus la carte graphique au cerveau. Chargée dans le processus d'ARIA, en flux elle aussi, donc rien à installer à la main et aucun port. En échange, 400 Mo à télécharger une fois et un modèle qui reste en mémoire.",
};

/** What each model is called while it is being read. */
export const LOAD_LABEL: Record<Load, string> = {
  hearing: "l'oreille",
  voice: "la voix",
  agent: "le modèle local",
};

// ------------------------------------------------------------------ the engine
//
// `crates/core/src/providers/mod.rs`. The shape that matters here is
// `ProfileSupport.native` being nullable: a provider that cannot honour a
// profile has to be *sayable*, or "the interface says which are missing" is a
// sentence with no mechanism behind it.

export interface NativeMode {
  /** The provider's own name for it — `acceptEdits`, and so on. */
  name: string;
  /** What it actually does, in French. Shown: a profile whose translation you
   *  cannot see is a trap. */
  explanation: string;
}

export interface ProfileSupport {
  profile: Profile;
  /** `null` when the provider has no equivalent for this profile. */
  native: NativeMode | null;
}

export type Availability =
  | { state: "ready"; version: string }
  | { state: "missing"; reason: string };

/** A CLI, a model on this machine, or a remote API. What the dropdown groups by. */
export type EngineKind = "cli" | "local" | "api";

export const ENGINE_KIND: Record<EngineKind, string> = {
  cli: "CLI",
  local: "moteur local",
  api: "API",
};

/** One entry in an engine dropdown. */
export interface EngineChoice {
  id: string;
  display_name: string;
  kind: EngineKind;
}

/**
 * What a model is **for**, which is the only vocabulary a dispatcher can choose
 * by.
 *
 * A tier and never a model id: `claude-opus-5-5` means nothing to Gemini or
 * Codex, it costs more bytes in a schema sent on every turn of speech, and
 * models are renamed constantly — a dispatcher that named one would eventually
 * name something that no longer exists.
 */
export type ModelTier = "fast" | "standard" | "expert";

export const TIER_LABEL: Record<ModelTier, string> = {
  fast: "rapide",
  standard: "standard",
  expert: "expert",
};

/** One model an engine offers. Asked of the provider, never listed here. */
export interface ModelChoice {
  /** The engine\x27s own identifier, in full. Never an alias: an alias resolves a
   *  generation behind, silently. */
  id: string;
  display_name: string;
  tier: ModelTier;
  /** One line for this screen. */
  note: string;
}

/** One tier, and the model it lands on. */
export interface ResolvedTier {
  tier: ModelTier;
  /** The word the voice agent writes. */
  word: string;
  /** `null` on an engine that declares no models: the session then runs on that
   *  engine\x27s own default. */
  model: ModelChoice | null;
}

/** What one post may run on, and what it is running on. */
export interface PostEngines {
  /** The **resolved** id: a typo in the file shows what is running, not what is not. */
  selected: string;
  choices: EngineChoice[];
  /** Cheapest first. */
  models: ModelChoice[];
  /** What each tier resolves to — shown, because the voice agent picks one on
   *  its own and the user is entitled to see what those words will run. */
  tiers: ResolvedTier[];
}

/**
 * Both dropdowns.
 *
 * Two, because the two posts are two jobs. The voice list has one entry today
 * and is shown as one entry rather than implying a choice that does not exist —
 * its second engine is tier B, a local model, not another CLI.
 */
export interface Engines {
  voice: PostEngines;
  delegation: PostEngines;
}

export interface ProviderInfo {
  id: string;
  display_name: string;
  kind: EngineKind;
  availability: Availability;
  mapping: ProfileSupport[];
  /** What this engine cannot do, in French. Empty when nothing is missing.
   *
   *  Said and not merely logged: an engine that cannot be asked for permission
   *  stops showing cards, and a capability you cannot see missing is as much a
   *  trap as a profile silently absent. */
  shortcomings: string[];
}

/** What the settings screen says about the engine, in French. */
export function availabilityLabel(a: Availability): string {
  switch (a.state) {
    case "ready":
      return `CLI ${a.version} — prête`;
    case "missing":
      return `CLI introuvable (${a.reason})`;
  }
}

/**
 * What a microphone measurement found.
 *
 * `app/src/voice/calibration.rs`. The verdict is a sentence and not a code:
 * the numbers alone leave the reader to work out that a threshold below the
 * noise means a gate that never closes, which is a deduction nobody should
 * have to make from percentiles.
 */
export interface Noise {
  device: string;
  frames: number;
  median: number;
  p90: number;
  /** The worst of the background. What a threshold has to clear. */
  p99: number;
  default_threshold: number;
  suggested: number;
  verdict: string;
}

/**
 * The voice chain's thresholds, stored **per input device**.
 *
 * `crates/voice/src/calibration.rs`. Per device and never global (decision
 * 38): a threshold measured on a headset is wrong on laptop speakers, and a
 * chain that silently degrades when a headset is plugged in is a fault nobody
 * ever diagnoses.
 */
export interface Calibration {
  speech_rms: number;
  silence_ms: number;
  min_voiced_ms: number;
  turn_threshold: number;
  echo_margin_db: number;
}

/** What ships, and what a device with nothing stored falls back to. */
export const DEFAULT_CALIBRATION: Calibration = {
  speech_rms: 0.01,
  silence_ms: 300,
  min_voiced_ms: 250,
  turn_threshold: 0.85,
  echo_margin_db: 12.0,
};

/**
 * A form a tile asked for — **declared**, never drawn by the tile.
 *
 * The window renders these fields outside the sandboxed frame, and that is the
 * whole point: the tile's script cannot read what the user types, because it
 * is not in its document, and cannot submit it, because it has no hand on the
 * IPC call. Anything a tile drew itself would only ever be "the tile says X".
 *
 * Mirrors `aria_core::tiles::Form`.
 */
export interface TileForm {
  /** The button's label. "Envoyer" when empty. */
  submit: string;
  fields: TileField[];
}

export interface TileField {
  /** The key this answer is reported under. */
  name: string;
  label: string;
  kind: "text" | "number" | "choice" | "toggle";
  /** The choices, for `choice`. Empty otherwise. */
  options: string[];
  /** What it starts with. */
  value: string;
}

// ------------------------------------------------------------ conversations

/**
 * A saved thread, as the left-hand menu lists it.
 *
 * Mirrors `aria_core::store::history::Conversation`. The turns are **not**
 * here: the menu shows every conversation and reads none of them, and loading
 * thirty threads to draw thirty lines is the mistake that split exists to
 * prevent.
 */
export interface Conversation {
  id: string;
  /** RFC 3339, with its offset. */
  started_at: string;
  last_used_at: string;
  /** The first thing said, trimmed. Empty until somebody speaks. */
  title: string;
  turns: number;
}

/** The menu's whole answer. Mirrors `ipc::Conversations`. */
export interface Conversations {
  /** The thread being written into, when there is one. */
  current: string | null;
  threads: Conversation[];
  /**
   * Whether anything is being saved at all — `false` when `aria.db` would not
   * open. An empty menu meaning "nothing yet" and one meaning "nothing is
   * being saved" look identical otherwise, and the user finds out tomorrow.
   */
  saving: boolean;
}

/**
 * One turn of a thread. Mirrors `aria_core::agent::journal::Entry`, which is
 * an externally-tagged enum: exactly one key, named after the variant.
 */
export type ThreadEntry =
  | { Said: { at: string; text: string } }
  | { Answered: { at: string; text: string } }
  | { Delegated: { at: string; id: string; task: string } }
  | { Returned: { at: string; id: string; spoken: string } };

/**
 * A session kept because it belongs to a project.
 *
 * Mirrors `aria_core::store::history::ProjectSession`. `expired` is set after
 * a resume **failed**, never deduced from a date: `cleanupPeriodDays` lives in
 * the user's own settings and can be one day. The entry stays either way —
 * what is lost is the ability to continue, not the memory of what was done.
 */
export interface ProjectSession {
  id: string;
  native: string | null;
  project: string;
  title: string;
  last_used_at: string;
  expired: boolean;
}

/** One line of the audit. Mirrors `aria_core::store::history::audit::Decided`. */
export interface Decided {
  at: string;
  session: string | null;
  tool: string;
  command: string | null;
  risk: Risk;
  decision: Decision;
  /** `registry` is an automatic approval — decision 6 keeps the trace. */
  channel: Channel;
}
