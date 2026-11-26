<script setup lang="ts">
/**
 * The main window.
 *
 * Three layers, and the order is the mock-up's: the two menus float **over**
 * the stage rather than squeezing it, the stage holds the orb, and the input
 * row and the menu button keep the full width underneath both panels — which
 * is what `--bg-bottom` is for.
 *
 * Nothing here holds state of its own. The core publishes, the stream
 * delivers, `applyEvent` folds, these components read. A click asks the core
 * and waits for the event to come back; it never flips a local flag, because a
 * second source of truth about "is ARIA listening" is the one thing this
 * product cannot afford.
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";

import CommandBar from "./components/CommandBar.vue";
import Conversations from "./components/Conversations.vue";
import TileLayer from "./components/TileLayer.vue";
import TopBar from "./components/TopBar.vue";
import UpdateOffer from "./components/UpdateOffer.vue";
import { watchUpdates } from "./lib/update";
import Orb from "./orb/Orb.vue";
import {
  connect,
  getConfig,
  saveConfig,
  sayToAgent,
  stopAll,
  toggleAudio,
} from "./lib/bridge";
import { LOAD_LABEL, MODE_LABEL, type Profile } from "./lib/contract";
import { exchange, liveSessions, markAsked, store } from "./lib/store";

const leftOpen = ref(false);
/** Which tab of the left-hand menu is showing. Held here so it can be switched. */
const leftTab = ref<"threads" | "sessions" | "settings" | "scenarios">("threads");

/**
 * A permission request opens the menu, on the sessions tab.
 *
 * Asked for by the user on 2026-11-16, when the sessions moved into the left
 * menu: a request blocks its session until someone answers and never expires
 * (decision 8), so it cannot be left behind a closed panel or another tab.
 * Only on a **new** request — a count that goes down is an answer, and the
 * menu the user arranged is left alone. `immediate`, so a request already
 * pending when the window opens is shown too.
 */
watch(
  () => store.pending.length,
  (now, before) => {
    if (now > (before ?? 0)) {
      leftOpen.value = true;
      leftTab.value = "sessions";
    }
  },
  { immediate: true },
);
/** What was typed, until there is a turn engine to hand it to. */
const typed = ref<string | null>(null);

/**
 * Open the connection, and **say why if it does not open**.
 *
 * `onMounted(connect)` on its own was a quiet trap: a rejected promise in a
 * lifecycle hook is an unhandled rejection, so the reason went to the webview
 * console — which nobody has open — and the screen showed "core injoignable"
 * with no way to find out more. The status line is where that belongs.
 *
 * Retried, because the first failure may be a start-up race: the window is a
 * client of the core, and a client that gives up after one attempt is a window
 * that stays blank for no reproducible reason.
 */
onMounted(async () => {
  for (let attempt = 1; ; attempt += 1) {
    try {
      await connect();
      return;
    } catch (e) {
      store.notice = `core injoignable : ${e}`;
      console.error("connect failed", e);
      if (attempt >= 5) return;
      await new Promise((wait) => setTimeout(wait, 400 * attempt));
    }
  }
});

/**
 * Escape closes the menu, or else is STOP.
 *
 * In that order, and it matters: STOP cuts the running sessions, so it must not
 * be what Escape does while the menu is open and Escape means "never mind".
 * Until 2026-11-16 only the settings tab was spared; any open menu is now.
 */
function onKey(event: KeyboardEvent) {
  if (event.key !== "Escape") return;
  if (leftOpen.value) {
    leftOpen.value = false;
    return;
  }
  stopAll();
}

/**
 * A press outside the menu closes it.
 *
 * Not while a permission request is waiting: the menu opened itself for it
 * (see the watch above), and a stray click must not hide the only place it
 * can be answered. The menu button is excluded because it toggles by itself,
 * and the tiles because dragging one over the menu is not a wish to close it.
 */
function onPress(event: PointerEvent) {
  if (!leftOpen.value || store.pending.length > 0) return;
  const target = event.target as Element | null;
  if (target?.closest(".panel.left, .bar .menu, #tiles .tile")) return;
  leftOpen.value = false;
}

