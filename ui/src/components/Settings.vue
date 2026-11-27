<script setup lang="ts">
/**
 * The settings screen — a **tab of the left-hand menu** since 2026-11-16.
 *
 * It was a dialog over a scrim, opened by a button at the foot of the left
 * column. The visual rework folded it into the menu with the sessions: one
 * panel, one way in. So there is no « fermer » any more — leaving the tab is
 * leaving the screen, and saving already happens on leaving each field.
 *
 * Epic 04 asks for four things here, and three of them carry a warning the user
 * has to see rather than discover:
 *
 *  - **the default profile** — no longer here since 2026-11-19: the main view
 *    already chooses it, and two places to set one thing is one too many.
 *    What the delegation engine cannot honour is still said, on its row.
 *  - **stay in background.** Unticked, closing the window *stops ARIA*. Epic 04
 *    asks for that to be said explicitly, and it is the one setting that turns
 *    a background service into an ordinary application.
 *  - **unload the audio models on sleep.** A memory-against-latency trade with
 *    a measured number on each side: about 3.8 s to reload, against the 300 MB
 *    resting target of the original specification.
 *  - **the global shortcut.** Captured by pressing it, not typed.
 *
 * **Everything saves on leaving the field.** There is no button, and the one
 * argument there was for one has an answer: the global shortcut is the single
 * setting where saving *acts* — registering a new one releases the current —
 * so a live-apply text field would leave someone who changed their mind
 * holding a shortcut they never confirmed. That field is not typed into. It is
 * **captured**: you press the combination, and pressing it is the
 * confirmation. Nobody presses Ctrl+Alt+K by accident.
 *
 * The write is in flight at most once. A toggle clicked twice quickly would
 * otherwise send two writes that can land in either order, and the file would
 * keep whichever the disk finished last rather than the last thing clicked.
 *
 * What a save does **not** do is restart anything. The engine, the model, the
 * persona and the voice are read once at start-up; the panel says so rather
 * than letting someone change the model and wonder why ARIA still sounds the
 * same.
 */
import { computed, ref, watch } from "vue";

import {
  getConfig,
  gpuCard,
  kyutaiStatus,
  kyutaiVoices,
  engines,
  measureNoise,
  providerInfo,
  saveConfig,
  shippedPrompt,
  phononModels,
  phononVoices,
  prepareKyutai,
  preparePhonon,
  tryVoice,
} from "../lib/bridge";
import {
  availabilityLabel,
  GPU_STACK_LABEL,
  type GpuCard,
  DEFAULT_CALIBRATION,
  ENGINE_EXPLAINED,
  ENGINE_KIND,
  ENGINE_LABEL,
  PROFILE_LABEL,
  TIER_LABEL,
  type Config,
  type EngineChoice,
  type EngineKind,
  type Engines,
  type ModelChoice,
  type KyutaiStatus,
  type KyutaiVoice,
  type Noise,
  type PhononModel,
  type PhononVoice,
  type ProviderInfo,
  type VoiceEngine,
} from "../lib/contract";
import { store } from "../lib/store";
import { checkUpdate, update } from "../lib/update";
import { getVersion } from "@tauri-apps/api/app";

const version = ref("");
getVersion().then((v) => (version.value = v)).catch(() => {});
const updateNote = ref("");
async function recheck() {
  const found = await checkUpdate();
  // Asked for by hand, so a « plus tard » given earlier no longer holds.
  update.declined = null;
  updateNote.value = found ? `${found.version} est disponible.` : "Aucune version plus récente.";
}

/**
 * One group only, for the first-run screen (T12), which walks through them a
 * step at a time. The same component rather than a copy: AGENTS.md's first
 * invariant, one implementation per component. `only` also drops the rows a
 * newcomer has no use for — the command, the model, the persona.
 */
const props = defineProps<{ only?: "agent" | "voice" | "listen" }>();
const full = computed(() => !props.only);
const show = (group: "agent" | "voice" | "listen") => !props.only || props.only === group;

const draft = ref<Config | null>(null);
const error = ref<string | null>(null);
const saved = ref(false);
/** True while the next key press is being read as the new shortcut. */
const capturing = ref(false);

// A failed read leaves `draft` null and says so. The alternative — falling back
// to the defaults — would show someone their settings reset to factory values
// and let them save that over the file they could not read.
getConfig()
  .then((config) => {
    draft.value = config;
    void checkKyutai();
  })
  .catch((e) => (error.value = `Réglages illisibles : ${e}`));

/**
 * The card found, so the switch can say what it switches on (2026-11-22).
 * With no card from a known maker, DirectML runs anyway and the switch is
 * disabled: there is nothing to choose.
 */
const card = ref<GpuCard | null>(null);
gpuCard()
  .then((found) => (card.value = found))
  .catch(() => (card.value = null));
const VENDOR_LABEL = { nvidia: "NVIDIA", amd: "AMD", intel: "Intel" } as const;
const cardLine = computed(() => {
  const c = card.value;
  if (!c) return "";
  if (!c.vendor) return "Aucune carte reconnue : DirectML est utilisé.";
  const found = c.name || VENDOR_LABEL[c.vendor];
  // AMD: ROCm has no Windows build (decision 131). Intel: OpenVINO refuses
  // Parakeet (2026-11-27, `aria_voice::gpu::Stack::available`).
  if (c.native && c.native !== c.runs) {
    const why = c.native === "oneapi" ? "ne lit pas encore le modèle d'écoute" : "n'existe pas sous Windows";
    return `${found} — ${GPU_STACK_LABEL[c.native]} ${why} : DirectML.`;
  }
  if (c.loaded === c.runs) return `${found} — ${GPU_STACK_LABEL[c.runs]}.`;
  // The maker's runtime is downloaded on demand and loaded at the next start.
  if (c.installed) {
    return `${found} — ${GPU_STACK_LABEL[c.runs]} installé : actif au prochain démarrage d'ARIA.`;
  }
  return `${found} — ${GPU_STACK_LABEL[c.runs]} n'est pas encore installé : DirectML en attendant.`;
});
/** What the maker's runtime weighs, to say before it is fetched. */
const DOWNLOAD_SIZE: Partial<Record<string, string>> = { cuda: "1,4 Go", oneapi: "43 Mo" };
const gpuSwitchable = computed(() => !!card.value?.vendor);

/**
 * What each profile means on the active provider.
 *
 * **Asked of the provider, never written here.** Decision 5, and it is the whole
 * reason the abstraction is worth having with one implementation: wiring a
 * second engine is a mapping it declares, not a table in this file.
 */
const provider = ref<ProviderInfo | null>(null);
providerInfo().then((info) => (provider.value = info));

/**
 * What each post may run on.
 *
 * **Two lists, because the two posts are two jobs.** The agent that speaks is a
 * dispatcher whose every word is spoken, under a byte budget, never billed per
 * token; a delegated session is the one doing the work, and may be given reach
 * the voice post never has. An engine good at one can be wrong for the other,
 * so this screen asks twice.
 *
 * Asked of the core, never written here — the same rule as the profile table
 * above, one level up. A release build offers one engine per post and that is
 * shown as one entry rather than as a choice that does not exist.
 */
