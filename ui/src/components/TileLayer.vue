<script setup lang="ts">
/**
 * The tiles a session has pushed, framed on the sandbox origin.
 *
 * A tile is a session's non-textual return value. MCP Apps (SEP-1865) carries
 * it as a `ui://` resource of type `text/html;profile=mcp-app`, and the host
 * frames it with `allow-scripts` **and** `allow-same-origin` — which is only
 * safe because the core serves it from `http://127.0.0.1:7420`, an origin
 * distinct from this window's by scheme, host and port.
 *
 * # The deviation the mock had, and that this does not
 *
 * The interface mock uses `sandbox="allow-scripts"` alone and
 * `srcdoc`, because a page opened over `file://` has no second origin to
 * offer. That is stricter than the spec and has **no postMessage channel at
 * all**. Here the second origin exists, so the attribute is the one the spec
 * asks for and the `src` is a real URL. Never `srcdoc`: a document inlined
 * here would inherit this window's origin, which is precisely the hole the
 * separate port exists to close.
 *
 * # Where the boundary is
 *
 * Not in the `sandbox` attribute — decision 10 is explicit. A tile asks for a
 * tool over JSON-RPC, this window relays the ask to Rust, and **Rust decides**.
 * Nothing is filtered on this side: the window is the tile's neighbour, not its
 * judge, and a check made in the same process the tile is talking to would be
 * no check at all.
 */
import { onBeforeUnmount, onMounted, ref, watch } from "vue";

import { closeTile, openLink, submitTileForm, tileCall } from "../lib/bridge";
import { store, type Tile } from "../lib/store";

/** Geometry, per tile, kept here because none of it is persisted (decision 11). */
interface Placed {
  x: number;
  y: number;
  w: number;
  h: number;
  z: number;
}

const MIN_W = 210;
const MIN_H = 168;

const placed = ref(new Map<string, Placed>());
const dragging = ref<string | null>(null);
const frames = new Map<string, HTMLIFrameElement>();
/**
 * Which tiles have sent `ui/notifications/initialized`.
 *
 * SEP-1865: the host **must not** send anything to a view before this arrives.
 * Kept rather than assumed, so that the day there is something to push, the
 * rule is already enforced instead of needing to be remembered.
 */
const ready = ref(new Set<string>());

let topmost = 10;

/**
 * Where a new tile lands.
 *
 * Cascaded from the top left of the layer rather than centred: two tiles
 * arriving from the same task would otherwise sit exactly on top of each other
 * and look like one.
 */
function place(uri: string): Placed {
  const existing = placed.value.get(uri);
  if (existing) return existing;
  const n = placed.value.size;
  // A form is drawn under the frame and takes real height. Opening a tile at
  // 260 px with three fields put the button below the fold, where nothing
  // said it existed — measured, not guessed.
  const tile = store.tiles.find((one) => one.uri === uri);
  const fields = tile?.form?.fields.length ?? 0;
  const ask = fields === 0 ? 0 : 42 + fields * 32;
  // A PDF at 340 px is a postage stamp: a tile carrying one opens wider and
  // taller, and stays resizable like any other.
  const docs = tile?.documents.length ?? 0;
  const spot: Placed = {
    x: 64 + (n % 4) * 28,
    y: 84 + (n % 4) * 28,
    w: docs === 0 ? 340 : 520,
    h: 260 + ask + docs * 380,
    z: ++topmost,
  };
  placed.value.set(uri, spot);
  return spot;
}

function style(tile: Tile) {
  const at = place(tile.uri);
  return {
    left: `${at.x}px`,
    top: `${at.y}px`,
    width: `${at.w}px`,
    height: `${at.h}px`,
    zIndex: at.z,
  };
}

function raise(uri: string) {
  const at = place(uri);
  at.z = ++topmost;
  placed.value = new Map(placed.value);
}

async function close(tile: Tile) {
  // The core first: it is what stops the tile being served. Removing the
  // element while the URL still answers would hide it, not close it.
  await closeTile(tile.uri);
  placed.value.delete(tile.uri);
  ready.value.delete(tile.uri);
  typed.value.delete(tile.uri);
  sending.value.delete(tile.uri);
  arrived.value.delete(tile.uri);
  missing.value.delete(tile.uri);
  frames.delete(tile.uri);
  loads.delete(tile.uri);
  store.tiles = store.tiles.filter((held) => held.uri !== tile.uri);
}

