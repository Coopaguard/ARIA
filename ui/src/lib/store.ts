/**
 * The state the window renders, and the fold that keeps it true.
 *
 * One rule, and everything here follows from it: **state changes only by
 * applying an event or a snapshot.** No component mutates this directly. A
 * command goes to the core, the core publishes, the event comes back, the fold
 * applies it — which is slower to write than an optimistic update and is the
 * reason the screen and the audit journal can never disagree.
 *
 * The one thing that is deliberately *not* here: optimistic UI. Clicking the orb
 * does not flip the orb; it asks the core, and the core answers with
 * `audio.mode` — or with a refusal, which is a sentence worth showing ("no voice
 * command leads to off").
 */

import { reactive } from "vue";

import type {
  ActivityStep,
  AudioMode,
  Download,
  HookNews,
  SecretAsk,
  Envelope,
  Loading,
  ModeTrigger,
  Part,
  PermissionRequest,
  Profile,
  Seq,
  Session,
  SessionState,
  Snapshot,
  TileForm,
  VoiceState,
} from "./contract";

import { NOT_LOADING } from "./contract";

/** A session with the activity the window has seen for it. */
export interface SessionView extends Session {
  steps: ActivityStep[];
  /** The streamed answer, assembled from its fragments. */
  message: string;
  /** Set when the session finished, successfully or not. */
  outcome?: { ok: boolean; text: string };
}

export interface Tile {
  session: string;
  uri: string;
  title: string;
  /** Where the iframe points, on the sandbox origin. Built by the core. */
  source: string;
  /** Fields to ask for. Drawn by the window, never by the tile. */
  form?: TileForm | null;
  /** PDFs framed by the window beside the tile. Built by the core. */
  documents: string[];
}

export interface Store {
  /** Where the applied state is. Also what a reconnect resumes from. */
  seq: Seq;
  connected: boolean;
  mode: AudioMode;
  /** Why the mode last moved — "you asked" and "two minutes went by" are not
   *  the same message to show. */
  trigger: ModeTrigger | null;
  voice: VoiceState;
  /**
   * How far the models are from being in memory.
   *
   * Both of them, which is the whole point: the bar used to end when the
   * first one finished, so the orb lit up while ARIA still had nothing to
   * answer with.
   */
  loading: Loading;
  /** The model file coming down, if any — shown by the first-run screen. */
  download: Download | null;
  profile: Profile;
  /**
   * The thread being written into, when there is one.
   *
   * Held here and not in the menu so the menu follows the core rather than
   * its own clicks: the live thread also moves at startup, and could move
   * from a second window.
   */
  conversation: string | null;
  sessions: SessionView[];
  pending: PermissionRequest[];
  tiles: Tile[];
  /** La dernière demande de rechargement d'une tuile. Voir `tile.updated`. */
  tileRefresh: { uri: string; at: number };
  /** Whether the tiles' sandbox origin is up. Voir `Snapshot.tiles_served`. */
  tilesServed: boolean;
  /** What the user is saying, as the ASR hears it. */
  heard: { text: string; final: boolean } | null;
  /**
   * What ARIA has said, oldest first.
   *
   * Kept as a list rather than a single line because a turn is often two
   * utterances — a waiting phrase, then the answer — and replacing the first
   * with the second would make the cover look like a false start.
   *
   * Until the TTS lands (T4) this is the *only* place ARIA's side of the
   * conversation is visible, so it is what the first real test is read from.
   */
  spoken: string[];
  /** How many lines ARIA has said since the window opened, trimmed or not. */
  spokenTotal: number;
  /**
   * `spokenTotal` when the user last spoke or typed: what ARIA said before
   * that sentence and what it said after are shown on either side of it.
   */
  askedAt: number;
  /** How loud the microphone is, 0 to 1. Not an event: see `stream.ts`. */
  level: number;
  /** Last refusal from the core, in French, for the status line. */
  notice: string | null;
  /**
   * What does not work right now — the ear, the agent — in words for the user.
   * Since 2026-11-27: both used to fail in the log only.
   */
  faults: Partial<Record<Part, string>>;
  /** Bumped by `hooks.changed`: the hooks tab reads its list again. */
  hooksVersion: number;
  /** The hooks' last news, newest last. Shown, said or not. */
  hookNews: HookNews[];
  /** Secrets sessions are waiting for. Drawn by ARIA's own form, never a tile. */
  secretAsks: SecretAsk[];
}

