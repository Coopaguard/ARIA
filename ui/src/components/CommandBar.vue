<script setup lang="ts">
/**
 * The input field, and the button that opens the left-hand menu.
 *
 * The microphone switch that sat here went on 2026-11-16, at the user's
 * request: clicking the orb does exactly the same thing, and two controls for
 * one switch is one too many to keep in agreement.
 *
 * It is part of the BACKGROUND, not a card: no border around the row, no blur,
 * just a gradient rising from the bottom so text stays readable over the
 * scene. That is the mock-up's decision and it is what keeps the stage from
 * looking like three stacked panels.
 *
 * Typing matters as much as speaking, and epic 04 says why: cutting the ear
 * must never interrupt a task, so you can hand back to the keyboard while
 * Claude works and pick the orb up afterwards.
 */
import { nextTick, onBeforeUnmount, onMounted, ref } from "vue";

const emit = defineEmits<{ send: [string]; toggleLeft: []; stop: [] }>();

const draft = ref("");
const box = ref<HTMLTextAreaElement | null>(null);
const bar = ref<HTMLElement | null>(null);

/**
 * Tell the page where the bar's top edge is, as `--bar-clear`.
 *
 * Asked for on 2026-11-16: the menu sits **above** the input and follows it
 * as it grows to five lines, instead of the input sliding under the menu. The
 * panel reads the variable for its `bottom`, so the two never overlap however
 * tall the text gets. A gap of 14 px keeps them reading as two surfaces.
 */
let watcher: ResizeObserver | null = null;
function publishClearance() {
  const el = bar.value;
  if (!el) return;
  const fromBottom = window.innerHeight - el.getBoundingClientRect().top;
  document.documentElement.style.setProperty("--bar-clear", `${fromBottom + 14}px`);
}
onMounted(() => {
  watcher = new ResizeObserver(publishClearance);
  if (bar.value) watcher.observe(bar.value);
  window.addEventListener("resize", publishClearance);
  publishClearance();
});
onBeforeUnmount(() => {
  watcher?.disconnect();
  window.removeEventListener("resize", publishClearance);
});

/** How tall the field may grow before it scrolls, in lines. */
const MAX_LINES = 5;

/**
 * Fit the field to what is typed, up to five lines.
 *
 * Asked for on 2026-11-16. `height: auto` first, or `scrollHeight` never
 * shrinks back when lines are deleted. The ceiling is read from the computed
 * line height rather than written in pixels, so a change of font size keeps
 * it at five lines.
 */
function fit() {
  const el = box.value;
  if (!el) return;
  const style = getComputedStyle(el);
  const line = parseFloat(style.lineHeight) || 21;
  const pad = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
  const ceiling = line * MAX_LINES + pad;
  el.style.height = "auto";
  el.style.height = `${Math.min(el.scrollHeight, ceiling)}px`;
  el.style.overflowY = el.scrollHeight > ceiling ? "auto" : "hidden";
}

function send() {
  const text = draft.value.trim();
  if (!text) return;
  emit("send", text);
  draft.value = "";
  void nextTick(fit);
}
</script>

<template>
  <div ref="bar" class="bar">
    <button
      class="menu ring"
      title="Menu — conversations, sessions, réglages"
      aria-label="Menu"
      @click="emit('toggleLeft')"
    >
      <span /><span /><span />
    </button>

    <!-- Wrapped, because a <textarea> is a replaced element and takes no
         ::before: the ring of the visual rework is drawn on its frame. -->
    <div class="field ring">
      <textarea
        ref="box"
        v-model="draft"
        class="input"
        rows="1"
        placeholder="Écris, ou parle."
        @input="fit"
        @keydown.enter.exact.prevent="send"
      />
    </div>

    <!-- STOP, as a tape deck's square. It closes the ear AND stops every
         delegated session — the one thing the orb deliberately does not do,
         since cutting the ear must never interrupt a task (epic 04). So it
         stays one click away and always visible; it moved here from the foot
         of the left column on 2026-11-16, in place of the key hints. -->
    <button
      class="stop ring"
      aria-label="Tout arrêter : le micro et les sessions en cours"
      data-tip="Tout arrêter — micro et sessions"
      @click="emit('stop')"
    >
      <span class="sq" />
    </button>
  </div>
</template>

<style scoped>
.bar {
  position: absolute;
  left: 30px;
  right: 30px;
  bottom: 18px;
  z-index: 6;
  display: flex;
  align-items: flex-end;
  gap: 11px;
  background: none;
  border: 0;
  padding: 0;
}