/**
 * The gesture, for the head and for the grip.
 *
 * The iframe is made **inert for the duration**, and that is not a nicety: it
 * swallows pointer events, so without this the drag stalls the moment the
 * cursor crosses the tile's own content. Learned in the mock, kept here.
 */
function gesture(
  uri: string,
  event: PointerEvent,
  onMove: (dx: number, dy: number, at: Placed) => void,
) {
  event.preventDefault();
  const handle = event.currentTarget as HTMLElement;
  const at = place(uri);
  const from = { ...at };
  const x0 = event.clientX;
  const y0 = event.clientY;
  handle.setPointerCapture(event.pointerId);
  dragging.value = uri;

  const move = (moved: PointerEvent) => {
    onMove(moved.clientX - x0, moved.clientY - y0, from);
    placed.value = new Map(placed.value);
  };
  const done = () => {
    handle.removeEventListener("pointermove", move);
    handle.removeEventListener("pointerup", done);
    handle.removeEventListener("pointercancel", done);
    dragging.value = null;
  };
  handle.addEventListener("pointermove", move);
  handle.addEventListener("pointerup", done);
  handle.addEventListener("pointercancel", done);
}

function startDrag(uri: string, event: PointerEvent) {
  raise(uri);
  gesture(uri, event, (dx, dy, from) => {
    const at = place(uri);
    // Bounded by the window, so a tile cannot be dragged out of reach.
    at.x = Math.max(0, Math.min(window.innerWidth - at.w, from.x + dx));
    at.y = Math.max(0, Math.min(window.innerHeight - at.h, from.y + dy));
  });
}

function startResize(uri: string, event: PointerEvent) {
  event.stopPropagation();
  raise(uri);
  gesture(uri, event, (dx, dy, from) => {
    const at = place(uri);
    at.w = Math.max(MIN_W, Math.min(window.innerWidth - at.x, from.w + dx));
    at.h = Math.max(MIN_H, Math.min(window.innerHeight - at.y, from.h + dy));
  });
}

function bind(uri: string, el: Element | null) {
  if (el instanceof HTMLIFrameElement && !frames.has(uri)) {
    frames.set(uri, el);
    // The clock starts when the frame exists, not when the tile is announced:
    // a tile declared while the window was busy would otherwise be given less
    // than its share of the grace.
    expect(uri);
  }
}

/** The origin a tile was pointed at, for the message when it never answers. */
function origin(tile: Tile): string {
  try {
    return new URL(tile.source).origin;
  } catch {
    return tile.source;
  }
}

/** What has been typed into each tile's form, by field name. */
const typed = ref(new Map<string, Record<string, string>>());
/** Tiles whose submission is in flight. */
const busy = ref(new Set<string>());
/** What came back from the last submission, per tile. */
const sending = ref(new Map<string, string>());

/**
 * The answers for one tile, seeded from the declared defaults.
 *
 * Seeded on first read rather than on render, because a `v-model` that writes
 * into a fresh object every tick loses a character per keystroke.
 */
function answers(uri: string): Record<string, string> {
  const held = typed.value.get(uri);
  if (held) return held;
  const tile = store.tiles.find((one) => one.uri === uri);
  const seeded: Record<string, string> = {};
  for (const field of tile?.form?.fields ?? []) {
    seeded[field.name] =
      field.kind === "toggle"
        ? field.value.trim() === "oui"
          ? "oui"
          : "non"
        : field.value;
  }
  typed.value.set(uri, seeded);
  return seeded;
}

async function submit(tile: Tile) {
  if (busy.value.has(tile.uri)) return;
  busy.value = new Set(busy.value).add(tile.uri);
  sending.value.delete(tile.uri);
  try {
    await submitTileForm(tile.uri, { ...answers(tile.uri) });
    sending.value.set(tile.uri, "envoyé");
  } catch (refusal) {
    // Shown on the tile rather than only logged: the session may have ended,
    // and "rien ne s'est passé" is the worst possible answer to a button.
    sending.value.set(tile.uri, String(refusal));
  } finally {
    const still = new Set(busy.value);
    still.delete(tile.uri);
    busy.value = still;
    sending.value = new Map(sending.value);
  }
}

