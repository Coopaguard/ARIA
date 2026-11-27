/**
 * The commands the window may call, and the stream it listens to.
 *
 * Commands go over Tauri's IPC, events come back over the core's WebSocket.
 * That split is the original specification's — WebSocket *for the events* —
 * and each half gives something the other cannot: an IPC command is scoped by
 * `app/capabilities/default.json`, so what a window may invoke is a file
 * someone can read; the event stream carries a sequence and outlives the
 * window, which is what makes a reconnect replay rather than restart.
 *
 * Everything here is `async` and nothing here mutates the store. The core
 * publishes, the stream delivers, [`applyEvent`](./store) folds. A command that
 * updated the screen itself would be a second source of truth about the most
 * visible question in the product.
 */

import { invoke } from "@tauri-apps/api/core";

import type {
  AudioMode,
  Config,
  GpuCard,
  Connection,
  Conversations,
  Decided,
  Decision,
  Engines,
  KyutaiStatus,
  KyutaiVoice,
  Noise,
  PhononModel,
  PhononVoice,
  ProjectSession,
  ProviderInfo,
  ShellState,
  Snapshot,
  ThreadEntry,
  VoiceEngine,
} from "./contract";
import { applyEvent, applySnapshot, store } from "./store";
import { EventStream } from "./stream";

let stream: EventStream | null = null;

/**
 * Read the snapshot, then open the stream from its sequence.
 *
 * In that order, and it matters: the snapshot says where we are, the stream says
 * what changed after. Opening the stream first would mean folding events into a
 * state the snapshot is about to overwrite.
 */
export async function connect(): Promise<void> {
  const connection = await invoke<Connection>("connection");
  const snapshot = await invoke<Snapshot>("snapshot");
  applySnapshot(snapshot);

  stream = new EventStream(
    (since) =>
      `ws://127.0.0.1:${connection.port}/events` +
      `?token=${encodeURIComponent(connection.token)}&since=${since}`,
    {
      apply: applyEvent,
      meter: (level) => {
        store.level = level;
      },
      connected: (open) => {
        store.connected = open;
        if (open) store.notice = null;
      },
      trouble: (attempts, url) => {
        // Said on screen, not only in a console nobody has open. The count
        // distinguishes a core that is still starting — one or two attempts,
        // then it opens — from one that is refusing, which never stops.
        store.notice = `flux injoignable après ${attempts} essai${
          attempts > 1 ? "s" : ""
        } : ${url.replace(/token=[^&]*/, "token=…")}`;
        console.error("event stream will not open", url);
      },
      resync: async ({ asked, oldest }) => {
        // Away longer than the ring holds. Re-read the state rather than render
        // a history with a hole in it.
        console.warn(`replay gap: asked ${asked}, oldest held ${oldest}`);
        const fresh = await invoke<Snapshot>("snapshot");
        applySnapshot(fresh);
        stream?.open(fresh.seq);
      },
    },
  );
  stream.open(snapshot.seq);
}

/**
 * The orb was clicked: open or close the voice channel, both directions.
 *
 * Epic 04's scope, and the distinction is the point: this is a **microphone
 * switch**, not a shutdown of the audio chain. Claude sessions run to
 * completion — cutting the ear never interrupts a task.
 */
export async function toggleAudio(): Promise<void> {
  try {
    await invoke<AudioMode>("toggle_audio");
  } catch (refusal) {
    // The core refuses some transitions on purpose, and the reason is worth
    // showing: "no voice command leads to off" is a sentence, not a bug.
    store.notice = String(refusal);
  }
}

// The profile is NOT set through a command of its own. It is a stored setting,
// so it goes through `saveConfig` like every other one — otherwise the status
// strip and the settings screen each hold their own idea of it, and a restart
// picks the file's.

export async function answerPermission(
  id: string,
  decision: Decision,
): Promise<void> {
  try {
    await invoke("answer_permission", { id, decision });
  } catch (refusal) {
    // The voice got there first. Decision 7: no overwriting after the fact,
    // because the action may already be under way.
    store.notice = String(refusal);
  }
}

/** STOP: the ear **and** the work. The other half of epic 04's pair. */
export async function stopAll(): Promise<void> {
  await invoke("stop_all");
}

// ----------------------------------------------------------------- the shell

/** The overview's left wedge: open the full window. */
export async function showMain(): Promise<void> {
  await invoke("show_main");
}

/**
 * The overview's right wedge: go to sleep.
 *
 * Hides the windows and, if the setting says so, unloads the audio models.
 * **Sleep is not shutdown** — the core, the sessions and the cadences keep
 * running, so this never interrupts a task.
 */
export async function sleep(): Promise<void> {
  await invoke("sleep");
}

export async function shellState(): Promise<ShellState> {
  return invoke<ShellState>("shell_state");
}

// --------------------------------------------------------------- the settings

export async function getConfig(): Promise<Config> {
  return invoke<Config>("get_config");
}

/** The graphics card found, and which stack runs on it — `aria_voice::gpu::Card`. */
export async function gpuCard(): Promise<GpuCard> {
  return invoke<GpuCard>("gpu_card");
}

