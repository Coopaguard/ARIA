<script setup lang="ts">
/**
 * The orb, mounted.
 *
 * This component owns nothing but the canvas and the animation's lifetime. The
 * drawing is `orb.js`, untouched — one implementation, shared with the mock-up,
 * because it was written three times before the views drifted apart.
 *
 * Two invariants from AGENTS.md are honoured here and are easy to break by
 * accident:
 *
 *  - **Nothing painted may reach the edge of the canvas.** A gradient cut by
 *    the edge draws the canvas rectangle, which is what made people "sense a
 *    square" behind an orb that was genuinely transparent. The halo is bounded
 *    to the inscribed circle — inside `orb.js`, which is why the canvas must
 *    stay square. Hence the `width`/`height` below being the same expression.
 *  - **No `backdrop-filter` here.** The canvas is a square box; a blur would
 *    be clipped to it and draw a blurred square.
 */
import { onBeforeUnmount, onMounted, ref, useTemplateRef, watch } from "vue";

import { ARIAOrb } from "./orb.js";
import type { OrbHandle } from "./orb.js";

const props = defineProps<{
  /** Is the voice channel open? Drives the resting level. */
  listening: boolean;
  /**
   * How bright, 0 to 1. Multiplies nearly every stroke in `orb.js`.
   *
   * A **number**, not a flag, and it always was one on the drawing side —
   * `orb.js` multiplies by it. Passing a boolean gave the orb two settings,
   * black or full, so an open microphone waiting for a word looked exactly
   * like a closed one. Listening is not idling, and the orb is the only thing
   * on screen that says so.
   */
  alive: number;
  /** Raw voice activity, 0..1. The orb maps it into the level band itself. */
  energy?: number;
  /**
   * A model is being read into memory.
   *
   * The one stretch where ARIA is on screen and cannot do its job. Without a
   * mark of its own the orb looks exactly as it does when idle, so the first
   * thing a new user does is speak into a chain that is not there yet — and
   * conclude it does not work.
   */
  loading?: boolean;
  /**
   * Which step of the load, and how many there are.
   *
   * Two numbers rather than a percentage, because a percentage is not known:
   * see `audio::Loading` in the core. Both default to one step so a caller
   * that passes nothing still gets a sensible bar.
   */
  loadDone?: number;
  loadTotal?: number;
  /**
   * What is still being read, in French, for the label.
   *
   * Passed in rather than looked up here: the label tables live in
   * `contract.ts`, and this component holds no French.
   */
  loadWhat?: string;
  /**
   * ARIA is talking. The orb turns amber for it and is aurora otherwise.
   *
   * Tried on 2026-11-17 with the visual rework: the frames are a blue-violet-
   * pink gradient, and an orb that is amber all the time was the one thing on
   * screen outside that family. Keeping amber for speech makes the colour say
   * **who has the floor** — the same split the transcript already makes,
   * cyan for what was heard and amber for what ARIA says.
   */
  speaking?: boolean;
}>();

const emit = defineEmits<{ toggle: [] }>();

const canvas = useTemplateRef<HTMLCanvasElement>("canvas");
let orb: OrbHandle | null = null;

/**
 * How full the loading bar is, 0 to 1.
 *
 * **One segment per model, eased against the clock inside it.** The earlier
 * version of this comment rejected "step one of two" on the grounds that it
 * would jump to half in a blink and then sit there — and that was right about
 * the case it was written for, which was the *inside* of the ASR load: an 8 MB
 * file and a 2.4 GB one, where the first step costs nothing.
 *
 * It stopped being right when a second engine arrived. The two models are now
 * read by two different threads, they take comparable times, and Kyutai's can
 * take far longer on a first run because it downloads first. Here a finished
 * step is a real half of a real wait, so the floor it sets is earned rather
 * than conjured.
 *
 * Inside its segment it still **never reaches the segment's end on its own**:
 * it stops 4 % short and only completes when the model actually arrives. A bar
 * that fills and then waits is a bar that has lied; one that slows down as it
 * approaches is simply a bar that does not know. What it cannot do by shape
 * alone is say *how long is left*, so the label beside it names what is still
 * being read — which is the honest answer, and the useful one when the thing
 * it is waiting for is a download.
 */