/**
 * Tiles whose document actually arrived and ran.
 *
 * # Why `load` on the frame is not enough
 *
 * A cross-origin frame fires `load` for the browser's own error page exactly
 * as it does for a document, so watching for it proves nothing. The wrapper
 * the core serves posts `tile.arrived` from **inside** — a message only a
 * document that was served, parsed and run can send.
 *
 * Silence after [`ARRIVAL_GRACE`] is the other case, and the panel says so
 * rather than leaving « 127.0.0.1 a refusé de se connecter » to speak for us.
 * Measured on 2026-11-03: that page, in every tile, read as « the tiles are
 * broken » when the truth was about a port.
 */
const arrived = ref(new Set<string>());
/** Tiles that waited and never spoke. */
const missing = ref(new Set<string>());

/**
 * How long a tile may take to say it is there.
 *
 * Generous on purpose: this decides whether to show an error, and an error
 * shown over a tile that was merely slow is worse than a moment of blank.
 */
const ARRIVAL_GRACE = 4000;

function expect(uri: string) {
  window.setTimeout(() => {
    if (arrived.value.has(uri)) return;
    missing.value = new Set(missing.value).add(uri);
  }, ARRIVAL_GRACE);
}

/** How many times each frame has finished loading. */
const loads = new Map<string, number>();

/**
 * Put a tile back where it belongs if it navigated itself away.
 *
 * # The hole this closes, found on 2026-11-01
 *
 * A sandboxed frame may always navigate **itself**, whatever the sandbox
 * flags say — `allow-top-navigation` governs the top, not the frame's own
 * document. And no shipped CSP directive covers it: `connect-src` is about
 * fetches, `frame-src` about child frames. So a tile could do
 * `location.href = "https://…"` and become a live remote page inside what
 * still looks like ARIA — outside our policy, outside our design, and with
 * whatever the tile knew now sitting in someone's access log.
 *
 * It is not a privilege escalation — the session that wrote the tile already
 * has the network — but it is an integrity one, and it would make
 * `ui/open-link` pointless: why ask the host to open a link when you can
 * simply go there.
 *
 * So the second load is undone. The first is ours.
 */
function onFrameLoad(uri: string) {
  const seen = (loads.get(uri) ?? 0) + 1;
  loads.set(uri, seen);
  if (seen === 1) return;

  const frame = frames.get(uri);
  const tile = store.tiles.find((held) => held.uri === uri);
  if (!frame || !tile) return;
  // Cross-origin by then, so there is nothing to read and nothing to report
  // beyond the fact itself — which is worth saying out loud.
  console.warn(`tuile ${uri} : navigation refusée, retour au contenu servi`);
  loads.set(uri, 0);
  frame.src = tile.source;
}

// ------------------------------------------------- the JSON-RPC host half

interface Rpc {
  jsonrpc: "2.0";
  id?: number | string | null;
  method?: string;
  params?: unknown;
}

/** Which tile a message came from, by its frame's window. */
function sender(source: MessageEvent["source"]): string | null {
  for (const [uri, frame] of frames) {
    if (frame.contentWindow && frame.contentWindow === source) return uri;
  }
  return null;
}

function reply(uri: string, id: Rpc["id"], body: Record<string, unknown>) {
  const frame = frames.get(uri);
  if (!frame?.contentWindow) return;
  // Targeted at the sandbox origin rather than `"*"`: a reply is only ever for
  // the tile that asked, and the origin is known because the core built the
  // URL this frame was given.
  const origin = new URL(frame.src).origin;
  frame.contentWindow.postMessage({ jsonrpc: "2.0", id, ...body }, origin);
}