const engineList = ref<Engines | null>(null);
engines().then((list) => (engineList.value = list));

/**
 * The models offered for the conversation, plus whatever is configured.
 *
 * **Plus whatever is configured**, and that is the half worth the code: the
 * model identifier lives in `aria.toml`, a file a person edits, and
 * the original specification is explicit that identifiers belong in configuration. A dropdown
 * that silently replaced a hand-set value with one of three would be this
 * screen overruling the file.
 */
const voiceModels = computed<ModelChoice[]>(() => {
  const offered = engineList.value?.voice.models ?? [];
  const set = draft.value?.agent.model?.trim();
  if (!set || offered.some((m) => m.id === set)) return offered;
  return [
    ...offered,
    {
      id: set,
      display_name: set,
      tier: "standard",
      note: "Réglé à la main dans aria.toml.",
    },
  ];
});


/**
 * Point a post at another engine.
 *
 * Saved like every other setting — on leaving the field — and read at start-up
 * like the model and the voice, which the footer already says. Switching the
 * delegation engine also re-asks the core what it can do: the profile table and
 * the shortcomings below belong to whichever engine is attached, and showing
 * the previous one's would be worse than showing none.
 */
function pickEngine(post: "voice" | "delegation", event: Event | string) {
  if (!draft.value) return;
  const id =
    typeof event === "string" ? event : (event.target as HTMLSelectElement).value;
  if (post === "voice") draft.value.agent.engine = id;
  else draft.value.delegation.engine = id;
  void save().then(() => {
    void providerInfo().then((info) => (provider.value = info));
    void engines().then((list) => (engineList.value = list));
  });
}

/**
 * The main agent's two modes: a local model that hears the audio, or Parakeet
 * transcribing for a CLI. An API engine, the day one exists, counts with the
 * CLI — both are fed a transcript.
 */
type AgentKind = "local" | "cli";
const AGENT_KINDS: { kind: AgentKind; label: string }[] = [
  { kind: "cli", label: "Parakeet + CLI" },
  { kind: "local", label: "modèle vocal local" },
];
const kindOf = (k: EngineKind): AgentKind => (k === "local" ? "local" : "cli");
function agentChoices(kind: AgentKind): EngineChoice[] {
  return (engineList.value?.voice.choices ?? []).filter((c) => kindOf(c.kind) === kind);
}
const agentKind = computed<AgentKind>(() => {
  const id = draft.value?.agent.engine;
  const c = engineList.value?.voice.choices.find((c) => c.id === id);
  if (c) return kindOf(c.kind);
  return id?.startsWith("local-") ? "local" : "cli";
});
/** Switching mode picks that mode's first engine; staying put does nothing. */
function pickAgentKind(kind: AgentKind) {
  if (kind === agentKind.value) return;
  const first = agentChoices(kind)[0];
  if (first) pickEngine("voice", first.id);
}

/**
 * The engines offered. Phonon only since 2026-11-19: the Windows voice is
 * removed, and Kyutai is **suspended** — add `"kyutai"` back here, and drop
 * `Engine::in_force` in `app/src/config.rs`, to bring it back. Its rows below
 * are kept for that day.
 */
const ENGINES: VoiceEngine[] = ["phonon"];

/**
 * The Phonon models, with their licences.
 *
 * Fetched once: the list is fixed in the core, only `cached` changes — and
 * that is refreshed by `checkPhonon` after a download.
 */
const phononCatalogue = ref<PhononModel[]>([]);
async function checkPhonon() {
  try {
    phononCatalogue.value = await phononModels();
  } catch {
    phononCatalogue.value = [];
  }
}
void checkPhonon();

/**
 * The model in force, defaults resolved — the same rule as
 * `phonon::default_for` in the core: empty means the French model in French
 * (decision 121), the open English one otherwise. `MODELS` lists the French
 * model before the multilingual one, so the first `fr` entry is it.
 */
const phononModel = computed(() => {
  const chosen = draft.value?.voice.phonon.model;
  const list = phononCatalogue.value;
  const french = draft.value?.voice.language.toLowerCase().startsWith("fr");
  return (
    list.find((m) => m.repo === chosen) ??
    (french ? list.find((m) => m.language === "fr") : undefined) ??
    list[0]
  );
});

/**
 * The models offered: those of the voice's language only (asked for on
 * 2026-11-22 — an English model speaking French is an accent, not a choice).
 * The one in force stays listed even when it does not match, so a hand-edited
 * `aria.toml` is shown rather than silently overruled.
 */
const phononOffered = computed(() => {
  const language = draft.value?.voice.language.toLowerCase().split(/[-_]/)[0] ?? "";
  return phononCatalogue.value.filter(
    (m) => m.language === language || m.repo === phononModel.value?.repo,
  );
});

/**
 * Every Phonon voice, all languages.
 *
 * It can fail — without the shipped set and without a network there is nothing
 * to read — and then a row says so, rather than a spinner that never ends.
 */
const phononVoiceList = ref<PhononVoice[]>([]);
const phononVoicesFailed = ref(false);
async function loadPhononVoices() {
  try {
    phononVoiceList.value = await phononVoices();
    phononVoicesFailed.value = false;
  } catch {
    phononVoiceList.value = [];
    phononVoicesFailed.value = true;
  }
}

/** The catalogue's language tags, as the screen says them. */
const LANGUAGE: Record<string, string> = {
  de: "allemand",
  en: "anglais",
  es: "espagnol",
  fr: "français",
  pt: "portugais",
};

/**
 * The voices the model in force offers: the fifteen French ones with the
 * French model, all the others with the multilingual one (asked for on
 * 2026-11-24). The voice in force stays listed even when it is not one of
 * them, for the same reason as the models above. Grouped by language; the
 * catalogue is already in alphabetical order.
 */
const phononVoicesOffered = computed(() => {
  const languages = phononModel.value?.voices ?? [];
  const current = draft.value?.voice.phonon.voice;
  return phononVoiceList.value
    .filter((v) => languages.includes(v.language) || v.id === current)
    .sort((a, b) => a.language.localeCompare(b.language));
});

/**
 * Change model. A voice the new model does not offer is let go: the list
 * would otherwise keep one that belongs to the other model.
 */
function chooseModel(repo: string) {
  if (!draft.value) return;
  const phonon = draft.value.voice.phonon;
  phonon.model = repo;
  const offered = phononCatalogue.value.find((m) => m.repo === repo)?.voices;
  const voice = phononVoiceList.value.find((v) => v.id === phonon.voice);
  if (offered && voice && !offered.includes(voice.language)) phonon.voice = "";
  void save();
  void checkPhonon();
}

/**
 * The voices Kyutai can speak with.
 *
 * A catalogue published with the model, so it is the same list everywhere.
 * Not filtered by
 * language either: the embeddings exist for every released language, and a
 * French model speaking with `giovanni` is a legitimate thing to want to hear.
 */
const catalogue = ref<KyutaiVoice[]>([]);
kyutaiVoices().then((list) => (catalogue.value = list));

