<script setup lang="ts">
/**
 * The status strip: what ARIA is doing, and under which profile.
 *
 * The **profile** sits here rather than in settings alone, and epic 04 says
 * why: it is overridable per session, so a screen that never shows it leaves
 * nobody knowing what runs under which guard rail.
 *
 * The **state badge is a control**, not a label. It reads what ARIA is doing
 * and, when that is a listening policy rather than an activity, it switches
 * between the two — named-only and listening to everything. That is the
 * closest a control can sit to the thing it changes, and it is where the eye
 * already goes to find out whether ARIA is listening.
 *
 * It is **not** the on/off switch: that is the orb, and only the orb since
 * the microphone button went on 2026-11-16.
 * Two controls, two scopes, and the hover label spells out what each policy
 * gives up, because the difference is a privacy guarantee (decision 19) and
 * not a convenience.
 *
 * Settings and STOP used to live here. They are at the foot of the left-hand
 * panel now, where they stay visible whether it is open or not — STOP cuts the
 * ear *and* the work, so it may never be behind a toggle.
 */
import { computed } from "vue";

import { setListening } from "../lib/bridge";
import {
  MODE_EXPLAINED,
  MODE_LABEL,
  PROFILE_LABEL,
  TRIGGER_LABEL,
  type AudioMode,
  type Profile,
} from "../lib/contract";
import { liveSessions, store } from "../lib/store";

const emit = defineEmits<{ cycleProfile: []; openSessions: [] }>();

/**
 * Delegated work still running, asked for on 2026-11-22: with the sessions
 * behind the left menu, nothing on screen said that ARIA was busy, and a
 * delegated task of two minutes read as an assistant that had stopped
 * answering. Shown only while there is something to show.
 */
const busy = computed(() => {
  const live = liveSessions();
  if (live.length === 0) return null;
  // The latest one: it is the one the user just asked for.
  const latest = live.reduce((a, b) => (b.opened_at > a.opened_at ? b : a));
  const step = [...latest.steps].reverse().find((s) => s.status === "running");
  const waiting = live.some((s) => s.state === "awaiting_permission");
  const count = live.length;
  return {
    waiting,
    text:
      count === 1
        ? waiting
          ? "1 tâche attend ton accord"
          : "1 tâche en cours"
        : waiting
          ? `${count} tâches · une attend ton accord`
          : `${count} tâches en cours`,
    detail: step ? `${latest.title} — ${step.label}` : latest.title,
    title: live
      .map((s) => `${s.title}${s.state === "awaiting_permission" ? " (attend ton accord)" : ""}`)
      .join("\n"),
  };
});

/**
 * Whether the policy can be switched.
 *
 * The ear has to be open — a policy is not a way to open it — and that is the
 * whole condition. It used to also require ARIA to be idle, on the reasoning
 * that clicking "réflexion" to change the listening mode is a control whose
 * meaning depends on the instant. That reasoning was wrong twice over: the
 * action never changes, only the word next to it; and in active mode on a
 * live microphone ARIA is **never** idle, so the one place the switch is most
 * needed was the one place it could not be clicked.
 */
const switchable = computed(() => store.connected && store.mode !== "off");

/** The policy the click would move to. */
const other = computed<AudioMode>(() =>
  store.mode === "active" ? "hybrid" : "active",
);

const explained = computed(() => {
  // Why the mode last moved — « à la demande », « deux minutes sans
  // échange »… It used to sit in the strip beside the profile, where the
  // user read it on 2026-11-16 as a control that did nothing. It is a reason,
  // not a state, so it lives in the hover text of the thing it explains.
  const why = because.value ? `\n\nDernier changement : ${because.value}.` : "";
  if (!switchable.value) return MODE_EXPLAINED[store.mode] + why;
  // The current policy is named here even when the badge is showing an
  // activity instead — "parle" tells you nothing about what ARIA is listening
  // to, and that is exactly what someone reaching for this switch wants.
  return `Écoute : ${MODE_LABEL[store.mode]}. ${
    MODE_EXPLAINED[store.mode]
  }\n\nCliquer pour passer en « ${MODE_LABEL[other.value]} » : ${
    MODE_EXPLAINED[other.value]
  }${why}`;
});