async function onMessage(event: MessageEvent) {
  // The wrapper's own note, sent by every tile the core serves. Not JSON-RPC:
  // it is a fact about this frame, not a call.
  if ((event.data as { aria?: string } | null)?.aria === "tile.arrived") {
    const from = sender(event.source);
    if (!from) return;
    arrived.value = new Set(arrived.value).add(from);
    const still = new Set(missing.value);
    still.delete(from);
    missing.value = still;
    return;
  }

  const message = event.data as Rpc | null;
  if (!message || message.jsonrpc !== "2.0" || !message.method) return;

  // Identified by **which frame sent it**, not by anything in the payload: a
  // tile could claim any URI it liked, and the window object cannot be forged.
  const uri = sender(event.source);
  if (!uri) return;

  switch (message.method) {
    case "ui/initialize":
      reply(uri, message.id, {
        result: {
          protocolVersion: "2026-01-26",
          // What this host can do, stated rather than implied. An empty
          // `tools` is the honest answer today: the visibility door is closed.
          capabilities: {},
        },
      });
      break;

    case "ui/notifications/initialized":
      ready.value.add(uri);
      break;

    case "tools/call": {
      const params = (message.params ?? {}) as {
        name?: string;
        arguments?: unknown;
      };
      try {
        const result = await tileCall(params.name ?? "", params.arguments ?? {});
        reply(uri, message.id, { result });
      } catch (refusal) {
        // The refusal text comes from Rust and is shown as it is: it names the
        // tool and says why, which is what makes a closed door debuggable.
        reply(uri, message.id, {
          error: { code: -32601, message: String(refusal) },
        });
      }
      break;
    }

    case "ui/open-link": {
      const asked = (message.params ?? {}) as { url?: string };
      try {
        await openLink(asked.url ?? "");
        reply(uri, message.id, { result: {} });
      } catch (refusal) {
        reply(uri, message.id, {
          error: { code: -32602, message: String(refusal) },
        });
      }
      break;
    }

    default:
      // Everything else the spec defines — `resources/read`, `ui/message`,
      // `ui/request-display-mode`, `ui/update-model-context` — is refused by
      // name rather than ignored.
      // Silence would look like a host that is still thinking, and a tile
      // would wait on it.
      if (message.id != null) {
        reply(uri, message.id, {
          error: {
            code: -32601,
            message: `ARIA ne gère pas encore ${message.method}.`,
          },
        });
      }
  }
}

/**
 * Reload one tile's frame when its session says its contents changed.
 *
 * The counter is what makes it fire twice for the same tile: a session that
 * rewrites a document three times sends three `tile.updated`, and a watcher
 * on the URI alone would see no change after the first.
 *
 * `loads` is reset first, deliberately. The navigation guard counts loads and
 * undoes the second one — a refresh we asked for would otherwise look exactly
 * like a tile walking off on its own.
 */
watch(
  () => store.tileRefresh.at,
  () => {
    const uri = store.tileRefresh.uri;
    const frame = frames.get(uri);
    const tile = store.tiles.find((held) => held.uri === uri);
    if (!frame || !tile) return;
    loads.set(uri, 0);
    // Reassigning the same `src` reloads in every engine; files are served
    // `no-store`, so what comes back is what is on disk now.
    frame.src = tile.source;
  },
);

onMounted(() => window.addEventListener("message", onMessage));
onBeforeUnmount(() => window.removeEventListener("message", onMessage));
</script>