onMounted(() => {
  watchUpdates();
  window.addEventListener("keydown", onKey);
  window.addEventListener("pointerdown", onPress);
});
onBeforeUnmount(() => {
  window.removeEventListener("keydown", onKey);
  window.removeEventListener("pointerdown", onPress);
});

const PROFILES: Profile[] = ["plan", "strict", "normal", "autonomous"];

/**
 * The quick cycle in the status strip — and it writes the same file the
 * settings screen does.
 *
 * It would have been shorter to poke the core directly and leave the file
 * alone. That is how the status strip and the settings screen end up disagreeing
 * about the profile, and how a profile someone chose is gone after a restart.
 * One source of truth, even for a one-click control.
 */
async function cycleProfile() {
  const config = await getConfig();
  const next = PROFILES[(PROFILES.indexOf(config.profile) + 1) % PROFILES.length];
  await saveConfig({ ...config, profile: next });
  // Settings are not events — the profile is a stored value, not something that
  // happened — so this one is read back rather than awaited from the stream.
  store.profile = next;
}

/**
 * What the transcript area shows.
 *
 * What was *heard* wins over what was answered: while someone is speaking, the
 * thing they most need on screen is whether the machine got the words right.
 */
const transcript = computed(() => {
  if (store.heard) return store.heard.text;
  if (typed.value) return typed.value;
  const latest = store.sessions.at(-1);
  return latest?.message || latest?.outcome?.text || "";
});

/** A partial transcript is shown dimmed: it will still be corrected. */
const transcriptPartial = computed(() => store.heard !== null && !store.heard.final);

/**
 * What ARIA said, last few lines.
 *
 * On screen because there is no voice yet (T4). A turn is often two utterances
 * — a waiting phrase, then the answer — so more than the last one is shown:
 * replacing the cover with the answer would make it look like a false start.
 * When the TTS lands this stays, because a reconnecting window has to be able
 * to see what was said and audio cannot be re-read.
 */
const around = computed(exchange);
const said = computed(() => around.value.after);
/** ARIA's line before the user's sentence: above it, faded (2026-11-26). */
const before = computed(() => (transcript.value ? around.value.before : null));

/** Which of ARIA's lines was just copied, for the « copié » acknowledgement. */
const copied = ref<number | null>(null);

/**
 * Copy one of ARIA's answers — asked for on 2026-11-16, on hover.
 *
 * Said back on the button itself rather than in the status line: the eye is
 * on the line that was copied, not on the orb.
 */
async function copy(line: string, i: number) {
  try {
    await navigator.clipboard.writeText(line);
    copied.value = i;
    window.setTimeout(() => {
      if (copied.value === i) copied.value = null;
    }, 1400);
  } catch (e) {
    store.notice = `copie impossible : ${e}`;
  }
}

/**
 * How bright the orb is, 0 to 1.
 *
 * Three readings, not two. It used to be a flag — dark unless a session was
 * running or the mode was active — which made an open microphone waiting for
 * a word look exactly like a closed one. **Listening is not idling**, and the
 * orb is the only thing on screen that says so.
 *
 * The listening level is deliberately close to the full one rather than half
 * way: the distinction that must read at a glance is *shut* against *open*,
 * and the one between open-and-waiting and open-and-answering is already
 * carried by the energy band, which moves.
 */
const brightness = computed(() => {
  if (store.mode === "off") return 0;
  // Dim while **any** model is being read. The ear is not open yet, so an orb
  // that already looks awake is an orb claiming to hear — which is the one
  // thing it must not do while it cannot. Since the loading state covers the
  // voice too, this now also holds through a voice that is still loading: ARIA
  // may be able to hear by then, and it still has nothing to answer with.
  if (store.voice === "loading") return 0.22;
  if (
    liveSessions().length > 0 ||
    store.mode === "active" ||
    store.voice === "speaking" ||
    store.voice === "thinking"
  ) {
    return 1;
  }
  return 0.78;
});


/**
 * What is still being read, for the label under the orb.
 *
 * Named rather than counted, because the two models are not interchangeable
 * from where the user sits: waiting for the ear and waiting for the voice are
 * different waits, and one of them can be a download. A bar at 95 % next to
 * the word « la voix » is informative; the same bar on its own is not.
 */
const loadWhat = computed(() => {
  const pending = store.loading.pending.map((what) => LOAD_LABEL[what]);
  if (pending.length === 0) return undefined;
  return pending.join(" et ");
});