/**
 * Apply and persist the settings.
 *
 * Throws when the shortcut cannot be registered — most often because something
 * else holds it. Nothing is applied and nothing is written in that case, which
 * is why the settings screen can show the message against the field instead of
 * having to undo anything.
 */
export async function saveConfig(config: Config): Promise<void> {
  await invoke("set_config", { config });
}

/**
 * Play one scenario against a fake session, by id. Debug builds only.
 *
 * Not a mock of the interface: it drives the real core, so what appears on
 * screen came through the real stream with real sequence numbers. The only
 * fiction is that no subprocess is running — the piece T5 replaces.
 */
export async function playScenario(id = "weather"): Promise<void> {
  try {
    await invoke("play_scenario", { id });
  } catch (e) {
    store.notice = String(e);
  }
}

/** What a scenario is, as the window lists it. Mirrors `ipc::Scenario`. */
export interface Scenario {
  id: string;
  label: string;
  about: string;
}

/**
 * Everything the window may play. Empty outside a debug build, because the
 * command does not exist there — which is the point, and why the failure is
 * swallowed rather than shown.
 */
export async function scenarios(): Promise<Scenario[]> {
  try {
    return await invoke("scenarios");
  } catch {
    return [];
  }
}

// ------------------------------------------------------------------ the engine

/** What the settings screen shows about the active engine. */
export async function providerInfo(): Promise<ProviderInfo | null> {
  return invoke<ProviderInfo | null>("provider_info");
}

/** What each post may run on, and what it is running on. */
export async function engines(): Promise<Engines> {
  return invoke<Engines>("engines");
}

/**
 * Delegate a task. Returns the session the core opened.
 *
 * Nothing is rendered from the return value: the session appears because the
 * core published `session.opened` and the stream delivered it. Drawing from the
 * return value instead would be the second source of truth this design exists
 * to avoid.
 */
export async function launchSession(prompt: string): Promise<string> {
  return invoke<string>("launch_session", { prompt });
}

/**
 * Choose the listening policy: everything, or only when named.
 *
 * A different gesture from {@link toggleAudio}, which opens and closes the
 * ear. Nothing is rendered from the return value — the mode changes because
 * the core published `audio.mode` — but a refusal comes back as a sentence,
 * and that one is worth showing.
 */
export async function setListening(everything: boolean): Promise<void> {
  try {
    await invoke("set_listening", { everything });
    store.notice = null;
  } catch (e) {
    store.notice = String(e);
  }
}

/** Measure the microphone's noise floor, through the chain that holds it. */
export async function measureNoise(seconds: number): Promise<Noise> {
  return invoke<Noise>("measure_noise", { seconds });
}
/**
 * The voices Kyutai can speak with.
 *
 * A catalogue published with the model rather than something read off the
 * machine, so it is the same list everywhere and no server has to be running
 * for the settings screen to open.
 */
export async function kyutaiVoices(): Promise<KyutaiVoice[]> {
  return invoke<KyutaiVoice[]>("kyutai_voices");
}

/**
 * What Kyutai costs on this machine: where its weights go, and whether they
 * are already on disk and in memory.
 *
 * Takes nothing, where it used to take the draft binary path and port — the
 * two settings decision 56 deleted.
 */
export async function kyutaiStatus(): Promise<KyutaiStatus> {
  return invoke<KyutaiStatus>("kyutai_status");
}

/**
 * Put the Kyutai model on the machine now, rather than at the next launch.
 *
 * Takes as long as the download does — minutes on a first run — and shows it
 * through the orb's loading bar, because that is what the bar is for. Returns a
 * sentence to put on screen, since "it worked" and "it was already there" are
 * different things to have been told.
 */
export async function prepareKyutai(
  language: string,
  voice: string,
  quantize: boolean,
): Promise<string> {
  return invoke<string>("prepare_kyutai", { language, voice, quantize });
}


/**
 * The Phonon models, with their licences.
 *
 * A fixed list from the core rather than a Hub query, so the screen opens
 * without the network — and each licence is a checked fact, not something the
 * API reports usefully.
 */
export async function phononModels(): Promise<PhononModel[]> {
  return invoke<PhononModel[]>("phonon_models");
}

/**
 * Every Phonon voice, with a name and a description — unlike Kyutai's, whose
 * identifiers say nothing. Which ones a model offers is `PhononModel.voices`.
 */
export async function phononVoices(): Promise<PhononVoice[]> {
  return invoke<PhononVoice[]>("phonon_voices");
}

/** Whether a Hugging Face token is set. Never the token itself. */
export async function hfToken(): Promise<boolean> {
  return invoke<boolean>("hf_token");
}

/** Store a token where `hf auth login` would. */
export async function saveHfToken(token: string): Promise<void> {
  return invoke<void>("save_hf_token", { token });
}