function flip() {
  if (!switchable.value) return;
  setListening(other.value === "active");
}

/** The dot's state, as the mock-up's `data-s` values name it. */
const badge = computed(() => {
  if (!store.connected) return { key: "error", text: "core injoignable" };
  if (store.pending.length > 0)
    return { key: "awaiting", text: "autorisation requise" };
  switch (store.voice) {
    case "listening":
      return { key: "listening", text: "écoute" };
    case "thinking":
      return { key: "thinking", text: "réflexion" };
    case "speaking":
      return { key: "executing", text: "parle" };
    case "interrupted":
      return { key: "idle", text: "interrompue" };
    default:
      return { key: "idle", text: MODE_LABEL[store.mode] };
  }
});

/** Why the mode last moved. Shown because "you asked" and "two minutes went
 *  by" are not the same thing to tell someone. */
const because = computed(() =>
  store.trigger ? TRIGGER_LABEL[store.trigger] : null,
);

const profileKey = computed<Profile>(() => store.profile);
</script>

<template>
  <div class="topbar">
    <span class="brand">A.R.I.A.</span>

    <!-- A button, always, so it is reachable by keyboard and announced as a
         control — disabled rather than rendered as a span when there is
         nothing to switch, which keeps the strip from reflowing. -->
    <button
      class="state ring"
      :data-s="badge.key"
      :data-live="switchable ? '1' : '0'"
      :disabled="!switchable"
      :title="explained"
      :aria-label="`Politique d'écoute — ${MODE_LABEL[store.mode]}`"
      @click="flip"
    >
      <i class="sdot" />
      {{ badge.text }}
      <span v-if="switchable" class="arrow">⇄</span>
    </button>

    <!-- Next to the listening switch, because the two answer the same
         question from either side: what ARIA may hear, and what it may then
         do about it. Reading them far apart is what left people unsure which
         of the two they had just changed. -->
    <button
      class="hudbtn profile ring"
      :data-p="profileKey"
      title="Profil de permission — ce qu'une session peut faire sans demander"
      @click="emit('cycleProfile')"
    >
      profil · {{ PROFILE_LABEL[profileKey] }}
    </button>

    <span class="spacer" />

    <button
      v-if="busy"
      class="busy ring"
      :data-waiting="busy.waiting ? '1' : '0'"
      :title="`${busy.title}\n\nCliquer pour voir les sessions`"
      @click="emit('openSessions')"
    >
      <i class="sdot" />
      <span class="count">{{ busy.text }}</span>
      <span class="detail">{{ busy.detail }}</span>
    </button>
  </div>
</template>

<style scoped>
.topbar {
  position: fixed;
  top: 18px;
  left: 30px;
  right: 30px;
  z-index: 6;
  display: flex;
  align-items: center;
  gap: 18px;
  font-family: var(--font-mono);
  font-size: 11px;
  color: var(--dim);
  letter-spacing: 0.26em;
  text-transform: uppercase;
}

.brand {
  color: var(--amber);
  text-shadow: 0 0 12px rgba(255, 179, 71, 0.75);
  white-space: nowrap;
}

.spacer {
  flex: 1;
}

/* Running delegated work. Cyan and pulsing while it works, amber when one of
   them waits on the user — the same two colours the session menu uses. */
.busy {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  max-width: 44vw;
  padding: 4px 12px;
  border: 0;
  border-radius: 999px;
  background: rgba(8, 14, 24, 0.55);
  box-shadow: var(--glow-soft);
  font: inherit;
  color: var(--cyan);
  white-space: nowrap;
  cursor: pointer;
}
.busy.ring::before {
  background: var(--ring-soft);
}
.busy.ring:hover::before {
  background: var(--ring-soft-on);
}
.busy:focus-visible {
  outline: 2px solid var(--focus);
  outline-offset: 2px;
}
.busy .sdot {
  flex: none;
  animation: pulse 1s infinite alternate;
}
.busy[data-waiting="1"] {
  color: var(--amber);
}
.busy[data-waiting="1"] .sdot {
  background: var(--amber);
  box-shadow: 0 0 8px var(--amber);
}
.busy .count {
  flex: none;
}
/* The detail may be cut: the count is what matters, and the whole list is in
   the hover text and one click away. */
