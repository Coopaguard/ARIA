<script setup lang="ts">
/**
 * The left-hand menu: the audio conversations, past and current.
 *
 * Not the same list as the right-hand menu, and the split is epic 04's: on the
 * left, conversations you reopen and resume; on the right, the Claude sessions
 * running right now.
 *
 * It has held real threads since 2026-11-13. A conversation is an audio thread
 * plus the project sessions under it, both saved in `aria.db`, and clicking one
 * reopens it: the core makes it the live thread, the main agent's **process is
 * replaced**, and the thread is replayed into the new session as context.
 *
 * The process part is not an implementation detail. The CLI keeps its own
 * history for the life of its process, so replaying a thread into the session
 * that was just holding another one leaves both in its context — and the agent
 * then answers, with every appearance of confidence, out of the conversation
 * the user walked away from.
 *
 * Reopening relaunches **no project session**: epic 05 says the main agent
 * relaunches what it needs, when it needs it. An expired one stays in the list,
 * marked, because what is lost is the ability to continue, not the memory of
 * what was done.
 *
 * # The sessions tab
 *
 * Since 2026-11-16 the live sessions are a tab here rather than a menu on the
 * right. The active tab is **owned by `App.vue`** (`v-model:tab`), because the
 * window has to be able to switch it by itself: a permission request opens
 * this menu on the sessions tab, since it blocks a session until someone
 * answers and must not wait behind a closed panel.
 *
 * # The scenarios tab, and why it lives here
 *
 * In a debug build there is a **Scénarios** tab. Each one drives the real
 * core — real events, real sequence numbers, real replay — and the only
 * fiction is that no subprocess runs. A list needs room, and this panel is
 * the room — **the only one** since 2026-11-03, when the shortcut button next
 * to the orb went away. It does not exist in a release build, because the
 * command behind it does not either.
 */
import { computed, onMounted, ref, watch } from "vue";

import {
  conversationSessions,
  conversations,
  forgetConversation,
  newConversation,
  openConversation,
  playScenario,
  scenarios,
  type Scenario,
} from "../lib/bridge";
import type { Conversation, ProjectSession } from "../lib/contract";
import { liveSessions, store } from "../lib/store";
import SessionPanel from "./SessionPanel.vue";
import Settings from "./Settings.vue";

/**
 * Four tabs. `settings` joined on 2026-11-16 with the visual rework: the
 * settings dialog and the button that opened it are gone, and the panel
 * widens while that tab shows, because its rows put a label beside a control.
 */
type LeftTab = "threads" | "sessions" | "settings" | "scenarios";

/**
 * Whether the scenario tab is offered **at all**.
 *
 * Read here rather than in the template: Vue's template compiler parses
 * expressions without module syntax, so `import.meta` there is a build error.
 *
 * It mirrors the Rust side's `#[cfg(debug_assertions)]`: `lib.rs` builds two
 * `generate_handler!` lists, and `scenarios` / `play_scenario` are only in
 * the debug one, so in a release build the invoke rejects with an unknown
 * command. `bridge.ts` already turns that into an empty list, so this flag
 * is not what keeps the window from breaking — it is what keeps it from
 * **offering** a tab that could only ever be empty.
 */
const isDev = import.meta.env.DEV;
const tab = defineModel<LeftTab>("tab", { default: "threads" });
const list = ref<Scenario[]>([]);
/** The one being played, so a second click does not stack two of them. */
const playing = ref<string | null>(null);

const threads = ref<Conversation[]>([]);
const current = ref<string | null>(null);
/** `false` when `aria.db` would not open — said, not merely logged. */
const saving = ref(true);
/** The one expanded, with its project sessions. */
const open = ref<string | null>(null);
const sessions = ref<ProjectSession[]>([]);
/** Set while a reopen is in flight: it restarts a process, and takes a moment. */
const switching = ref(false);

async function refresh() {
  const answer = await conversations();
  threads.value = answer.threads;
  current.value = answer.current;
  saving.value = answer.saving;
}

onMounted(async () => {
  if (isDev) list.value = await scenarios();
  await refresh();
});