export const store = reactive<Store>({
  seq: 0,
  connected: false,
  mode: "hybrid",
  trigger: null,
  voice: "idle",
  loading: NOT_LOADING,
  download: null,
  profile: "normal",
  conversation: null,
  sessions: [],
  pending: [],
  tiles: [],
  tileRefresh: { uri: "", at: 0 },
  // Optimiste jusqu'à preuve du contraire : le cas normal est qu'il tourne,
  // et un avertissement affiché avant le premier instantané serait faux une
  // fois sur mille et alarmant à chaque démarrage.
  tilesServed: true,
  heard: null,
  spoken: [],
  spokenTotal: 0,
  askedAt: 0,
  level: 0,
  notice: null,
  faults: {},
  hooksVersion: 0,
  hookNews: [],
  secretAsks: [],
});

/** Replace everything with a snapshot. Used at start-up, and after a gap. */
export function applySnapshot(snapshot: Snapshot): void {
  store.seq = snapshot.seq;
  store.mode = snapshot.mode;
  store.voice = snapshot.voice;
  store.loading = snapshot.loading;
  store.profile = snapshot.profile;
  store.tilesServed = snapshot.tiles_served;
  store.faults = { ...snapshot.faults };
  // A snapshot carries sessions and pending requests but not the activity
  // feed: the feed is a stream of events, and events that fell off the ring are
  // gone. Showing an empty feed for a session whose state we do know is the
  // honest rendering of that.
  store.sessions = snapshot.sessions.map((session) => ({
    ...session,
    steps: [],
    message: "",
  }));
  store.pending = [...snapshot.pending];
  store.tiles = [];
  store.tileRefresh = { uri: "", at: 0 };
  // Not in the snapshot: what was being said is not state, it is an
  // utterance in flight. Restoring a stale one would put words back on
  // screen that nobody is saying any more.
  store.heard = null;
}

/** The user starts a sentence, spoken or typed. */
export function applySecretAsks(asks: SecretAsk[]): void {
  store.secretAsks = [...asks];
}

export function markAsked() {
  store.askedAt = store.spokenTotal;
}

/**
 * ARIA's lines around the user's last sentence, in the order they were said.
 *
 * `before` is the line just before it, `after` at most two said since. With no
 * sentence yet, the last two lines are `after`.
 */
export function exchange(): { before: string | null; after: string[] } {
  const first = store.spokenTotal - store.spoken.length;
  const at = (n: number) => (n >= first && n < store.spokenTotal ? store.spoken[n - first] : null);
  if (store.askedAt === 0 && !store.heard) return { before: null, after: store.spoken.slice(-2) };
  const since = store.spoken.slice(Math.max(0, store.askedAt - first));
  return { before: at(store.askedAt - 1), after: since.slice(-2) };
}

function find(id: string): SessionView | undefined {
  return store.sessions.find((session) => session.id === id);
}