<template>
  <!-- Full window and outside the stage: a tile has to be draggable over both
       menus and over the input row, not stay trapped between the panels.
       Inert to the mouse — only the tiles capture it — otherwise nothing,
       including the orb, is clickable through it. -->
  <div id="tiles" aria-label="Tuiles">
    <div
      v-for="tile in store.tiles"
      :key="tile.uri"
      class="tile ring"
      :class="{ drag: dragging === tile.uri }"
      :style="style(tile)"
      @pointerdown.capture="raise(tile.uri)"
    >
      <!-- Une poignée, pas une barre : sans séparateur, et sans titre quand la
           session n'en a pas donné (2026-11-16). Elle reste parce qu'il faut
           bien saisir la tuile quelque part — l'iframe avale les pointeurs. -->
      <div
        class="head"
        :class="{ bare: !tile.title.trim() }"
        @pointerdown="startDrag(tile.uri, $event)"
      >
        <span class="t" v-if="tile.title.trim()">{{ tile.title }}</span>
        <span class="sp"></span>
        <button
          class="x"
          type="button"
          title="Fermer"
          :aria-label="`Fermer la tuile ${tile.title}`"
          @pointerdown.stop
          @click="close(tile)"
        >
          ✕
        </button>
      </div>
      <!-- Le bac à sable n'est pas joignable : on le dit, plutôt que de
           laisser le navigateur afficher sa page « a refusé de se connecter »
           dans le panneau. Vu le 2026-11-03 : ça se lit comme « les tuiles
           sont cassées » alors que le port était simplement pris. -->
      <div v-if="missing.has(tile.uri)" class="down">
        <p class="what">Tuile injoignable</p>
        <p class="why">
          Le contenu n'est jamais arrivé depuis
          <code>{{ origin(tile) }}</code
          >.
          <template v-if="!store.tilesServed">
            Le core dit que le bac à sable <strong>n'a pas démarré</strong> —
            le port était probablement pris.
          </template>
          <template v-else>
            Le core dit pourtant l'avoir ouvert : une autre instance d'ARIA
            tourne-t-elle, qui le tiendrait sans le servir ?
          </template>
          Sous Windows le wrapper npm meurt sans ses enfants :
          <code>taskkill /IM aria.exe /F</code>, puis relance.
        </p>
      </div>

      <iframe
        v-show="!missing.has(tile.uri)"
        :ref="(el) => bind(tile.uri, el as Element | null)"
        :src="tile.source"
        :title="tile.title"
        sandbox="allow-scripts allow-same-origin"
        :style="{ pointerEvents: dragging === tile.uri ? 'none' : undefined }"
        @load="onFrameLoad(tile.uri)"
      ></iframe>
      <!-- Les PDF déclarés, cadrés par la FENÊTRE et sans `sandbox`.
           Mesuré le 2026-11-16 : Chromium refuse son visionneur PDF dans un
           cadre sandboxé (« Failed to load … as a plugin, because the frame
           … is sandboxed »), donc un <embed> dans la tuile reste blanc. L'URL
           vient du core, nomme un fichier déclaré en .pdf servi en
           application/pdf avec nosniff : ce cadre ne peut montrer qu'un PDF.
           Même geste que le formulaire déclaré, décision 69. -->
      <iframe
        v-for="doc in tile.documents"
        :key="doc"
        class="doc"
        :src="doc"
        :title="`${tile.title} — document`"
        :style="{ pointerEvents: dragging === tile.uri ? 'none' : undefined }"
      ></iframe>
      <!-- Drawn HERE and not inside the frame, and that is the whole point of
           decision 69: what the user types is in the window's document, so the
           tile's script can neither read it nor submit it. A form the tile drew
           itself could only ever report "the tile says X". -->
      <form
        v-if="tile.form"
        class="ask"
        @submit.prevent="submit(tile)"
        @pointerdown.stop
      >
        <label v-for="field in tile.form.fields" :key="field.name" class="field">
          <span class="k">{{ field.label || field.name }}</span>

          <select
            v-if="field.kind === 'choice'"
            v-model="answers(tile.uri)[field.name]"
          >
            <option v-for="choice in field.options" :key="choice" :value="choice">
              {{ choice }}
            </option>
          </select>

          <input
            v-else-if="field.kind === 'toggle'"
            type="checkbox"
            class="sw"
            :checked="answers(tile.uri)[field.name] === 'oui'"
            @change="
              answers(tile.uri)[field.name] = (
                $event.target as HTMLInputElement
              ).checked
                ? 'oui'
                : 'non'
            "
          />

          <input
            v-else
            :type="field.kind === 'number' ? 'number' : 'text'"
            v-model="answers(tile.uri)[field.name]"
          />
        </label>

        <div class="go">
          <span v-if="sending.get(tile.uri)" class="said">{{
            sending.get(tile.uri)
          }}</span>
          <button type="submit" :disabled="busy.has(tile.uri)">
            {{ busy.has(tile.uri) ? "…" : tile.form.submit.trim() || "Envoyer" }}
          </button>
        </div>
      </form>

      <div
        class="grip"
        title="Redimensionner"
        @pointerdown="startResize(tile.uri, $event)"
      ></div>
    </div>
  </div>
</template>

<style scoped>
#tiles {
  position: fixed;
  inset: 0;
  z-index: 8;
  pointer-events: none;
}

/* La tuile de la refonte du 2026-11-16 : une bordure en dégradé « RGB » —
   bleu, violet, rose, ambre — et sa lueur, d'après la capture fournie par
   l'utilisateur. Plus de barre de titre séparée ni de pied. */