const status = computed(() => {
  // The notice comes FIRST when disconnected, and that ordering is the whole
  // point of it. The other way round — which is how this was written — means
  // "core injoignable" always wins and the reason it was unreachable is never
  // shown, which is a status line that reports the symptom and swallows the
  // cause.
  if (!store.connected) return store.notice ?? "core injoignable";
  if (store.notice) return store.notice;
  if (store.mode === "off") return "canal vocal fermé";
  if (store.mode === "hybrid") return "dis son nom pour parler";
  return MODE_LABEL[store.mode];
});

/**
 * Typing goes to the main agent, exactly as speaking does.
 *
 * It used to open a Claude Code session per line, because the intent router was
 * T3's and did not exist. It does now: the agent answers what it can and calls
 * `delegate_task` for the rest, so the session appears **when the agent decides
 * it needs one** rather than because something was typed.
 *
 * Nothing is rendered from the return value except the one thing it is for:
 * whether an agent was there to hear it. Everything else arrives as events —
 * the transcript, the answer, any session the agent opens — because a second
 * source of truth about what was said is the thing this design avoids.
 */
async function send(text: string) {
  markAsked();
  typed.value = text;
  store.notice = null;
  try {
    if (!(await sayToAgent(text))) {
      // Not an error: ARIA runs with its agent off when the CLI is missing. But
      // silence and absence look identical on screen, and they are not the
      // same thing to the person waiting.
      store.notice = "aucun agent ne tourne — vérifie la CLI dans les réglages";
    }
  } catch (e) {
    store.notice = String(e);
  }
}
</script>

<template>
  <TopBar
    @cycle-profile="cycleProfile"
    @open-sessions="
      leftOpen = true;
      leftTab = 'sessions';
    "
  />

  <Conversations v-if="leftOpen" v-model:tab="leftTab" />

  <!-- STOP lived at the foot of this column until 2026-11-16. It is the square
       to the right of the input now — still outside any panel, still never
       behind a toggle, because it cuts the work as well as the ear. -->

  <main class="stage" :data-left="leftOpen ? 'on' : 'off'">
    <!-- The exchange in the order it happened (2026-11-26): ARIA's line before
         the user's sentence, faded, then the sentence, then what ARIA said
         since. The sentence used to sit above both of ARIA's lines, even the
         one that came before it. -->
    <div class="exchange">
    <p v-if="before" class="before" :title="before">{{ before }}</p>

    <!-- One line: what was heard is there to be checked at a glance, and the
         whole of it is in the hover text when it runs long. -->
    <div
      class="transcript"
      :class="{ partial: transcriptPartial }"
      :title="transcript"
      aria-live="polite"
    >
      {{ transcript }}
    </div>

    <!-- ARIA's side of the exchange. Its own block, not folded into the
         transcript above: one is what the machine heard, the other is what it
         answered, and showing them in the same place is how you end up unable
         to tell a mis-hearing from a bad answer. -->
    <!-- Two lines at most since 2026-11-16: the answer, and the one before it
         faded, so a waiting phrase followed by the answer does not read as a
         false start. Each can be copied. -->
    <div v-if="said.length" class="said" aria-live="polite">
      <p
        v-for="(line, i) in said"
        :key="`${store.spoken.length}-${i}`"
        :class="{ latest: i === said.length - 1 }"
      >
        <span class="line">{{ line }}</span>
        <button
          class="copy"
          :title="copied === i ? 'Copié' : 'Copier'"
          :aria-label="copied === i ? 'Copié' : 'Copier cette réponse'"
          @click="copy(line, i)"
        >
          <svg v-if="copied !== i" viewBox="0 0 24 24" aria-hidden="true">
            <rect x="8" y="8" width="12" height="12" rx="2" />
            <path d="M16 8V5a1 1 0 0 0-1-1H5a1 1 0 0 0-1 1v10a1 1 0 0 0 1 1h3" />
          </svg>
          <svg v-else viewBox="0 0 24 24" aria-hidden="true">
            <path d="M5 12l5 5 9-10" />
          </svg>
        </button>
      </p>
    </div>
    </div>

    <div class="orb-wrap">
      <Orb
        :listening="store.mode !== 'off'"
        :alive="brightness"
        :energy="store.level"
        :loading="store.voice === 'loading'"
        :load-done="store.loading.done"
        :load-total="store.loading.total"
        :load-what="loadWhat"
        :speaking="store.voice === 'speaking'"
        @toggle="toggleAudio"
      />
      <div class="status">{{ status }}</div>
    </div>

    <CommandBar
      @send="send"
      @toggle-left="leftOpen = !leftOpen"
      @stop="stopAll"
    />
  </main>

  <!-- Above everything and inert to the mouse: a tile must be draggable over
       both panels and over the input row, and nothing underneath may stop
       being clickable because the layer is there. -->
  <TileLayer />

  <UpdateOffer />