/** Fold one event into the state. Called in sequence order. */
export function applyEvent(envelope: Envelope): void {
  store.seq = envelope.seq;

  switch (envelope.type) {
    case "conversation.opened":
      store.conversation = envelope.id;
      break;

    case "session.opened":
      if (!find(envelope.session)) {
        store.sessions.push({
          id: envelope.session,
          title: envelope.title,
          profile: envelope.profile,
          state: "idle",
          opened_at: envelope.at,
          steps: [],
          message: "",
        });
      }
      break;

    case "session.state": {
      const session = find(envelope.session);
      if (session) session.state = envelope.state as SessionState;
      break;
    }

    case "message.delta": {
      const session = find(envelope.session);
      if (session) session.message += envelope.text;
      break;
    }

    case "activity.step": {
      const session = find(envelope.session);
      if (!session) break;
      // A step reported twice is the same step moving on — running, then ok.
      // Matched on label and command, which is what the core sends, rather
      // than on an id it does not.
      const existing = session.steps.find(
        (step) =>
          step.label === envelope.step.label &&
          step.command === envelope.step.command,
      );
      if (existing) existing.status = envelope.step.status;
      else session.steps.push(envelope.step);
      break;
    }

    case "permission.request":
      if (!store.pending.some((p) => p.id === envelope.request.id)) {
        store.pending.push(envelope.request);
      }
      break;

    case "permission.resolved":
      store.pending = store.pending.filter((p) => p.id !== envelope.id);
      // Decision 7: the losing invitation withdraws *saying where the answer
      // came from*. Silence here would look like the request vanished.
      if (envelope.by === "voice") {
        store.notice = "répondu à la voix";
      } else if (envelope.by === "registry") {
        // Not a withdrawal: nobody was ever invited. Said anyway, because an
        // action that ran without being confirmed is exactly the one someone
        // will later ask about.
        store.notice = "autorisé par le registre de motifs";
      }
      break;

    case "tile.ready":
      // Replayed on a reconnect like everything else, so a tile already on
      // screen must not be added twice.
      if (!store.tiles.some((tile) => tile.uri === envelope.uri)) {
        store.tiles.push({
          session: envelope.session,
          uri: envelope.uri,
          title: envelope.title,
          source: envelope.source,
          form: envelope.form ?? null,
          documents: envelope.documents ?? [],
        });
      }
      break;

    case "tile.updated":
      // Le rechargement est demandé à la couche de tuiles, qui détient les
      // cadres : le magasin ne connaît que des données. Un compteur suffit —
      // elle le surveille et relit.
      store.tileRefresh = { uri: envelope.uri, at: store.tileRefresh.at + 1 };
      break;

    case "hooks.changed":
      store.hooksVersion += 1;
      break;

    case "hook.event": {
      const { type: _type, ...news } = envelope;
      store.hookNews.push(news);
      if (store.hookNews.length > 20) store.hookNews.splice(0, store.hookNews.length - 20);
      break;
    }

    case "secret.asked":
      if (!store.secretAsks.some((ask) => ask.id === envelope.ask.id)) {
        store.secretAsks.push(envelope.ask);
      }
      break;

    case "secret.closed":
      store.secretAsks = store.secretAsks.filter((ask) => ask.id !== envelope.id);
      break;

    case "transcript.partial":
      // A final utterance stays until the next one starts; a partial is
      // replaced as it is corrected.
      if (!store.heard || store.heard.final) markAsked();
      store.heard = { text: envelope.text, final: envelope.final };
      break;

    case "spoken":
      // Appended, and trimmed from the front: a window left open all day must
      // not grow a transcript of the whole day in memory.
      store.spoken.push(envelope.text);
      store.spokenTotal += 1;
      if (store.spoken.length > 40) store.spoken.splice(0, store.spoken.length - 40);
      break;

    case "session.engaged": {
      const session = find(envelope.session);
      if (session) session.native_id = envelope.native_id;
      break;
    }

    case "voice.state":
      store.voice = envelope.state;
      break;

    case "voice.loading":
      store.loading = envelope.loading;
      break;

    case "models.download":
      store.download = envelope.download;
      break;

    case "audio.mode":
      store.mode = envelope.mode;
      store.trigger = envelope.trigger;
      break;

    case "task.done": {
      const session = find(envelope.session);
      if (session) session.outcome = { ok: true, text: envelope.summary };
      break;
    }

    case "task.error": {
      const session = find(envelope.session);
      if (session) session.outcome = { ok: false, text: envelope.message };
      break;
    }

    case "fault":
      if (envelope.message) store.faults[envelope.part] = envelope.message;
      else delete store.faults[envelope.part];
      break;
  }
}

/** Sessions still doing something — what the right-hand menu lists. */
export function liveSessions(): SessionView[] {
  return store.sessions.filter((session) =>
    ["idle", "thinking", "executing", "awaiting_permission", "unresponsive"].includes(
      session.state,
    ),
  );
}

/** The requests filed against one session, shown underneath it. */
export function pendingFor(id: string): PermissionRequest[] {
  return store.pending.filter((request) => request.session === id);
}