.tile {
  position: absolute;
  pointer-events: auto;
  display: flex;
  flex-direction: column;
  min-width: 210px;
  min-height: 120px;
  background: rgba(6, 12, 22, 0.82);
  backdrop-filter: blur(12px);
  /* L'anneau vient de `.ring` (base.css). La lueur est la douce, pas
     `--glow` : jugée trop forte le 2026-11-16 sur une tuile, qui en porte
     davantage qu'une barre puisqu'elle est plus grande. */
  box-shadow: var(--glow-soft);
  /* Rounded, and that is a constraint rather than a taste: `backdrop-filter`
     clips to the element's box, so a square shape draws a visible square of
     blur even with no background and no border. */
  border-radius: 18px;
  overflow: hidden;
  animation: materialize 0.35s cubic-bezier(0.2, 0.9, 0.3, 1);
}

/* `.ring` sets `position: relative`; a tile is placed absolutely. */
.tile.ring {
  position: absolute;
}

/* Un anneau plus sourd, et qui vibre (2026-11-16) : le dégradé glisse
   lentement le long du bord et sa luminosité respire un peu. Assez lent pour
   ne pas tirer l'œil d'une tuile qu'on lit ; coupé par
   `prefers-reduced-motion`, comme toute animation de la fenêtre. */
.tile.ring::before {
  opacity: 0.6;
  background-size: 250% 250%;
  animation: ring-drift 7s ease-in-out infinite alternate;
}
.tile.ring:hover::before,
.tile.drag::before {
  opacity: 0.85;
}

@keyframes ring-drift {
  0% {
    background-position: 0% 0%;
    filter: brightness(0.9);
  }
  50% {
    filter: brightness(1.08);
  }
  100% {
    background-position: 100% 100%;
    filter: brightness(0.95);
  }
}

.tile.drag {
  box-shadow: var(--glow);
}

.tile .head {
  flex: 0 0 auto;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 14px 0;
  cursor: grab;
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.18em;
  text-transform: uppercase;
  color: var(--dim);
  user-select: none;
}

.tile .head:active {
  cursor: grabbing;
}

