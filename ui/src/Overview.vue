<script setup lang="ts">
/**
 * The floating overview: the orb, and nothing else.
 *
 * **There is no panel.** Three shapes sit side by side — two triangular wedges,
 * the orb, a pill — and between them the window is genuinely transparent. That
 * is what makes this float above the active windows instead of dropping a
 * rectangular thumbnail on them, and it is why the markup below has no
 * container with a background.
 *
 * Corollary, and AGENTS.md records it as an invariant: **no `backdrop-filter`
 * on a rectangular shape.** A backdrop blur is clipped exactly to the element's
 * box, so a square element draws a visible blurred square even with no border
 * and no background. Only the pill is blurred, and the pill is rounded.
 *
 * The pill shows itself only when it has something to say. An empty pill
 * floating under the orb would be an opaque block for no reason.
 */
import { computed, onMounted } from "vue";

import Orb from "./orb/Orb.vue";
import { answerPermission, connect, showMain, sleep, toggleAudio } from "./lib/bridge";
import { MODE_LABEL, TRIGGER_LABEL } from "./lib/contract";
import { store } from "./lib/store";

/** Rounded right triangle, right angle at the top left. The rounding is in the
 *  path, not faked with a thick stroke, so a thin border can sit on the real
 *  edge. */
const TRI =
  "M 9,2 L 43,2 Q 50,2 45.05,6.95 L 6.95,45.05 Q 2,50 2,43 L 2,9 Q 2,2 9,2 Z";

onMounted(connect);

/**
 * The one request the overview is showing.
 *
 * The oldest, not the newest: requests are answered in the order they blocked
 * a session, and a newer one jumping the queue would leave the first session
 * waiting behind a window that keeps changing its mind.
 */
const request = computed(() => store.pending[0] ?? null);

/**
 * High risk is announced here but **not answered here**.
 *
 * The original specification: it requires a gesture in the main window. The overview is a
 * glance, not a place to approve something irreversible.
 */
const screenOnly = computed(() => request.value?.risk === "high");

/** The last streamed answer — what ARIA is saying. */
const saying = computed(() => {
  const latest = store.sessions.at(-1);
  return latest?.message || latest?.outcome?.text || "";
});

/**
 * A short status line, and it stays empty most of the time.
 *
 * The mock-up learned this: "à l'écoute" and "clique l'orbe" under the orb
 * repeated what the orb already says by lighting up, and added a line of text
 * floating with nothing to sit on. Only what is worth reading goes here.
 */
const status = computed(() => {
  if (!store.connected) return "core injoignable";
  if (store.notice) return store.notice;
  if (store.mode === "off") return "canal vocal fermé";
  if (store.trigger === "idle_timeout") return TRIGGER_LABEL.idle_timeout;
  if (store.mode === "active") return MODE_LABEL.active;
  return "";
});

const pillHasContent = computed(
  () => !!status.value || !!saying.value || !!request.value,
);
</script>

<template>
  <div class="ov">
    <button
      class="ov-tri ov-tri-l"
      title="Ouvrir la fenêtre principale"
      aria-label="Ouvrir la fenêtre principale"
      @click="showMain"
    >
      <svg viewBox="0 0 52 52" aria-hidden="true"><path :d="TRI" /></svg>
      <span class="ic">⤢</span>
    </button>

    <button
      class="ov-tri ov-tri-r ov-tri-sleep"
      title="Mettre en veille"
      aria-label="Mettre en veille"
      @click="sleep"
    >
      <svg viewBox="0 0 52 52" aria-hidden="true"><path :d="TRI" /></svg>
      <span class="ic">⏻</span>
    </button>

    <Orb
      class="ov-orb"
      :listening="store.mode !== 'off'"
      :alive="store.mode === 'off' ? 0 : store.mode === 'active' || store.pending.length > 0 ? 1 : 0.78"
      :energy="store.level"
      :speaking="store.voice === 'speaking'"
      @toggle="toggleAudio"
    />

    <div v-if="pillHasContent" class="ov-pill" :class="{ alert: !!request }">
      <div v-if="status" class="ov-status" aria-live="polite">{{ status }}</div>

      <!-- Clamped to three lines, and only this is. -->
      <div v-if="saying && !request" class="ov-say" aria-live="polite">
        {{ saying }}
      </div>

      <!-- In its own slot, emphatically outside `.ov-say`: the clamp up there
           was taking the buttons with it. A request is read in full or not at
           all. -->
      <div v-if="request" class="ov-perm-slot">
        <div class="ov-perm">
          <div class="t">⚠ autorisation requise</div>
          <div class="a">{{ request.detail }}</div>
          <div v-if="screenOnly" class="screenonly">
            Risque élevé — se répond dans la fenêtre principale.
          </div>
          <div v-else class="r">
            <button
              class="no"
              @click="answerPermission(request.id, { decision: 'deny' })"
            >
              refuser
            </button>
            <button
              class="yes"
              @click="answerPermission(request.id, { decision: 'allow' })"
            >
              autoriser
            </button>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* The orb is smaller here than in the main window, and the wedges nest in the
   empty corners the circle leaves. `:deep` because the size belongs to this
   view, not to the orb component — the main window wants its own. */
.ov :deep(.orb) {
  width: 190px;
  height: 190px;
  z-index: 2;
  /* No drop shadow: against a transparent window it would halo the desktop. */
  filter: none;
}
</style>