const BUDGET_MS = 3800;
/** 2πr for r = 47, in the SVG's own units. */
const CIRCUMFERENCE = 2 * Math.PI * 47;
const progress = ref(0);
let started = 0;
let ticking: number | null = null;

/** The step the bar is in, as a fraction of the whole. */
const segment = () => {
  const total = Math.max(1, props.loadTotal ?? 1);
  const done = Math.min(props.loadDone ?? 0, total);
  return { floor: done / total, span: 1 / total };
};

function tick() {
  const elapsed = performance.now() - started;
  const { floor, span } = segment();
  // Asymptotic: half the remaining distance per budget, capped short of the
  // segment's end.
  const within = Math.min(0.96, 1 - Math.pow(0.5, elapsed / (BUDGET_MS / 2)));
  progress.value = floor + span * within;
  ticking = requestAnimationFrame(tick);
}

watch(
  // The step counts too, not only the flag: a model finishing restarts the
  // clock, so the next segment eases from where the last one ended instead of
  // continuing an easing that has already flattened out.
  () => [props.loading, props.loadDone] as const,
  ([loading]) => {
    if (loading) {
      started = performance.now();
      progress.value = segment().floor;
      if (ticking === null) ticking = requestAnimationFrame(tick);
      return;
    }
    if (ticking !== null) {
      cancelAnimationFrame(ticking);
      ticking = null;
    }
    // Completed because it completed, which is the one moment the bar is
    // telling the truth about the end.
    progress.value = 1;
  },
  { immediate: true },
);
onBeforeUnmount(() => {
  if (ticking !== null) cancelAnimationFrame(ticking);
});

onMounted(() => {
  if (!canvas.value) return;
  orb = ARIAOrb(canvas.value, {
    palette: props.speaking ? ARIAOrb.PALETTES.amber : ARIAOrb.PALETTES.aurora,
    // Read through closures rather than passed as values: `orb.js` pulls these
    // every frame, so reactivity happens for free and no watcher is needed to
    // push an energy level into it.
    energy: () => props.energy ?? 0,
    alive: () => props.alive,
    listening: () => props.listening,
    speaking: () => props.speaking ?? false,
  });
});

// The canvas is sized in viewport units, so a resized window needs the
// backing store resized with it — otherwise the orb renders blurry at the new
// size and nothing says why.
const resize = () => orb?.resize();
onMounted(() => window.addEventListener("resize", resize));
onBeforeUnmount(() => {
  window.removeEventListener("resize", resize);
  orb?.stop();
  orb = null;
});

/**
 * Cross-fade between the two palettes rather than snapping.
 *
 * A turn of speech starts and stops several times a minute; a hard switch
 * would flash the whole orb each time. 450 ms, eased, on every colour of the
 * palette at once.
 */
type Rgb = [number, number, number];
type Pal = (typeof ARIAOrb.PALETTES)["amber"];
let shown: Pal = props.speaking ? ARIAOrb.PALETTES.amber : ARIAOrb.PALETTES.aurora;
let fading: number | null = null;
const FADE_MS = 450;

function mix(a: Rgb, b: Rgb, t: number): Rgb {
  return [0, 1, 2].map((i) => Math.round(a[i] + (b[i] - a[i]) * t)) as Rgb;
}

function fadeTo(target: Pal) {
  if (fading !== null) cancelAnimationFrame(fading);
  const from = shown;
  const start = performance.now();
  const step = () => {
    const raw = Math.min(1, (performance.now() - start) / FADE_MS);
    const t = raw * raw * (3 - 2 * raw);
    shown = {
      name: target.name,
      primary: mix(from.primary, target.primary, t),
      arc1: mix(from.arc1, target.arc1, t),
      arc2: mix(from.arc2, target.arc2, t),
      iris: mix(from.iris, target.iris, t),
      glass: mix(from.glass, target.glass, t),
      accent: mix(from.accent, target.accent, t),
    };
    orb?.setPalette(shown);
    fading = raw < 1 ? requestAnimationFrame(step) : null;
  };
  fading = requestAnimationFrame(step);
}