</template>

<style scoped>
.stage {
  flex: 1;
  position: relative;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  z-index: 3;
  min-width: 0;
}

/* The gradient that makes the input row readable over the scene. It catches
   nothing, and the tiles layer will sit above it. */
.stage::after {
  content: "";
  position: absolute;
  left: 0;
  right: 0;
  bottom: 0;
  height: 190px;
  pointer-events: none;
  z-index: 5;
  background: linear-gradient(
    to top,
    rgba(1, 4, 9, 0.92) 0%,
    rgba(1, 4, 9, 0.55) 45%,
    transparent 100%
  );
}

.orb-wrap {
  position: relative;
  display: flex;
  flex-direction: column;
  align-items: center;
}

.status {
  margin-top: 10px;
  letter-spacing: 0.4em;
  text-transform: uppercase;
  font-size: 12px;
  font-weight: 600;
  color: var(--dim);
  font-family: var(--font-mono);
  text-align: center;
}


.transcript.partial {
  color: var(--dim);
  font-style: italic;
}

/* ARIA's own voice: amber, where the user's transcript is cyan. The palette
   already carries that split — the orb is amber when ARIA acts — so the colour
   says who is talking without a label. */
/* Before, the sentence, after: one column, so a line before the sentence pushes
   the rest down instead of covering it. */
.exchange {
  position: absolute;
  top: 72px;
  left: 50%;
  transform: translateX(-50%);
  width: min(620px, 54vw);
  z-index: 4;
  display: flex;
  flex-direction: column;
  gap: 10px;
}

/* ARIA's line before the sentence: as faded as the old line under the answer. */
.exchange .before {
  margin: 0;
  font-family: var(--font-read);
  font-size: 13px;
  line-height: 1.5;
  text-align: center;
  color: var(--dim);
  opacity: 0.45;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.said {
  text-align: center;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

/* The line before: faded, smaller — context, not something to read. */
.said p {
  position: relative;
  margin: 0;
  /* Read, not glanced at: the reading face rather than the condensed HUD one
     (2026-11-17). */
  font-family: var(--font-read);
  font-size: 13px;
  line-height: 1.5;
  color: var(--dim);
  opacity: 0.45;
  transition: opacity 0.2s;
}
.said p:hover {
  opacity: 0.9;
}

/* The latest line is the one being said. */
.said p.latest {
  font-size: 15.5px;
  font-weight: 500;
  color: var(--amber);
  opacity: 1;
  text-shadow: 0 0 18px rgba(255, 179, 71, 0.3);
}

/* Copy, on hover only, hung just past the line's end so it never pushes the
   centred text sideways. */
.said .copy {
  display: inline-grid;
  place-items: center;
  vertical-align: middle;
  width: 28px;
  height: 28px;
  margin-left: 4px;
  padding: 0;
  border: 0;
  border-radius: 6px;
  background: rgba(8, 14, 24, 0.7);
  color: var(--dim);
  cursor: pointer;
  opacity: 0;
  transition: opacity 0.15s, color 0.15s;
}
.said p:hover .copy,
.said .copy:focus-visible {
  opacity: 1;
}
.said .copy:hover {
  color: var(--amber);
}
.said .copy svg {
  width: 14px;
  height: 14px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.transcript {
  text-align: center;
  font-size: 15.5px;
  font-weight: 500;
  line-height: 1.5;
  color: #9fd4f5;
  text-shadow: 0 0 18px rgba(64, 220, 255, 0.35);
  z-index: 4;
  /* One line (2026-11-16), the rest in the hover text. */
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