.menu {
  width: 34px;
  height: 36px;
  flex: 0 0 auto;
  border: 0;
  border-radius: 999px;
  background: rgba(8, 14, 24, 0.6);
  backdrop-filter: blur(8px);
  color: var(--dim);
  cursor: pointer;
  display: grid;
  place-items: center;
  gap: 4px;
  padding: 9px;
}
.menu span {
  display: block;
  width: 100%;
  height: 1.5px;
  background: currentColor;
  transition: 0.18s;
}
.menu:hover {
  color: var(--amber);
}
.menu:focus-visible {
  outline: 2px solid var(--focus);
  outline-offset: 2px;
}

.field {
  flex: 1;
  min-width: 0;
  display: flex;
  border-radius: var(--r-lg);
  background: rgba(8, 14, 24, 0.55);
  backdrop-filter: blur(8px);
  box-shadow: var(--glow-soft);
}
.field:focus-within {
  box-shadow:
    -8px -6px 24px -12px rgba(63, 208, 255, 0.45),
    8px 8px 26px -12px rgba(123, 108, 255, 0.45),
    0 10px 30px rgba(0, 0, 0, 0.45);
}
/* The orb's colours, quieter than the tiles' ring (2026-11-19): this row is
   always on screen, right under the orb. Scoped, so tiles are untouched. */
.field.ring::before,
.menu.ring::before {
  background: var(--ring-soft);
}
.field.ring:focus-within::before,
.menu.ring:hover::before {
  background: var(--ring-soft-on);
}
/* The keyboard's focus ring, shared with every control. On the frame rather
   than the textarea, which has no border of its own. */
.field:has(.input:focus-visible) {
  outline: 2px solid var(--focus);
  outline-offset: 2px;
}

.input {
  flex: 1;
  min-width: 0;
  resize: none;
  overflow-y: auto;
  background: transparent;
  border: 0;
  border-radius: var(--r-lg);
  color: var(--text);
  font-family: var(--font-hud);
  font-size: 15px;
  line-height: 1.4;
  letter-spacing: 0.01em;
  padding: 9px 14px;
  min-height: 38px;
  /* The height is set by `fit()`; this only stops a flash of a tall box
     before the first measurement. */
  overflow-y: hidden;
}
.input::placeholder {
  color: var(--dim);
}
.input:focus {
  outline: none;
}

.stop {
  width: 38px;
  height: 38px;
  flex: 0 0 auto;
  border: 0;
  border-radius: 50%;
  background: rgba(8, 14, 24, 0.6);
  backdrop-filter: blur(8px);
  box-shadow: 0 0 16px -4px rgba(255, 84, 112, 0.55);
  cursor: pointer;
  display: grid;
  place-items: center;
  transition: box-shadow 0.15s;
}
/* Red, not the shared gradient (2026-11-17): STOP is the one control whose
   colour has to say « danger » before it is read. The ring keeps its shape
   and width; only its paint changes. */
.stop.ring::before {
  background: var(--err);
}
.stop .sq {
  width: 12px;
  height: 12px;
  border-radius: 2px;
  background: var(--err);
  box-shadow: 0 0 6px rgba(255, 84, 112, 0.6);
  transition: box-shadow 0.15s;
}
.stop:hover {
  box-shadow: 0 0 22px -2px rgba(255, 84, 112, 0.8);
}
.stop:hover .sq {
  box-shadow: 0 0 12px rgba(255, 84, 112, 0.9);
}
.stop:focus-visible {
  outline: 2px solid var(--focus);
  outline-offset: 2px;
}

/* The overlay label. `::after`, because `::before` is the ring. Above the
   button and right-aligned to it, since the button sits at the window's
   right edge and a centred label would be cut off. */
.stop::after {
  content: attr(data-tip);
  position: absolute;
  right: 0;
  bottom: calc(100% + 10px);
  padding: 5px 10px;
  border-radius: 999px;
  background: rgba(8, 14, 24, 0.92);
  box-shadow: var(--glow-soft);
  color: var(--text);
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  white-space: nowrap;
  pointer-events: none;
  opacity: 0;
  transform: translateY(4px);
  transition: opacity 0.15s, transform 0.15s;
}
.stop:hover::after,
.stop:focus-visible::after {
  opacity: 1;
  transform: none;
}
</style>