watch(
  () => props.speaking,
  (speaking) => fadeTo(speaking ? ARIAOrb.PALETTES.amber : ARIAOrb.PALETTES.aurora),
);
onBeforeUnmount(() => {
  if (fading !== null) cancelAnimationFrame(fading);
});

// Nothing to do on prop changes — the closures above are read per frame. The
// watcher exists only to resize when the orb becomes visible again, where a
// zero-sized box would otherwise leave the backing store at 1×1.
watch(
  () => props.alive,
  () => orb?.resize(),
);
</script>

<template>
  <div class="shell">
    <canvas
      ref="canvas"
      class="orb"
      role="button"
      tabindex="0"
      aria-label="Basculer la session vocale"
      @click="emit('toggle')"
      @keydown.enter.prevent="emit('toggle')"
      @keydown.space.prevent="emit('toggle')"
    />
    <!-- Model loading. Under the orb rather than painted on it: nothing
         painted may reach the edge of that canvas — a gradient cut by the
         edge draws the canvas rectangle — so anything that wants an edge
         belongs outside it, in CSS. -->
    <div
      v-if="loading"
      class="loadring"
      role="progressbar"
      aria-label="Chargement des modèles audio"
      :aria-valuenow="Math.round(progress * 100)"
      aria-valuemin="0"
      aria-valuemax="100"
    >
      <svg viewBox="0 0 100 100">
        <circle class="track" cx="50" cy="50" r="47" />
        <!-- The arc's length *is* the progress. Rotated so it starts at the
             top, because a ring that fills from three o'clock reads as
             decoration rather than as a measure. -->
        <circle
          class="fill"
          cx="50"
          cy="50"
          r="47"
          :stroke-dasharray="`${progress * CIRCUMFERENCE} ${CIRCUMFERENCE}`"
        />
      </svg>
    </div>
    <!-- The label says *what* is still being read, not only how far along.
         The shape of the ring cannot say how long is left — a download has no
         length to show — so the words carry what the arc cannot. -->
    <div v-if="loading" class="loadlabel" role="status">
      chargement · {{ loadWhat ?? "des modèles" }} ·
      {{ Math.round(progress * 100) }}&nbsp;%
    </div>
  </div>
</template>

<style scoped>
.orb {
  /* Square, and it has to stay square: the halo is bounded to the inscribed
     circle, so a rectangle would put the bound in the wrong place. */
  width: min(42vh, 380px);
  height: min(42vh, 380px);
  cursor: pointer;
  display: block;
  filter: drop-shadow(0 0 35px rgba(64, 220, 255, 0.25));
}

.orb:focus-visible {
  outline: 2px solid var(--focus);
  outline-offset: 8px;
}

.shell {
  position: relative;
  display: inline-block;
  line-height: 0;
}

/* The loading bar. Inert to the pointer: the orb above stays clickable,
   because cutting the ear while the models load is legitimate — and it is the
   moment someone is most likely to want to. */
/* A ring around the orb, in the primary amber. Its arc length is the
   progress, so it is a measure and a shape at once. */
.loadring {
  position: absolute;
  inset: -16px;
  pointer-events: none;
}
.loadring svg {
  width: 100%;
  height: 100%;
  /* Start at twelve o'clock. A ring that fills from three reads as
     decoration rather than as a measure. */
  transform: rotate(-90deg);
}
.loadring circle {
  fill: none;
  stroke-width: 1.4;
  stroke-linecap: round;
}
.loadring .track {
  stroke: rgba(255, 179, 71, 0.12);
}
.loadring .fill {
  stroke: var(--amber);
  filter: drop-shadow(0 0 5px rgba(255, 179, 71, 0.6));
  /* Eased rather than stepped, so the frame-by-frame advance reads as motion
     instead of as a counter. */
  transition: stroke-dasharray 0.12s linear;
}

.loadlabel {
  position: absolute;
  left: 50%;
  bottom: -42px;
  transform: translateX(-50%);
  white-space: nowrap;
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.14em;
  color: var(--amber);
  line-height: 1.4;
  opacity: 0.8;
  pointer-events: none;
}

/* Someone who asked for less motion still gets the bar — it is information,
   not decoration — but without the easing that animates every frame. */
@media (prefers-reduced-motion: reduce) {
  .loadring .fill {
    transition: none;
  }
}
</style>