/**
 * The menu follows the core rather than its own clicks.
 *
 * `conversation.opened` is published whenever the live thread changes — here,
 * or at startup, or from another window. Folding the event instead of trusting
 * the click is the same rule the rest of the window follows: the core
 * publishes, the window draws.
 */
watch(
  () => store.conversation,
  () => {
    void refresh();
  },
);

/** The turn count also moves as ARIA talks, so the list is re-read then. */
watch(
  () => store.spoken.length,
  () => {
    void refresh();
  },
);

async function reopen(one: Conversation) {
  if (switching.value || one.id === current.value) return;
  switching.value = true;
  try {
    await openConversation(one.id);
    await refresh();
  } finally {
    switching.value = false;
  }
}

async function start() {
  if (switching.value) return;
  switching.value = true;
  try {
    await newConversation();
    await refresh();
  } finally {
    switching.value = false;
  }
}

async function expand(one: Conversation) {
  if (open.value === one.id) {
    open.value = null;
    return;
  }
  open.value = one.id;
  sessions.value = await conversationSessions(one.id);
}

async function forget(one: Conversation) {
  await forgetConversation(one.id);
  if (open.value === one.id) open.value = null;
  await refresh();
}

/** `2026-11-13T14:32:05+01:00` → `aujourd'hui 14:32`, `hier`, or a date. */
function when(iso: string): string {
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) return "";
  const time = at.toLocaleTimeString("fr-FR", {
    hour: "2-digit",
    minute: "2-digit",
  });
  const days = Math.round(
    (startOfDay(new Date()).getTime() - startOfDay(at).getTime()) / 86_400_000,
  );
  if (days === 0) return time;
  if (days === 1) return `hier ${time}`;
  if (days < 7) return `il y a ${days} jours`;
  return at.toLocaleDateString("fr-FR", { day: "numeric", month: "short" });
}

function startOfDay(at: Date): Date {
  return new Date(at.getFullYear(), at.getMonth(), at.getDate());
}

const count = computed(() => threads.value.length);

async function play(one: Scenario) {
  if (playing.value) return;
  playing.value = one.id;
  try {
    await playScenario(one.id);
  } finally {
    playing.value = null;
  }
}
</script>