/**
 * Where the weights go, whether they are on disk, and whether they are in
 * memory.
 *
 * Takes nothing: the two draft fields it used to be asked about — the binary
 * path and the port — stopped existing with decision 56. Re-asked after a
 * download, which is the only thing that changes the answer.
 */
const kyutai = ref<KyutaiStatus | null>(null);
async function checkKyutai() {
  if (!draft.value) return;
  try {
    kyutai.value = await kyutaiStatus();
  } catch {
    // Not surfaced: the row below simply says nothing is known, which is
    // honest, where an error banner over a status probe would be noise.
    kyutai.value = null;
  }
}

/**
 * Whether the chosen Kyutai voice allows commercial use.
 *
 * Two of the twenty-seven voices come from Expresso and EARS, which are
 * CC-BY-NC, and nothing in their names says so — hence the flag, and hence
 * this line on screen beside the choice.
 */
const chosenIsCommercial = computed(() => {
  const chosen = draft.value?.voice.kyutai.voice;
  if (!chosen) return true; // the language default, which is CC0 by design
  return catalogue.value.find((v) => v.id === chosen)?.commercial ?? true;
});

/**
 * One voice on offer, whichever engine provides it.
 *
 * **One shape for both engines, and that is the point.** The two catalogues
 * have nothing in common underneath — Phonon's is a manifest with names,
 * Kyutai ships a fixed list with licences attached — and the row that used to
 * show each of them was written separately, so choosing a voice looked like a
 * different gesture depending on which engine was selected. It is the same
 * gesture. `orb.js` earned this rule the hard way (AGENTS.md: one
 * implementation per component, because the orb was written three times); a
 * voice picker is small enough to make the same mistake in.
 */
interface Persona {
  /** What the engine is given. Empty means "the best one for the language". */
  id: string;
  /** What to show. */
  name: string;
  /** A qualifier under the name: the language tag, or a licence that bites. */
  note?: string;
  /** A short, stable identifier, so two voices can be told apart in a
   *  conversation about them — names may repeat, the id does not. */
  tag?: string;
  /** False greys it out as a choice while leaving it audible. */
  shippable: boolean;
}

/** The empty choice, which both engines have and neither names the same way. */
/** The catalogue is in English; the screen is not. */
const GENDER: Record<string, string> = { female: "femme", male: "homme" };

const BEST: Persona = {
  id: "",
  name: "la meilleure pour la langue",
  shippable: true,
};

/**
 * The voices on offer for the engine currently selected.
 *
 * The default comes **first** rather than in alphabetical place: it is the one
 * answer that is right without knowing anything, and a list that opens on
 * `alba` invites a choice nobody needed to make.
 */
const personas = computed<Persona[]>(() => {
  if (!draft.value) return [];
  switch (draft.value.voice.engine) {
    case "kyutai":
      return [
        BEST,
        ...catalogue.value.map((v) => ({
          id: v.id,
          name: v.id,
          note: v.commercial ? undefined : "licence non commerciale",
          shippable: v.commercial,
        })),
      ];
    case "phonon":
      // The catalogue that carries what a person would actually choose by:
      // a name, a gender and a sentence about the voice. All `apache-2.0`,
      // so nothing here is flagged — the licence that bites for this engine
      // is the model's, one row below.
      return [
        BEST,
        ...phononVoicesOffered.value.map((v, i) => ({
          id: v.id,
          name: v.name || `Voix ${i + 1}`,
          tag: v.id.slice(0, 6),
          note:
            [
              v.language === "fr" ? "" : (LANGUAGE[v.language] ?? v.language),
              GENDER[v.gender] ?? v.gender,
              v.description,
            ]
              .filter(Boolean)
              .join(" · ") || undefined,
          shippable: true,
        })),
      ];
  }
  return [];
});

/** Which one is in force, read from whichever field the engine owns. */
const chosen = computed(() => {
  if (!draft.value) return "";
  switch (draft.value.voice.engine) {
    case "kyutai":
      return draft.value.voice.kyutai.voice;
    case "phonon":
      return draft.value.voice.phonon.voice;
  }
  return "";
});

/** Pick one. Writes to the field the engine owns, never to the other. */
function choose(id: string) {
  if (!draft.value) return;
  switch (draft.value.voice.engine) {
    case "kyutai":
      draft.value.voice.kyutai.voice = id;
      break;
    case "phonon":
      draft.value.voice.phonon.voice = id;
      break;
  }
  void save();
}

/** Open by default when nothing has been chosen, shut once something has. */
const personasOpen = ref(!full.value);

/**
 * Which voice is being auditioned, or `null`.
 *
 * The identifier rather than a flag, so the row that is speaking is the row
 * that lights up. It also holds `""` — the default voice is a legitimate
 * choice to audition, so "nothing selected" cannot be the sentinel.
 */
const trying = ref<string | null>(null);

/**
 * Hear one voice.
 *
 * Takes the identifier rather than reading the selection, so a voice can be
 * **tried before being chosen**. That was the actual complaint about the old
 * row: auditioning meant selecting, which saved, which changed what ARIA
 * sounds like — so comparing three voices left the third one in force whether
 * or not it was the one wanted.
 */
async function hear(id?: string) {
  if (!draft.value || trying.value !== null) return;
  const voice = draft.value.voice;
  const want = id ?? chosen.value;
  trying.value = want;
  try {
    await tryVoice(
      voice.engine,
      want,
      voice.language,
      voice.phonon.model,
      voice.kyutai.quantize,
    );
  } catch (e) {
    error.value = `La voix n'a rien dit : ${e}`;
  } finally {
    trying.value = null;
  }
}

/** What `prepare_*` answered, for the line under the button. */
const prepared = ref<string | null>(null);
const preparing = ref(false);

/**
 * Download the model now instead of at the next launch.
 *
 * One button for the two engines that read something, because from where the
 * user sits it is one action — « mets le modèle sur la machine ». All that
 * differs underneath is which command.
 */
async function prepare() {
  if (!draft.value || preparing.value) return;
  preparing.value = true;
  prepared.value = null;
  const { engine, kyutai, phonon, language } = draft.value.voice;
  try {
    prepared.value =
      engine === "phonon"
        ? await preparePhonon(phonon.model, phonon.voice, language)
        : await prepareKyutai(language, kyutai.voice, kyutai.quantize);
    await checkKyutai();
    await checkPhonon();
  } catch (e) {
    error.value = `Le modèle n'a pas pu être préparé : ${e}`;
  } finally {
    preparing.value = false;
  }
}

/**
 * Phonon's voices are read when they are needed rather than at start-up: an
 * engine nobody selected costs nothing.
 */
watch(
  () => [draft.value?.voice.engine, draft.value?.voice.language] as const,
  ([engine]) => {
    if (engine === "phonon" && phononVoiceList.value.length === 0) {
      void loadPhononVoices();
    }
  },
  { immediate: true },
);

/**
 * What ARIA is told it is, before the user's lines.
 *
 * Shown, not summarised. A field that says "your text is appended" without
 * showing to *what* is a field people fill in blind — and these are the lines
 * that make delegation and verbatim repetition work, so someone writing a
 * persona has to be able to see they are not overruling them.
 */