/** Put the Phonon model on the machine now, rather than at the next launch. */
export async function preparePhonon(
  model: string,
  voice: string,
  language: string,
): Promise<string> {
  return invoke<string>("prepare_phonon", { model, voice, language });
}
/**
 * Hear a voice before choosing it.
 *
 * Plays on its own stream, so it never cuts ARIA off mid-answer. Every engine
 * is simply opened: Kyutai used to need a running server to preview against,
 * and since decision 56 its weights are held in ARIA's own process, so a
 * second voice costs only its prompt file.
 */
export async function tryVoice(
  engine: VoiceEngine,
  id: string,
  language: string,
  model: string,
  quantize: boolean,
): Promise<void> {
  await invoke("try_voice", { engine, id, language, model, quantize });
}

/** What ARIA is told it is, before the user's own lines are added. */
export async function shippedPrompt(): Promise<string> {
  return invoke<string>("shipped_prompt");
}

/**
 * Say something to the main agent, from the keyboard.
 *
 * The same door the microphone comes through, so typed and spoken are one
 * conversation and the agent cannot tell them apart.
 *
 * Returns whether anyone was listening. `false` is not an error — ARIA runs
 * with its agent off when the CLI is missing — but it is the difference
 * between "it said nothing" and "nothing is running", and the user is owed it.
 */
export async function sayToAgent(text: string): Promise<boolean> {
  return invoke<boolean>("say_to_agent", { text });
}

/** The **Arrêter** button on a session. The only way one gets killed. */
export async function stopSession(id: string): Promise<void> {
  await invoke("stop_session", { id });
}

/**
 * A tile asked to run a tool. The host decides, not the window.
 *
 * Relayed straight through: SEP-1865 says the **host** must reject a
 * `tools/call` for a tool without `app` visibility, and the window is the
 * tile's neighbour rather than its judge — a check made here would be a check
 * made by the same process the tile is talking to. So nothing is filtered on
 * this side; the refusal comes back from Rust.
 */
export async function tileCall(
  tool: string,
  args: unknown,
): Promise<unknown> {
  return invoke("tile_call", { tool, arguments: args });
}

/**
 * The user closed a tile.
 *
 * Decision 11: closing has to make it **unreachable**, not merely invisible —
 * the window is not the only thing that can hold the sandbox URL. So the core
 * forgets it and stops serving it; removing the element is the cosmetic half.
 */
export async function closeTile(uri: string): Promise<boolean> {
  return invoke("close_tile", { uri });
}

/**
 * A tile asked to open a link — SEP-1865's `ui/open-link`.
 *
 * It opens in the user's **own browser**, not in ARIA. Measured on
 * 2026-11-01: `netflix.com` answers `X-Frame-Options: DENY`, so a tile cannot
 * frame it whatever our policy says. Leaving the application is not a
 * limitation here, it is the only thing that works — and the user gets their
 * session, their DRM and their full screen.
 *
 * The scheme is checked in Rust, not here: the window is the tile's
 * neighbour, not its judge.
 */
export async function openLink(url: string): Promise<void> {
  return invoke("tile_open_link", { url });
}

/**
 * A tile's form, filled in by the user, handed back to its session.
 *
 * The values travel as data, never as markup: the form was declared by the
 * session and drawn by this window, so what goes back is exactly the set of
 * fields that were asked for. The core drops anything else.
 *
 * It may **relaunch** the session — a tile outlives the work that drew it —
 * and it rejects with a sentence worth showing when it cannot.
 */
export async function submitTileForm(
  uri: string,
  answers: Record<string, string>,
): Promise<void> {
  return invoke("submit_tile_form", { uri, answers });
}

// ----------------------------------------------------------- conversations

/** The left-hand menu: every saved thread, most recently used first. */
export async function conversations(): Promise<Conversations> {
  return await invoke("conversations");
}

/** One thread's turns, for the panel that shows it. */
export async function conversationThread(id: string): Promise<ThreadEntry[]> {
  return await invoke("conversation_thread", { id });
}

/** Its project sessions, expired ones included and marked. */
export async function conversationSessions(
  id: string,
): Promise<ProjectSession[]> {
  return await invoke("conversation_sessions", { id });
}

/**
 * Reopen a thread. **Replaces the main agent's process** — the CLI keeps its
 * own history for the life of its process, so a thread replayed into the
 * session that just held another one would leave both in its context.
 *
 * It relaunches no project session: the agent relaunches what it needs when it
 * needs it (epic 05).
 */
export async function openConversation(id: string): Promise<void> {
  await invoke("open_conversation", { id });
}

/** Start a fresh thread, and a fresh session with it. */
/** Start the main agent again on the open thread — « Réessayer » under a fault. */
export async function restartAgent(): Promise<void> {
  await invoke("restart_agent");
}

export async function newConversation(): Promise<void> {
  await invoke("new_conversation");
}

/** Forget a thread and everything under it. The audit is untouched. */
export async function forgetConversation(id: string): Promise<void> {
  await invoke("forget_conversation", { id });
}

/** The audit, newest first — who allowed what, and through which channel. */
export async function audit(limit?: number): Promise<Decided[]> {
  return await invoke("audit", { limit: limit ?? null });
}