<template>
  <aside class="panel left ring" :data-tab="tab">
    <!-- Icons since 2026-11-16: four words did not fit in 340 px and
         « sessions 1 » wrapped onto two lines. The word is still there, as the
         hover label and for a screen reader. -->
    <div class="tabs" role="tablist">
      <button
        role="tab"
        :aria-selected="tab === 'threads'"
        :class="{ on: tab === 'threads' }"
        :title="`Conversations (${count})`"
        aria-label="Conversations"
        @click="tab = 'threads'"
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M4 5h16v11H9l-5 4z" />
        </svg>
      </button>
      <button
        role="tab"
        :aria-selected="tab === 'sessions'"
        :class="{ on: tab === 'sessions', asking: store.pending.length > 0 }"
        title="Sessions"
        aria-label="Sessions"
        @click="tab = 'sessions'"
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M12 3l9 5-9 5-9-5z" />
          <path d="M3 13l9 5 9-5" />
        </svg>
        <span class="badge" v-if="liveSessions().length">{{
          liveSessions().length
        }}</span>
      </button>
      <button
        role="tab"
        :aria-selected="tab === 'settings'"
        :class="{ on: tab === 'settings' }"
        title="Réglages"
        aria-label="Réglages"
        @click="tab = 'settings'"
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <circle cx="12" cy="12" r="3" />
          <path
            d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"
          />
        </svg>
      </button>
      <button
        v-if="isDev"
        role="tab"
        :aria-selected="tab === 'scenarios'"
        :class="{ on: tab === 'scenarios' }"
        title="Scénarios de test"
        aria-label="Scénarios de test"
        @click="tab = 'scenarios'"
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M9 3h6M10 3v6l-5 9a2 2 0 0 0 2 3h10a2 2 0 0 0 2-3l-5-9V3" />
        </svg>
      </button>
    </div>

    <div class="scroll">
    <SessionPanel v-if="tab === 'sessions'" />

    <Settings v-else-if="tab === 'settings'" />

    <div class="sec" v-else-if="tab === 'threads'">
      <button class="fresh" :disabled="switching || !saving" @click="start">
        + nouvel échange
      </button>

      <div class="muted" v-if="!saving">
        L'historique n'a pas pu s'ouvrir : cette conversation ne sera pas
        retrouvée au prochain démarrage. Le journal de l'application dit
        pourquoi.
      </div>
      <div class="muted" v-else-if="!threads.length">
        Rien encore. Le premier mot ouvre le fil.
      </div>

      <div
        v-for="one in threads"
        :key="one.id"
        class="thread"
        :class="{ live: one.id === current, expanded: open === one.id }"
      >
        <button
          class="head"
          :disabled="switching"
          @click="reopen(one)"
          :title="one.id === current ? 'conversation en cours' : 'reprendre'"
        >
          <span class="t">{{ one.title || "sans titre" }}</span>
          <span class="meta">
            <span>{{ when(one.last_used_at) }}</span>
            <span class="dot">·</span>
            <span>{{ one.turns }} tours</span>
            <span class="dot" v-if="one.id === current">·</span>
            <span class="here" v-if="one.id === current">en cours</span>
          </span>
        </button>
        <div class="acts">
          <button class="mini" @click="expand(one)">
            {{ open === one.id ? "masquer" : "sessions" }}
          </button>
          <button class="mini danger" @click="forget(one)">oublier</button>
        </div>
        <ul class="sessions" v-if="open === one.id">
          <li class="muted small" v-if="!sessions.length">
            Aucune session de projet. Les tâches jetables ne laissent rien.
          </li>
          <li v-for="s in sessions" :key="s.id" :class="{ gone: s.expired }">
            <span class="p">{{ s.project }}</span>
            <span class="d">{{ s.title }}</span>
            <span class="x" v-if="s.expired">plus reprenable</span>
          </li>
        </ul>
      </div>
    </div>

    <div class="sec" v-else>
      <div class="muted small">
        Chacun traverse le vrai core : vrais événements, vraie séquence, vrai
        rejeu. Seule manque la CLI en sous-processus.
      </div>
      <button
        v-for="one in list"
        :key="one.id"
        class="scenario"
        :disabled="playing !== null"
        @click="play(one)"
      >
        <span class="t">{{ one.label }}</span>
        <span class="a">{{ playing === one.id ? "en cours…" : one.about }}</span>
      </button>
    </div>
    </div>
  </aside>
</template>

<style scoped>
.left {
  left: 14px;
  /* Wider since it holds the sessions too: a permission card's three buttons
     need the room, and they may never be squeezed out of reach. */
  width: 340px;
  transition: width 0.18s ease;
}

/* Settings put a label beside its control on every row: 340 px squeezes the
   controls under their labels. */
.left[data-tab="settings"] {
  width: min(560px, calc(100vw - 28px));
}

.left .sec {
  padding: 16px 14px 4px;
}

.tabs {
  flex: 0 0 auto;
  display: flex;
  gap: 2px;
  padding: 12px 12px 0;
}

.tabs button {
  flex: 1;
  position: relative;
  display: grid;
  place-items: center;
  background: transparent;
  border: 0;
  border-bottom: 1px solid var(--line);
  color: var(--dim);
  padding: 8px 4px 7px;
  cursor: pointer;
  transition: color 0.15s;
}

.tabs svg {
  width: 18px;
  height: 18px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.6;
  stroke-linecap: round;
  stroke-linejoin: round;
}

.tabs button:hover {
  color: var(--text);
}

.tabs button.on {
  color: var(--chrome);
  border-bottom-color: rgba(183, 164, 255, 0.5);
}

/* A pill on the icon's shoulder rather than a word beside it. */
.tabs .badge {
  position: absolute;
  top: 3px;
  left: calc(50% + 6px);
  min-width: 15px;
  height: 15px;
  padding: 0 4px;
  border-radius: 999px;
  background: rgba(64, 220, 255, 0.18);
  color: var(--cyan);
  font-family: var(--font-mono);
  font-size: 11px;
  line-height: 15px;
  text-align: center;
}