.busy .detail {
  overflow: hidden;
  text-overflow: ellipsis;
  text-transform: none;
  letter-spacing: 0.04em;
  opacity: 0.7;
}

/* Pills framed by the shared ring (2026-11-16). The state used to carry its
   colour on a thick left border; it is on the dot and the text now, so the
   frame can be the same for every surface. */
.state {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 4px 12px;
  border: 0;
  border-radius: 999px;
  background: rgba(8, 14, 24, 0.55);
  box-shadow: var(--glow-soft);
  white-space: nowrap;
  font: inherit;
  color: inherit;
  cursor: default;
}

/* Switchable: it gains the affordances of a control, and only then. */
.state[data-live="1"] {
  cursor: pointer;
}
.state[data-live="1"]:hover {
  background: rgba(255, 179, 71, 0.08);
  box-shadow: var(--glow);
}

.profile {
  border: 0;
  padding: 4px 12px;
  background: rgba(8, 14, 24, 0.55);
  box-shadow: var(--glow-soft);
}
/* The orb's colours, quieter than the tiles' ring (2026-11-19) — the same
   frame as the input row, brighter under the pointer. */
.state.ring::before,
.profile.ring::before {
  background: var(--ring-soft);
}
.state[data-live="1"]:hover::before,
.profile.ring:hover::before {
  background: var(--ring-soft-on);
}
.state[data-live="1"]:focus-visible {
  outline: 2px solid var(--focus);
  outline-offset: 2px;
}
.state .arrow {
  font-size: 11px;
  opacity: 0.55;
}

.sdot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: var(--cyan);
  box-shadow: 0 0 8px var(--cyan);
}

.state[data-s="idle"] {
  border-left-color: var(--dim);
  color: var(--dim);
}
.state[data-s="idle"] .sdot {
  background: var(--dim);
  box-shadow: none;
}
.state[data-s="listening"],
.state[data-s="thinking"] {
  border-left-color: var(--cyan);
  color: var(--cyan);
}
.state[data-s="listening"] .sdot,
.state[data-s="thinking"] .sdot,
.state[data-s="executing"] .sdot {
  animation: pulse 1s infinite alternate;
}
.state[data-s="executing"] {
  border-left-color: var(--amber);
  color: var(--amber);
}
.state[data-s="executing"] .sdot {
  background: var(--amber);
  box-shadow: 0 0 8px var(--amber);
}
.state[data-s="awaiting"] {
  border-left-color: var(--amber);
  color: var(--amber);
}
.state[data-s="awaiting"] .sdot {
  background: var(--amber);
  box-shadow: 0 0 8px var(--amber);
  animation: pulse 0.5s infinite alternate;
}
.state[data-s="error"] {
  border-left-color: var(--err);
  color: var(--err);
}
.state[data-s="error"] .sdot {
  background: var(--err);
  box-shadow: 0 0 8px var(--err);
}

.profile[data-p="strict"] {
  color: var(--err);
  border-color: rgba(255, 84, 112, 0.4);
}
.profile[data-p="normal"] {
  color: var(--cyan);
  border-color: rgba(64, 220, 255, 0.4);
}
.profile[data-p="autonomous"] {
  color: var(--amber);
  border-color: rgba(255, 179, 71, 0.4);
}

.stop {
  border-color: rgba(255, 84, 112, 0.35);
  color: #d98a9c;
}
.stop:hover {
  color: var(--err);
  border-color: var(--err);
  background: rgba(255, 84, 112, 0.08);
}
</style>