.tile .head .t {
  color: var(--chrome);
  /* Deliberately not clamped. A title that overflows is ugly; a tile whose
     controls are hidden under a clamp is broken, and that has happened once
     already — the permission block lost its buttons that way. */
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tile .head .sp {
  flex: 1;
}

/* Sans titre, la poignée n'est plus qu'une bande fine en haut. */
.tile .head.bare {
  padding-top: 4px;
  min-height: 18px;
}

/* La croix n'apparaît qu'au survol : au repos, la tuile n'est que son
   contenu. */
/* A real button since 2026-11-17, so the keyboard reaches it, and 28 px
   square like every other target. */
.tile .head .x {
  display: grid;
  place-items: center;
  width: 28px;
  height: 28px;
  padding: 0;
  border: 0;
  border-radius: 8px;
  background: transparent;
  font: inherit;
  cursor: pointer;
  color: var(--dim);
  opacity: 0;
  transition: opacity 0.15s;
}
.tile:hover .head .x,
.tile .head .x:focus-visible {
  opacity: 1;
}

.tile .head .x:hover {
  color: var(--err);
}

.tile iframe {
  flex: 1;
  width: 100%;
  border: 0;
  display: block;
  background: transparent;
  /* Un élément flex ne descend pas sous sa hauteur intrinsèque sans ça, et
     celle d'une iframe vaut 150 px par défaut : le formulaire était poussé
     hors de la tuile et coupé par `overflow: hidden`. Vu en photographiant
     une tuile de 340 × 260. */
  min-height: 0;
}

/* Le document prend la place : le contenu de la tuile n'est plus qu'une
   légende au-dessus de lui. */
.tile iframe.doc {
  flex: 4;
  background: #fff;
}

.tile .down {
  flex: 1;
  min-height: 0;
  display: flex;
  flex-direction: column;
  justify-content: center;
  gap: 7px;
  padding: 14px 16px;
  overflow-y: auto;
}

.tile .down .what {
  color: var(--err);
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.16em;
  text-transform: uppercase;
}

.tile .down .why {
  color: var(--dim);
  font-size: 12.5px;
  line-height: 1.45;
}

.tile .down code {
  font-family: var(--font-mono);
  font-size: 0.9em;
  color: var(--cyan);
}

/* La poignée de redimensionnement. Jugée trop discrète le 2026-11-16 : un
   triangle à 45 % d'opacité collé dans un coin arrondi de 18 px était à moitié
   rogné par l'arrondi lui-même. Elle est donc rentrée dans la tuile, et ce
   sont trois traits obliques — le signe que tout le monde lit comme
   « tirer ici » —, plus vifs au survol. */
/* 28 px to grab, 16 px drawn: the hit area grows, the drawing does not. */
.tile .grip {
  position: absolute;
  right: 0;
  bottom: 0;
  width: 28px;
  height: 28px;
  background-size: 16px 16px !important;
  background-repeat: no-repeat !important;
  background-position: center !important;
  z-index: 4;
  cursor: nwse-resize;
  opacity: 0.7;
  transition: opacity 0.15s;
  background: linear-gradient(
    135deg,
    transparent 0 38%,
    #ff8fc8 38% 46%,
    transparent 46% 58%,
    #ffb347 58% 66%,
    transparent 66% 78%,
    #ffb347 78% 86%,
    transparent 86%
  );
}
.tile:hover .grip {
  opacity: 1;
}

/* Le formulaire déclaré, dessiné ici et pas dans l'iframe — décision 69.
   Il vit donc dans le contexte CSS de la fenêtre, et hérite de ses jetons
   sans que `tile.css` ait son mot à dire. */
.tile .ask {
  flex: 0 0 auto;
  display: flex;
  flex-direction: column;
  gap: 7px;
  padding: 9px 10px 10px;
  background: rgba(64, 220, 255, 0.03);
}

.tile .field {
  display: flex;
  align-items: center;
  gap: 9px;
}

.tile .field .k {
  flex: 0 0 76px;
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  color: var(--dim);
  /* Jamais tronqué sans fin visible : un libellé coupé à mi-mot fait douter
     de ce qu'on remplit. */
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tile .field input[type="text"],
.tile .field input[type="number"],
.tile .field select {
  flex: 1;
  min-width: 0;
  background: rgba(1, 4, 9, 0.6);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  color: var(--text);
  font-family: var(--font-hud);
  font-size: 13px;
  padding: 4px 7px;
}

.tile .field input:focus,
.tile .field select:focus {
  outline: none;
  border-color: var(--focus);
  box-shadow: 0 0 0 1px var(--focus);
}

/* Dessinée plutôt qu'héritée : `accent-color` ne teinte que l'état coché, et
   une case décochée restait le carré gris clair du système au milieu d'une
   interface sombre. Vu en photo. */
.tile .field input[type="checkbox"] {
  appearance: none;
  width: 16px;
  height: 16px;
  margin: 0;
  background: rgba(1, 4, 9, 0.6);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  cursor: pointer;
  position: relative;
}

.tile .field input[type="checkbox"]:checked {
  background: rgba(183, 164, 255, 0.16);
  border-color: rgba(183, 164, 255, 0.5);
}

.tile .field input[type="checkbox"]:checked::after {
  content: "";
  position: absolute;
  left: 4px;
  top: 0;
  width: 5px;
  height: 10px;
  border: solid var(--chrome);
  border-width: 0 2px 2px 0;
  transform: rotate(45deg);
}

.tile .go {
  display: flex;
  align-items: center;
  gap: 9px;
  justify-content: flex-end;
}

.tile .go .said {
  flex: 1;
  min-width: 0;
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.1em;
  color: var(--dim);
  /* Une phrase de refus peut être longue — « cette session est terminée… » —
     et elle doit rester lisible en entier. */
  white-space: normal;
  line-height: 1.4;
}

.tile .go button {
  background: rgba(183, 164, 255, 0.12);
  border: 1px solid rgba(183, 164, 255, 0.4);
  border-radius: var(--r-sm);
  color: var(--chrome);
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.16em;
  text-transform: uppercase;
  padding: 5px 11px;
  cursor: pointer;
}

.tile .go button:hover:not(:disabled) {
  background: rgba(183, 164, 255, 0.2);
}

.tile .go button:disabled {
  opacity: 0.5;
  cursor: default;
}

</style>