/* A request is waiting under a session: the tab says so even when another one
   is open, because nothing else on screen does. */
.tabs button.asking {
  color: var(--err);
  border-bottom-color: rgba(255, 84, 112, 0.6);
}
.tabs button.asking .badge {
  background: rgba(255, 84, 112, 0.22);
  color: var(--err);
  animation: pulse 1s infinite alternate;
}

.muted.small {
  font-size: 11.5px;
  margin-bottom: 10px;
}

.fresh {
  width: 100%;
  background: transparent;
  border: 1px dashed var(--line);
  border-radius: var(--r-sm);
  color: var(--dim);
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.1em;
  padding: 7px 4px;
  margin-bottom: 10px;
  cursor: pointer;
}

.fresh:hover:not(:disabled) {
  border-color: rgba(183, 164, 255, 0.45);
  color: var(--chrome);
}

.fresh:disabled {
  opacity: 0.45;
  cursor: default;
}

.thread {
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  background: rgba(8, 14, 24, 0.5);
  margin-bottom: 7px;
  overflow: hidden;
}

.thread.live {
  border-color: rgba(183, 164, 255, 0.45);
}

.thread .head {
  display: flex;
  flex-direction: column;
  gap: 3px;
  width: 100%;
  text-align: left;
  background: transparent;
  border: 0;
  padding: 8px 10px 6px;
  cursor: pointer;
}

.thread .head:disabled {
  opacity: 0.55;
  cursor: default;
}

.thread .t {
  color: var(--chrome);
  font-family: var(--font-hud);
  font-size: 13.5px;
  line-height: 1.25;
  /* Never a clamp on anything that matters: the buttons below are outside it,
     deliberately — the prototype already lost a permission block that way. */
  overflow-wrap: anywhere;
}

.thread .meta {
  display: flex;
  gap: 5px;
  color: var(--dim);
  font-family: var(--font-mono);
  font-size: 11px;
}

.thread .here {
  color: var(--chrome);
}

.thread .dot {
  opacity: 0.5;
}

/* Shown on hover only (2026-11-16): two buttons on every card doubled its
   height for something used rarely. Kept while the card is expanded or holds
   the keyboard focus, so a keyboard user can still reach them. */
.acts {
  display: none;
  gap: 4px;
  padding: 0 10px 8px;
}
.thread:hover .acts,
.thread:focus-within .acts,
.thread.expanded .acts {
  display: flex;
}

.mini {
  background: transparent;
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  color: var(--dim);
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.1em;
  padding: 3px 7px;
  cursor: pointer;
}

.mini:hover {
  color: var(--chrome);
  border-color: rgba(183, 164, 255, 0.4);
}

.mini.danger:hover {
  color: #ff8d7a;
  border-color: rgba(255, 141, 122, 0.45);
}

.sessions {
  list-style: none;
  margin: 0;
  padding: 0 10px 8px;
  border-top: 1px solid var(--line);
}

.sessions li {
  display: flex;
  flex-wrap: wrap;
  gap: 5px;
  align-items: baseline;
  padding: 6px 0 0;
  font-size: 11.5px;
}

.sessions .p {
  color: var(--chrome);
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.08em;
}

.sessions .d {
  color: var(--dim);
}

.sessions .x {
  color: #ff8d7a;
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.08em;
}

.sessions li.gone .d {
  opacity: 0.6;
}

.scenario {
  display: flex;
  flex-direction: column;
  gap: 3px;
  width: 100%;
  text-align: left;
  background: rgba(8, 14, 24, 0.5);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  padding: 8px 10px;
  margin-bottom: 7px;
  cursor: pointer;
}

.scenario:hover:not(:disabled) {
  border-color: rgba(183, 164, 255, 0.4);
}

.scenario:disabled {
  opacity: 0.55;
  cursor: default;
}

.scenario .t {
  color: var(--chrome);
  font-family: var(--font-hud);
  font-size: 14px;
}

.scenario .a {
  color: var(--dim);
  font-size: 11px;
  line-height: 1.35;
}
</style>