const shipped = ref("");
const promptOpen = ref(false);
shippedPrompt().then((text) => (shipped.value = text));

/**
 * Measuring the microphone — the first tier of decision 38.
 *
 * `speech_rms` is the one calibration number that depends on the room and the
 * gain rather than on the voice, and getting it wrong is **silent from both
 * ends**: too high and nothing is ever heard, too low and the gate never finds
 * a pause, so nothing is ever transcribed.
 *
 * Measured, then **offered** — never applied behind the user's back. A
 * threshold that changed without being seen is one nobody can undo.
 */
const measuring = ref(false);
const noise = ref<Noise | null>(null);

async function measure() {
  if (measuring.value) return;
  measuring.value = true;
  noise.value = null;
  error.value = null;
  try {
    noise.value = await measureNoise(6);
  } catch (e) {
    error.value = String(e);
  } finally {
    measuring.value = false;
  }
}

/** Store the suggestion under the device it was measured on. */
function applyNoise() {
  if (!draft.value || !noise.value) return;
  const device = noise.value.device;
  const existing = draft.value.calibration[device];
  draft.value.calibration = {
    ...draft.value.calibration,
    // Only `speech_rms` is touched. The other four come from the twenty
    // dictated utterances, which this measurement says nothing about — and
    // overwriting them with defaults would quietly undo that work.
    [device]: { ...(existing ?? DEFAULT_CALIBRATION), speech_rms: noise.value.suggested },
  };
  noise.value = null;
  void save();
}

/** Profiles the active engine cannot honour, listed by name. The profile
 *  itself is chosen from the main view, not here. */
const unsupported = computed(() =>
  (provider.value?.mapping ?? [])
    .filter((row) => row.native === null)
    .map((row) => PROFILE_LABEL[row.profile]),
);

/**
 * Read a shortcut from an actual key press.
 *
 * Typing one into a text field means accepting strings nothing can parse. This
 * builds the accelerator from the event, in the order Tauri expects.
 */
function captureKey(event: KeyboardEvent) {
  if (!capturing.value || !draft.value) return;
  event.preventDefault();

  const key = event.key;
  // A modifier on its own is not a shortcut: keep listening.
  if (["Control", "Alt", "Shift", "Meta"].includes(key)) return;

  const parts: string[] = [];
  if (event.ctrlKey) parts.push("Ctrl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  // Deliberately no Meta: decision 9 keeps the default off the Windows key,
  // because Windows 11 reassigns Win+C to Copilot. Someone who insists can
  // still edit aria.toml.
  if (parts.length === 0) {
    error.value = "Un raccourci global a besoin d'au moins un modificateur.";
    return;
  }
  parts.push(key.length === 1 ? key.toUpperCase() : key);
  draft.value.global_shortcut = parts.join("+");
  capturing.value = false;
  error.value = null;
  // The combination having been *pressed* is the confirmation. This is the one
  // field where saving means acting — registering a shortcut releases the
  // current one — which is why the panel used to wait for a button. Capture
  // replaces it: nobody presses Ctrl+Alt+K by accident, and a refusal
  // ("already taken") comes straight back into `error` with the old shortcut
  // still registered, because `set_config` registers before it writes.
  void save();
}

watch(capturing, (on) => {
  if (on) window.addEventListener("keydown", captureKey);
  else window.removeEventListener("keydown", captureKey);
});

/**
 * Write the settings. Called when a field is left, never from a button.
 *
 * In flight at most once: a toggle clicked twice quickly would otherwise send
 * two writes that can land in either order, and the file would end up holding
 * whichever one the disk finished last rather than the last thing clicked.
 * The second click sets `again`, and the write repeats with the current draft
 * once the first has landed.
 */
let writing = false;
let again = false;

async function save() {
  if (!draft.value) return;
  if (writing) {
    again = true;
    return;
  }
  writing = true;
  error.value = null;
  try {
    await saveConfig(draft.value);
    saved.value = true;
    window.setTimeout(() => (saved.value = false), 1600);
  } catch (e) {
    // "Already taken", most often. The settings are not applied and the file is
    // not written — see `set_config`, which registers the shortcut first for
    // exactly this reason.
    error.value = String(e);
  } finally {
    writing = false;
    if (again) {
      again = false;
      void save();
    }
  }
}
</script>

<template>
  <div>
    <div class="settings" aria-labelledby="setTitle">
      <h2 v-if="full" id="setTitle">
        réglages
        <span class="sp" />
        <!-- In the pinned header, because it is the one place a confirmation
             is visible from wherever you are in the list — and a save that
             happens on leaving a field needs to be seen, or nobody believes
             it happened. -->
        <Transition name="fade">
          <span v-if="saved" class="ok">enregistré</span>
        </Transition>
      </h2>

      <template v-if="draft">
        <!-- Two selectors, because the two posts are two jobs. The one that
             speaks is a dispatcher under a byte budget whose every word is
             spoken; the one that is delegated to does the work. Asked of the
             core, never listed here. -->
        <template v-if="show('agent')">
        <h3 v-if="full" class="grp">Agent principal</h3>

        <!-- The mode first, then the engine within it: which of the two
             decides what the rest of the group even offers. -->
        <div class="srow">
          <div class="lbl">
            <div class="n">Mode</div>
            <div class="d">
              <b>Parakeet + CLI</b> (recommandé) : ta voix est transcrite en
              texte, puis confiée à un assistant en ligne. <b>Modèle vocal
              local</b> : l'IA tourne sur cet ordinateur et entend directement
              ta voix — plus rapide, mais moins capable.
            </div>
          </div>
          <div class="seg" role="radiogroup" aria-label="Mode de l'agent principal">
            <button
              v-for="k in AGENT_KINDS"
              :key="k.kind"
              role="radio"
              :aria-checked="agentKind === k.kind"
              :data-on="agentKind === k.kind ? '1' : '0'"
              :disabled="!agentChoices(k.kind).length"
              @click="pickAgentKind(k.kind)"
            >
              {{ k.label }}
            </button>
          </div>
        </div>

        <!-- Right under the mode (2026-11-22): it moves both the transcription
             and the local model, so it belongs to the agent before anything
             that depends on it. The card found is named, with what runs on it. -->
        <div class="srow">
          <div class="lbl">
            <div class="n">Calcul sur la carte graphique</div>
            <div class="d">
              ARIA est plus rapide sur la carte graphique. Décoche pour la
              laisser libre, pendant un jeu par exemple : ARIA sera plus lente.
              <template v-if="cardLine"><br /><b>{{ cardLine }}</b></template>
              <template
                v-if="card && !card.installed && DOWNLOAD_SIZE[card.runs] && card.loaded !== card.runs"
              >
                <br />Activé, ARIA télécharge une fois
                {{ GPU_STACK_LABEL[card.runs] }} ({{ DOWNLOAD_SIZE[card.runs] }}),
                utilisé au démarrage suivant.
              </template>
            </div>
          </div>
          <button
            class="sw"
            role="switch"
            :data-on="(gpuSwitchable ? draft.gpu : true) ? '1' : '0'"
            :aria-checked="gpuSwitchable ? draft.gpu : true"
            :disabled="!gpuSwitchable"
            aria-label="Calcul sur la carte graphique"
            @click="draft.gpu = !draft.gpu; save()"
          />
        </div>

        <div class="srow">
          <div class="lbl">
            <div class="n">
              {{ agentKind === "local" ? "Modèle vocal" : "CLI" }}
            </div>
          </div>
          <select
            class="txt wide"
            :value="draft.agent.engine"
            :aria-label="agentKind === 'local' ? 'Modèle vocal' : 'CLI'"
            @change="pickEngine('voice', $event)"
          >
            <option
              v-for="choice in agentChoices(agentKind)"
              :key="choice.id"
              :value="choice.id"
            >
              {{ choice.display_name }}
            </option>
          </select>
        </div>

        <template v-if="agentKind === 'cli' && full">
        <!-- The CLI. First of the agent rows because it is the one that
             decides whether any of the others matter: no binary, no post. -->
        <div class="srow">
          <div class="lbl">
            <div class="n">Commande</div>
            <div class="d">
              Le programme à lancer. Laisse vide pour utiliser
              <code>claude</code>.
            </div>
          </div>
          <input
            v-model="draft.agent.command"
            @blur="save"
            class="txt"
            spellcheck="false"
            aria-label="Commande de l'agent principal"
          />
        </div>

        <!-- A list rather than a free field, and the models are asked of the
             engine. The fastest is the default and that is not a compromise:
             this post answers every sentence and is never billed per token. -->
        <div class="srow">
          <div class="lbl">
            <div class="n">Modèle</div>
            <div class="d">
              Le plus rapide suffit pour la conversation. Pris en compte au
              prochain démarrage.
            </div>
          </div>
          <select
            v-model="draft.agent.model"
            @change="save()"
            class="txt wide"
            aria-label="Modèle de l'agent principal"
          >
            <option v-for="m in voiceModels" :key="m.id" :value="m.id">
              {{ m.display_name }} — {{ TIER_LABEL[m.tier] }}
            </option>
          </select>
        </div>
        </template>

        <!-- The persona. Shown next to what it is added to, because a field
             that says "your text is appended" without showing to what is a
             field people fill in blind. -->
        <div v-if="full" class="srow col">
          <div class="lbl">
            <div class="n">
              Consignes personnelles
              <button class="hudbtn tiny" @click="promptOpen = !promptOpen">
                {{ promptOpen ? "masquer" : "voir les règles" }}
              </button>
            </div>
            <div class="d">
              Ce qu'ARIA doit savoir de toi, ou comment elle doit te parler.
              Elles s'ajoutent à ses règles de base sans les remplacer.
            </div>
          </div>
          <pre v-if="promptOpen" class="shipped">{{ shipped }}</pre>
          <textarea
            v-model="draft.agent.persona"
            @blur="save"
            class="area"
            rows="3"
            placeholder="Tutoie-moi. Je m'appelle…"
            aria-label="Consignes personnelles"
          />
        </div>

        </template>

        <template v-if="show('voice')">
        <h3 v-if="full" class="grp">Voix</h3>

        <!-- No on/off switch: silencing ARIA is the orb's job.
             The engine, before the voice: which engine is speaking decides
             what the next row even offers. -->
        <div v-if="ENGINES.length > 1" class="srow">
          <div class="lbl">
            <div class="n">Moteur de voix</div>
            <div class="d">
              {{ ENGINE_EXPLAINED[draft.voice.engine] }}
              <b>Lu au démarrage : le changement prend effet au prochain lancement.</b>
            </div>
          </div>
          <select
            v-model="draft.voice.engine"
            @change="save(); checkKyutai()"
            class="txt wide"
            aria-label="Moteur de voix"
          >
            <option v-for="e in ENGINES" :key="e" :value="e">
              {{ ENGINE_LABEL[e] }}
            </option>
          </select>
        </div>

        <!-- The voice itself, as a sub-menu, and the **same** sub-menu for
             both engines. Choosing a voice is one gesture; it was two rows
             written separately, which made it look like two.

             A list rather than a `<select>` for two reasons that only show up
             with real data: Kyutai ships twenty-seven voices, which is past
             where a dropdown is comfortable, and each row carries its own
             « écouter » — so a voice can be tried **without** being chosen.
             With the dropdown, auditioning meant selecting, which saved, which
             changed what ARIA sounds like: comparing three voices left the
             third one in force whether or not it was wanted. -->
        <div class="srow col">
          <div class="lbl">
            <div class="n">
              Voix
              <button class="hudbtn tiny" @click="personasOpen = !personasOpen">
                {{ personasOpen ? "replier" : `choisir (${personas.length})` }}
              </button>
            </div>
            <div class="d">
              <template v-if="draft.voice.engine === 'kyutai'">
                {{ catalogue.length }} voix livrées avec le modèle, les mêmes
                sur toutes les machines. Le défaut en français est
                <code>estelle</code>, en CC0.
                <b v-if="!chosenIsCommercial">
                  Celle choisie est en CC-BY-NC : pas d'usage commercial.
                </b>
                <b v-else-if="kyutai && !kyutai.loaded">
                  Le modèle n'est pas encore en mémoire : la première
                  « écoute » prendra quelques secondes, les suivantes non.
                </b>
              </template>
              <template v-else-if="draft.voice.engine === 'phonon'">
                {{ phononVoicesOffered.length }} voix disponibles avec ce modèle. « Écouter » en
                essaie une sans la choisir.
              </template>
            </div>
          </div>

          <!-- Shut, the choice in force is still readable: a sub-menu that
               hides what it holds makes you open it to find out nothing
               changed. -->
          <div v-if="!personasOpen" class="chosen">
            <span class="pline">
              <span class="pn">{{
                personas.find((p) => p.id === chosen)?.name ?? chosen
              }}</span>
              <code
                v-if="personas.find((p) => p.id === chosen)?.tag"
                class="ptag"
                :title="chosen"
              >{{ personas.find((p) => p.id === chosen)?.tag }}</code>
            </span>
            <button
              class="hudbtn"
              :disabled="trying !== null"
              @click="hear()"
            >
              {{ trying !== null ? "…" : "écouter" }}
            </button>
          </div>

          <div v-else class="personas" role="radiogroup" aria-label="Voix">
            <div
              v-for="p in personas"
              :key="p.id"
              class="persona"
              :data-on="p.id === chosen ? '1' : '0'"
            >
              <button
                class="pick"
                role="radio"
                :aria-checked="p.id === chosen"
                @click="choose(p.id)"
              >
                <span class="pline">
                  <span class="pn">{{ p.name }}</span>
                  <code v-if="p.tag" class="ptag" :title="p.id">{{ p.tag }}</code>
                </span>
                <span v-if="p.note" class="pnote" :class="{ warn: !p.shippable }">
                  {{ p.note }}
                </span>
              </button>
              <button
                class="hudbtn tiny"
                :disabled="trying !== null"
                @click="hear(p.id)"
              >
                {{ trying === p.id ? "…" : "écouter" }}
              </button>
            </div>
          </div>
        </div>

        <!-- Phonon : le modèle, et la licence qui va avec. Le reste — la voix —
             est le sous-menu partagé au-dessus. -->
        <template v-if="draft.voice.engine === 'phonon'">
          <div class="srow">
            <div class="lbl">
              <div class="n">Modèle de voix</div>
              <div class="d">
                Change dès la phrase suivante, après quelques secondes de
                chargement.
                <b v-if="draft.voice.language.startsWith('fr') && phononModel?.language === 'en'">
                  Ce modèle est anglais : il parlera français avec un accent.
                </b>
                <b v-if="phononModel?.gated && !phononModel.cached">
                  Son téléchargement demande un compte Hugging Face.
                </b>
              </div>
            </div>
            <select
              :value="phononModel?.repo"
              @change="chooseModel(($event.target as HTMLSelectElement).value)"
              class="txt wide"
              aria-label="Modèle Phonon"
            >
              <option v-for="m in phononOffered" :key="m.repo" :value="m.repo">
                {{ m.repo.replace("gradium/phonon-", "") }} ·
                {{ m.voices.length > 1 ? "multilingue" : m.language }}{{ m.commercial ? "" : " (NC)" }}
              </option>
            </select>
          </div>
          <div v-if="phononVoicesFailed" class="srow">
            <div class="lbl">
              <div class="n">Liste des voix</div>
              <div class="d">
                <b>
                  La liste n'a pas pu être chargée (pas de connexion ?). La voix
                  par défaut est utilisée en attendant.
                </b>
              </div>
            </div>
            <button class="hudbtn" @click="loadPhononVoices">réessayer</button>
          </div>
        </template>

        <!-- Kyutai : où vont ses poids, et en quelle précision. La voix, elle,
             est le sous-menu partagé au-dessus.

             **Deux lignes de moins qu'avant, et c'est l'objet de la décision
             56.** Il y avait une ligne « Binaire Pocket TTS » — un chemin à
             remplir, un .zip à aller chercher sur une page de releases — et un
             port à régler parce qu'un port peut être pris. Lier le modèle n'a
             pas répondu à ces deux questions : il les a supprimées. -->
        <template v-if="draft.voice.engine === 'kyutai'">
          <!-- The weights, downloaded when the engine is chosen instead of at
               the next launch.

               Left alone, the model fetches them the first time it is asked to
               speak — which is at start-up, behind an orb that stays dim for
               four hundred megabytes with nothing saying why. Here it is a
               button, with the orb's own loading bar behind it. -->
          <div class="srow col">
            <div class="lbl">
              <div class="n">
                Modèle
                <button class="hudbtn tiny" :disabled="preparing" @click="prepare">
                  {{
                    preparing
                      ? "chargement…"
                      : kyutai?.loaded
                        ? "recharger"
                        : kyutai?.weights
                          ? "charger la voix"
                          : "télécharger la voix"
                  }}
                </button>
              </div>
              <div class="d">
                <template v-if="kyutai?.weights">
                  Déjà sur la machine, dans <code>{{ kyutai.cache }}</code>.
                </template>
                <template v-else>
                  Environ 400 Mo, depuis un dépôt public : aucun compte ni jeton
                  Hugging Face n'est nécessaire. Ils vont dans
                  <code>{{ kyutai?.cache }}</code> — un dossier qu'ARIA impose,
                  parce que la bibliothèque mettrait sinon son cache là d'où
                  ARIA a été lancée, c'est-à-dire dans le dépôt.
                </template>
                Le modèle reste ensuite en mémoire dans le processus d'ARIA :
                les voix du sous-menu sont écoutables tout de suite, et il n'y
                a qu'un seul exemplaire chargé, partagé entre l'écoute d'essai
                et la voix qui parle.
              </div>
            </div>
            <div v-if="prepared" class="result">{{ prepared }}</div>
          </div>

          <div class="srow">
            <div class="lbl">
              <div class="n">Mémoire</div>
              <div class="d">
                Les poids en int8 tiennent moins de mémoire, pour un son
                légèrement différent. C'est le seul réglage qui change ce qu'on
                entend — et basculer le fait charger un second exemplaire,
                jusqu'au prochain lancement.
              </div>
            </div>
            <button
              class="sw"
              role="switch"
              :data-on="draft.voice.kyutai.quantize ? '1' : '0'"
              :aria-checked="draft.voice.kyutai.quantize"
              aria-label="Poids int8"
              @click="draft.voice.kyutai.quantize = !draft.voice.kyutai.quantize; save()"
            />
          </div>
        </template>

        </template>

        <template v-if="show('listen')">
        <h3 v-if="full" class="grp">Écoute</h3>

        <!-- Calibration. First tier: the noise floor, which is the one number
             that depends on the room rather than on the voice. -->
        <div class="srow col">
          <div class="lbl">
            <div class="n">Calibrer le micro</div>
            <div class="d">
              ARIA écoute six secondes de silence pour régler la sensibilité
              du micro. À refaire si elle t'entend mal ou réagit au bruit.
              <b v-if="store.mode === 'off'">Active d'abord le micro.</b>
            </div>
          </div>
          <div class="pair">
            <button
              class="hudbtn"
              :disabled="measuring || store.mode === 'off'"
              @click="measure"
            >
              {{ measuring ? "ne dis rien…" : "mesurer" }}
            </button>
          </div>
          <div v-if="noise" class="result">
            <p>{{ noise.verdict }}</p>
            <div class="pair">
              <button class="hudbtn" @click="applyNoise">appliquer</button>
              <button class="hudbtn" @click="noise = null">ignorer</button>
            </div>
            <p class="tiny-note">
              Réglage propre à ce micro : {{ noise.device }}.
            </p>
          </div>
        </div>

        </template>

        <template v-if="full">
        <h3 class="grp">Tâches déléguées</h3>

        <div class="srow">
          <div class="lbl">
            <div class="n">Moteur des tâches déléguées</div>
            <div class="d">
              Le programme qui fait les tâches longues qu'ARIA lui confie.<br />
              {{
                provider
                  ? availabilityLabel(provider.availability)
                  : "aucun moteur attaché"
              }}
              <!-- The three words the voice agent chooses between on its own,
                   and what each one will actually run. Shown because it picks
                   one without asking, and the user is entitled to know. -->
              <span v-for="row in engineList?.delegation.tiers ?? []" :key="row.word">
                <br />{{ TIER_LABEL[row.tier] }} →
                {{ row.model ? row.model.display_name : "défaut du moteur" }}
              </span>
              <b v-if="unsupported.length">
                Profils non pris en charge par {{ provider?.display_name }} :
                {{ unsupported.join(", ") }}.
              </b>
              <!-- A capability you cannot see missing is as much a trap as a
                   profile silently absent. An engine that cannot be asked for
                   permission stops showing cards, and that has to be read. -->
              <b v-for="line in provider?.shortcomings ?? []" :key="line">
                {{ provider?.display_name }} {{ line }}.
              </b>
            </div>
          </div>
          <select
            class="txt wide"
            :value="draft.delegation.engine"
            aria-label="Moteur des tâches déléguées"
            @change="pickEngine('delegation', $event)"
          >
            <option
              v-for="choice in engineList?.delegation.choices ?? []"
              :key="choice.id"
              :value="choice.id"
            >
              {{ choice.display_name }} — {{ ENGINE_KIND[choice.kind] }}
            </option>
          </select>
        </div>

        <!-- Per engine, and no longer global: `2.1.165` means something to
             Claude Code and nothing at all to anything else. -->
        <div class="srow">
          <div class="lbl">
            <div class="n">Commande</div>
            <div class="d">Le programme à lancer pour ces tâches.</div>
          </div>
          <input
            v-model="draft.delegation.command"
            @blur="save"
            class="txt"
            spellcheck="false"
            aria-label="Commande des tâches déléguées"
          />
        </div>

        <h3 class="grp">Application</h3>

        <div class="srow">
          <div class="lbl">
            <div class="n">Rester en arrière-plan</div>
            <div class="d">
              ARIA reste disponible quand sa fenêtre est fermée, et les tâches
              en cours se terminent.
              <b v-if="!draft.stay_in_background">
                Décoché, fermer la fenêtre quitte ARIA et arrête les tâches en
                cours.
              </b>
            </div>
          </div>
          <button
            class="sw"
            role="switch"
            :data-on="draft.stay_in_background ? '1' : '0'"
            :aria-checked="draft.stay_in_background"
            aria-label="Rester en arrière-plan"
            @click="draft.stay_in_background = !draft.stay_in_background; save()"
          />
        </div>

        <div class="srow">
          <div class="lbl">
            <div class="n">Raccourci global</div>
            <div class="d">
              Affiche ou cache ARIA depuis n'importe où. Clique, puis appuie
              sur la combinaison voulue.
            </div>
          </div>
          <button class="skbd" @click="capturing = true">
            {{ capturing ? "appuie…" : draft.global_shortcut }}
          </button>
        </div>

        <div class="srow">
          <div class="lbl">
            <div class="n">Libérer la mémoire en veille</div>
            <div class="d">
              ARIA prend moins de mémoire quand elle dort, mais met environ
              4 secondes à se réveiller.
            </div>
          </div>
          <button
            class="sw"
            role="switch"
            :data-on="draft.unload_audio_on_sleep ? '1' : '0'"
            :aria-checked="draft.unload_audio_on_sleep"
            aria-label="Décharger les modèles audio en veille"
            @click="draft.unload_audio_on_sleep = !draft.unload_audio_on_sleep; save()"
          />
        </div>

        <div class="srow">
          <div class="lbl">
            <div class="n">Mises à jour</div>
            <div class="d">
              Version {{ version || "…" }}. ARIA vérifie au démarrage puis toutes
              les six heures, et propose l'installation.
              <b v-if="updateNote">{{ updateNote }}</b>
            </div>
          </div>
          <button class="hudbtn" :disabled="update.checking" @click="recheck">
            {{ update.checking ? "…" : "vérifier" }}
          </button>
        </div>

        </template>

        <div v-if="error" class="swarn">{{ error }}</div>

        <p v-if="full" class="muted foot">
          Les réglages s'enregistrent tout seuls. Certains ne s'appliquent
          qu'au prochain démarrage.
        </p>
      </template>

      <div v-else-if="error" class="swarn">{{ error }}</div>
      <div v-else class="muted">Lecture des réglages…</div>
    </div>
  </div>
</template>

<style scoped>
/* Inside the menu's own scroller now: no frame, no shadow, no height of its
   own. The menu is the surface; this is its content. */
.settings {
  padding: 20px 22px 18px;
}

/* Pinned to the top of the scrolling box, so « fermer » is reachable from
   anywhere in the list. The panel is long enough that the button would
   otherwise be several screens above someone reading the last row — and
   AGENTS.md is blunt about it: nothing important goes where it can be
   scrolled out of reach.

   The negative margins and the padding give it the panel's own gutter back,
   so the backdrop covers the full width as rows slide under it rather than
   letting them show at the edges. */
h2 {
  position: sticky;
  top: 0;
  z-index: 2;
  margin: -20px -22px 14px;
  padding: 20px 22px 12px;
  /* Fully opaque: `--panel` is itself translucent, so anything mixed from it
     let the rows scrolling beneath show through the title. */
  background: #050a12; /* --panel laid over --bg, made solid */
  border-bottom: 1px solid rgba(64, 220, 255, 0.1);
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.3em;
  text-transform: uppercase;
  color: var(--chrome);
  display: flex;
  align-items: center;
  gap: 10px;
}
h2 .sp {
  flex: 1;
}

/* A group title. The first row of a group drops its rule: the title already
   separates it from the group above. */
.grp {
  margin: 22px 0 2px;
  font-family: var(--font-mono);
  font-size: 10.5px;
  font-weight: 500;
  letter-spacing: 0.24em;
  text-transform: uppercase;
  color: var(--cyan);
}
.grp:first-of-type {
  margin-top: 4px;
}
.grp + .srow {
  border-top: none;
}

/* Two buttons acting as one choice. */
.seg {
  display: flex;
  flex-shrink: 0;
  border: 1px solid var(--line);
  border-radius: 8px;
  overflow: hidden;
}
.seg button {
  padding: 6px 10px;
  background: transparent;
  border: none;
  color: var(--dim);
  font: inherit;
  font-size: 12px;
  cursor: pointer;
}
.seg button + button {
  border-left: 1px solid var(--line);
}
.seg button[data-on="1"] {
  background: rgba(64, 220, 255, 0.12);
  color: var(--text);
}
.seg button:disabled {
  opacity: 0.4;
  cursor: default;
}
.seg button:focus-visible {
  outline: 2px solid var(--focus);
  outline-offset: -2px;
}

.srow {
  display: flex;
  align-items: flex-start;
  gap: 14px;
  padding: 11px 0;
  border-top: 1px solid rgba(64, 220, 255, 0.08);
}
.srow .lbl {
  flex: 1;
  min-width: 0;
}
.srow .lbl .n {
  font-size: 14px;
  color: var(--text);
}
.srow .lbl .d {
  font-size: 11.5px;
  color: var(--dim);
  line-height: 1.4;
  margin-top: 3px;
}
.srow .lbl .d code {
  font-family: var(--font-mono);
  color: var(--cyan);
  font-size: 11px;
}
.srow .lbl .d b {
  display: block;
  margin-top: 5px;
  color: var(--chrome);
  font-weight: 500;
}

/* A row whose control is too wide to sit beside its label — the persona, and
   anything else that is a paragraph rather than a value. */
.srow.col {
  flex-direction: column;
  align-items: stretch;
  gap: 8px;
}

.srow .lbl .n .tiny {
  font-size: 11px;
  padding: 2px 7px;
  margin-left: 9px;
  vertical-align: middle;
}

.txt,
.area {
  background: rgba(8, 14, 24, 0.55);
  border: 1px solid var(--line);
  border-radius: var(--r-sm, 8px);
  color: var(--text);
  font-family: var(--font-hud);
  font-size: 12.5px;
  padding: 7px 10px;
  min-width: 0;
}
.txt {
  flex: 0 0 auto;
  width: 148px;
}
.txt.wide {
  width: 212px;
}
/* A port number is five digits. A field sized for a path beside it would
   invite someone to type something else into it. */
.txt.narrow {
  width: 72px;
}
.area {
  width: 100%;
  resize: vertical;
  font-size: 12px;
  line-height: 1.45;
}
/* A field shows focus on its own border rather than with an outline around
   it — but in the shared focus colour, like every other control. */
.txt:focus,
.area:focus {
  outline: none;
  border-color: var(--focus);
  box-shadow: 0 0 0 1px var(--focus);
}

/* What ARIA is told it is. Monospaced and dimmed: it is quoted, not offered
   for editing — the point is to show what the persona is added to. */
.shipped {
  margin: 0;
  padding: 9px 11px;
  background: rgba(8, 14, 24, 0.7);
  border: 1px solid rgba(64, 220, 255, 0.12);
  border-radius: var(--r-sm, 8px);
  color: var(--dim);
  font-family: var(--font-mono);
  font-size: 11px;
  line-height: 1.5;
  white-space: pre-wrap;
  max-height: 180px;
  overflow-y: auto;
}


/* The voice sub-menu. A list and not a dropdown, because twenty-seven entries
   each carry their own « écouter » — see the comment on it in the template. */
.personas {
  display: flex;
  flex-direction: column;
  gap: 2px;
  width: 100%;
  max-height: 320px;
  overflow-y: auto;
  overflow-x: hidden;
  /* Its own scroll, inside a panel that already scrolls. Without the bound,
     twenty-seven rows push every setting below them off the screen and the
     sub-menu stops reading as a sub-menu. */
  padding: 4px;
  border: 1px solid rgba(255, 255, 255, 0.07);
  border-radius: 8px;
  background: rgba(0, 0, 0, 0.16);
}

.persona {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 5px 4px 5px 8px;
  border-radius: 6px;
}
/* « écouter » keeps its width whatever the description is. */
.persona > .hudbtn {
  flex: 0 0 auto;
}
.persona[data-on="1"] {
  background: rgba(183, 164, 255, 0.1);
  box-shadow: inset 2px 0 0 var(--chrome);
}

/* The name is the hit area, so picking does not need the cursor on a dot. */
.pick {
  flex: 1 1 auto;
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 1px;
  min-width: 0;
  background: none;
  border: 0;
  padding: 2px 0;
  color: inherit;
  font: inherit;
  text-align: left;
  cursor: pointer;
}
.pick:focus-visible {
  outline: 2px solid var(--focus);
  outline-offset: 2px;
}

.pn {
  font-size: 12px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.persona[data-on="1"] .pn {
  color: var(--chrome);
}

/* Under the name and wrapping, never beside it: on one line, a sentence of
   description ate the name and ran under « écouter ». */
.pnote {
  font-size: 11px;
  line-height: 1.35;
  opacity: 0.55;
  white-space: normal;
  overflow-wrap: anywhere;
}

/* The name and its identifier, on the first line. */
.pline {
  display: flex;
  align-items: baseline;
  gap: 6px;
  min-width: 0;
}
.ptag {
  flex: 0 0 auto;
  font-size: 10px;
  opacity: 0.45;
  letter-spacing: 0.04em;
}
/* A licence that forbids shipping is not a detail among details. */
.pnote.warn {
  opacity: 0.9;
  color: var(--chrome);
}

/* Folded: what is in force, still readable without opening anything. */
.chosen {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  width: 100%;
}
.chosen .pn {
  color: var(--chrome);
}

.pair {
  display: flex;
  gap: 7px;
  align-items: center;
  flex: 0 0 auto;
}

/* What a measurement found. Framed, because it is an offer awaiting a
   decision and not a value that has been applied. */
.result {
  border: 1px solid rgba(183, 164, 255, 0.3);
  border-radius: var(--r-sm, 8px);
  background: rgba(183, 164, 255, 0.05);
  padding: 10px 12px;
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.result .nums {
  display: flex;
  flex-wrap: wrap;
  gap: 4px 16px;
  font-size: 11px;
  color: var(--dim);
}
.result code {
  font-family: var(--font-mono);
  color: var(--chrome);
  font-size: 11px;
}
.result p {
  margin: 0;
  font-size: 12px;
  color: var(--text);
  line-height: 1.45;
}
.result .tiny-note {
  font-size: 11px;
  color: var(--dim);
  line-height: 1.5;
}

.sw {
  flex: 0 0 auto;
  width: 48px;
  height: 28px;
  border: 1px solid var(--line);
  border-radius: 999px;
  background: rgba(255, 255, 255, 0.03);
  cursor: pointer;
  position: relative;
  transition: 0.18s;
}
.sw::after {
  content: "";
  position: absolute;
  top: 4.5px;
  left: 3px;
  width: 17px;
  height: 17px;
  border-radius: 50%;
  background: var(--dim);
  transition: 0.18s;
}
.sw[data-on="1"] {
  border-color: rgba(183, 164, 255, 0.6);
  background: rgba(183, 164, 255, 0.12);
  box-shadow: 0 0 14px rgba(183, 164, 255, 0.22);
}
.sw:disabled {
  cursor: not-allowed;
  opacity: 0.45;
}
.sw[data-on="1"]::after {
  left: 26px;
  background: var(--chrome);
  box-shadow: 0 0 10px var(--chrome);
}
.sw:focus-visible {
  outline: 2px solid var(--focus);
  outline-offset: 3px;
}

.skbd {
  font-family: var(--font-mono);
  font-size: 11px;
  color: var(--cyan);
  border: 1px solid var(--line);
  border-radius: 999px;
  padding: 5px 11px;
  cursor: pointer;
  background: transparent;
  white-space: nowrap;
}
.skbd:hover {
  border-color: var(--cyan);
}

.ver {
  font-family: var(--font-mono);
  font-size: 11px;
  color: var(--dim);
  padding-top: 4px;
}

.hudbtn[data-p="strict"] {
  color: var(--err);
  border-color: rgba(255, 84, 112, 0.4);
}
.hudbtn[data-p="normal"] {
  color: var(--cyan);
  border-color: rgba(64, 220, 255, 0.4);
}
.hudbtn[data-p="autonomous"] {
  color: var(--chrome);
  border-color: rgba(183, 164, 255, 0.4);
}

.swarn {
  margin-top: 12px;
  font-size: 11.5px;
  line-height: 1.45;
  color: var(--chrome);
  border-left: 2px solid var(--chrome);
  padding-left: 10px;
}

h2 .ok {
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.18em;
  text-transform: uppercase;
  color: var(--ok);
}

.fade-enter-active,
.fade-leave-active {
  transition: opacity 0.25s ease;
}
.fade-enter-from,
.fade-leave-to {
  opacity: 0;
}

/* The closing note: what a save does, and what it does not. */
.foot {
  margin: 18px 0 0;
  padding-top: 14px;
  border-top: 1px solid rgba(64, 220, 255, 0.08);
  font-size: 11px;
  line-height: 1.5;
}
</style>
