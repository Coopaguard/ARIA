<script setup lang="ts">
/**
 * The live sessions, and the requests blocking them.
 *
 * A **tab of the left-hand menu** since 2026-11-16, no longer a menu of its
 * own on the right — asked for by the user with the visual rework. One panel
 * instead of two leaves the stage to the orb and the tiles; and because a
 * pending request is the one thing that may not wait unseen, `App.vue` opens
 * the menu on this tab by itself when one arrives.
 *
 * Two things here are load-bearing rather than decorative:
 *
 *  - **The profile badge per session.** Epic 04: once the profile is
 *    overridable at launch, a list without it leaves nobody knowing what runs
 *    under which guard rail.
 *  - **The request sits UNDER its session.** Epic 02: with the orb closed this
 *    is the only channel left, so it has to be enough on its own — which means
 *    the buttons must be reachable. AGENTS.md records how that was lost once:
 *    the permission block already lost its buttons under a `-webkit-line-clamp`.
 *    Nothing important goes in a truncated container.
 */
import { ref } from "vue";

import {
  PROFILE_LABEL,
  RISK_LABEL,
  SESSION_LABEL,
  type PermissionRequest,
} from "../lib/contract";
import { answerPermission, stopSession } from "../lib/bridge";
import { liveSessions, pendingFor, store } from "../lib/store";

/** Which requests have their detail expanded. */
const expanded = ref(new Set<string>());

function toggleDetail(id: string) {
  const next = new Set(expanded.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  expanded.value = next;
}

/**
 * The state class the mock-up uses. `awaiting` is kept distinct from the error
 * states because a blocked session is not a broken one.
 */
function stateClass(state: string): string {
  if (state === "awaiting_permission") return "awaiting";
  if (state === "done") return "done";
  if (state === "failed") return "error";
  if (state === "unresponsive" || state === "expired") return "cancelled";
  return "";
}

async function allow(request: PermissionRequest) {
  await answerPermission(request.id, { decision: "allow" });
}

async function allowAlways(request: PermissionRequest) {
  // The scope is sent as the user was shown it, so the audit records what they
  // agreed to rather than what was inferred from it afterwards.
  await answerPermission(request.id, {
    decision: "allow_always",
    scope: request.command ?? request.tool,
  });
}

async function deny(request: PermissionRequest) {
  await answerPermission(request.id, { decision: "deny" });
}
</script>

<template>
  <div class="sessions-tab">
    <div class="sec">
      <h2>
        sessions
        <span class="n">{{ liveSessions().length }}</span>
      </h2>

      <div v-if="liveSessions().length === 0" class="muted">
        Aucune session. Demande une tâche.
      </div>

      <div
        v-for="session in liveSessions()"
        :key="session.id"
        class="task"
        :class="stateClass(session.state)"
      >
        <div class="t">{{ session.title }}</div>
        <div class="s">
          <i class="dot" />
          {{ SESSION_LABEL[session.state] }}
          <span class="prof" :data-p="session.profile">
            {{ PROFILE_LABEL[session.profile] }}
          </span>
        </div>

        <!-- Decision 8: a session that has gone quiet for three minutes is
             MARKED and offered this button. Nothing kills it on a timer —
             a long compile legitimately produces no events, and the CLI's
             own `--max-turns` and `--max-budget-usd` are the hard ceilings.
             This is the only way one is killed, and a hand has to do it. -->
        <div v-if="session.state === 'unresponsive'" class="quiet">
          <span>Sans réponse depuis trois minutes. Une compilation longue en
            produit autant — elle n'est pas arrêtée toute seule.</span>
          <button class="stop" @click="stopSession(session.id)">arrêter</button>
        </div>

        <div v-if="session.message" class="out">{{ session.message }}</div>

        <!-- Pending requests, under the session they block. They never expire,
             and closing the orb is not a refusal: this is where they wait. -->
        <div
          v-for="request in pendingFor(session.id)"
          :key="request.id"
          class="ask"
          :class="{ open: expanded.has(request.id) }"
        >
          <div class="h">
            autorisation · risque
            <span :data-r="request.risk">{{ RISK_LABEL[request.risk] }}</span>
          </div>
          <div class="w">{{ request.detail }}</div>

          <div class="r">
            <button class="ok" @click="allow(request)">autoriser</button>
            <button class="no" @click="deny(request)">refuser</button>
            <button class="det" @click="toggleDetail(request.id)">détail</button>
          </div>

          <!-- High risk is never approved in one click from a list: "always"
               is withheld, so a blanket rule needs a deliberate gesture. -->
          <div v-if="request.risk !== 'high'" class="r">
            <button class="always" @click="allowAlways(request)">
              toujours, dans ce périmètre
            </button>
          </div>

          <div class="more">
            <div><b>outil</b> {{ request.tool }}</div>
            <div v-if="request.command"><b>commande</b> {{ request.command }}</div>
          </div>
        </div>
      </div>
    </div>

    <div class="sec">
      <h2>activité</h2>
      <div v-if="store.sessions.every((s) => s.steps.length === 0)" class="muted">
        Les étapes de l'agent s'affichent ici.
      </div>
      <template v-for="session in store.sessions" :key="`steps-${session.id}`">
        <div
          v-for="(step, i) in session.steps"
          :key="`${session.id}-${i}`"
          class="step"
          :data-st="step.status"
        >
          <div class="row">
            <span class="ic">⟡</span>
            <span class="id">{{ step.label }}</span>
          </div>
          <div v-if="step.command" class="det">{{ step.command }}</div>
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.task {
  border: 1px solid var(--line);
  border-left: 3px solid var(--cyan);
  padding: 10px 12px;
  margin-bottom: 10px;
  background: linear-gradient(180deg, rgba(64, 220, 255, 0.06), transparent);
  border-radius: var(--r-sm);
  animation: materialize 0.35s ease;
}

.task .t {
  font-size: 13.5px;
  font-weight: 600;
  margin-bottom: 5px;
  letter-spacing: 0.03em;
}

.task .s {
  font-size: 11px;
  color: var(--dim);
  display: flex;
  align-items: center;
  gap: 8px;
  font-family: var(--font-mono);
}

.task .out {
  font-size: 11px;
  color: #a9cbe4;
  margin-top: 7px;
  /* Scrollable, never clamped. AGENTS.md: nothing important goes in a
     truncated container — the permission block lost its buttons that way. */
  max-height: 110px;
  overflow: auto;
  white-space: pre-wrap;
  font-family: var(--font-mono);
}

.dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--cyan);
  box-shadow: 0 0 8px var(--cyan);
  animation: pulse 1s infinite alternate;
  flex: 0 0 auto;
}

.task.done {
  border-left-color: var(--ok);
}
.task.done .dot {
  background: var(--ok);
  box-shadow: 0 0 8px var(--ok);
  animation: none;
}
.task.error {
  border-left-color: var(--err);
}
.task.error .dot {
  background: var(--err);
  box-shadow: 0 0 8px var(--err);
  animation: none;
}
.task.cancelled {
  border-left-color: var(--dim);
  opacity: 0.6;
}
.task.cancelled .dot {
  background: var(--dim);
  box-shadow: none;
  animation: none;
}
.task.awaiting {
  border-left-color: var(--err);
}
.task.awaiting .dot {
  background: var(--err);
  box-shadow: 0 0 8px var(--err);
}

.quiet {
  margin-top: 9px;
  display: flex;
  align-items: flex-start;
  gap: 9px;
  font-size: 11.5px;
  line-height: 1.35;
  color: var(--dim);
}
.quiet .stop {
  flex: 0 0 auto;
  cursor: pointer;
  border-radius: 999px;
  border: 1px solid rgba(255, 84, 112, 0.4);
  background: rgba(255, 255, 255, 0.04);
  color: #d98a9c;
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  padding: 5px 11px;
}
.quiet .stop:hover {
  color: var(--err);
  border-color: var(--err);
}

.prof {
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.14em;
  text-transform: uppercase;
  border: 1px solid var(--line);
  border-radius: 999px;
  padding: 2px 8px;
  color: var(--dim);
  margin-left: auto;
}
.prof[data-p="autonomous"] {
  color: var(--amber);
  border-color: rgba(255, 179, 71, 0.4);
}
.prof[data-p="plan"] {
  color: var(--cyan);
  border-color: rgba(64, 220, 255, 0.4);
}
.prof[data-p="strict"] {
  color: var(--err);
  border-color: rgba(255, 84, 112, 0.4);
}

.ask {
  margin-top: 9px;
  border: 1px solid rgba(255, 84, 112, 0.4);
  border-radius: var(--r-sm);
  background: rgba(255, 84, 112, 0.07);
  padding: 8px 10px;
}

.ask .h {
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.2em;
  text-transform: uppercase;
  color: var(--err);
  margin-bottom: 4px;
}
.ask .h [data-r="low"] {
  color: var(--risk-low);
}
.ask .h [data-r="medium"] {
  color: var(--risk-medium);
}
.ask .h [data-r="high"] {
  color: var(--risk-high);
}

.ask .w {
  font-size: 12.5px;
  line-height: 1.35;
  color: #e8f2fa;
}

.ask .r {
  display: flex;
  gap: 7px;
  margin-top: 8px;
}

.ask .r button {
  flex: 1;
  cursor: pointer;
  border-radius: 999px;
  border: 1px solid var(--line);
  background: rgba(255, 255, 255, 0.04);
  color: rgba(205, 225, 240, 0.9);
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  padding: 6px 4px;
  transition: 0.15s;
}
.ask .r .ok:hover {
  color: var(--ok);
  border-color: var(--ok);
  box-shadow: 0 0 12px rgba(67, 255, 164, 0.25);
}
.ask .r .no:hover {
  color: var(--err);
  border-color: var(--err);
}
.ask .r .det:hover,
.ask .r .always:hover {
  color: var(--chrome);
  border-color: var(--chrome);
}

.ask .more {
  display: none;
  margin-top: 8px;
  padding-top: 8px;
  border-top: 1px solid rgba(255, 84, 112, 0.25);
  font-family: var(--font-mono);
  font-size: 11px;
  color: #a9cbe4;
  line-height: 1.5;
  word-break: break-all;
}
.ask.open .more {
  display: block;
}
.ask .more b {
  color: var(--dim);
  font-weight: 400;
  letter-spacing: 0.12em;
  text-transform: uppercase;
  font-size: 11px;
  margin-right: 6px;
}

/* activity feed — the original specification */
.step {
  border-left: 2px solid var(--line);
  padding: 5px 0 5px 11px;
  margin-left: 3px;
  font-size: 12.5px;
}
.step .row {
  display: flex;
  align-items: baseline;
  gap: 8px;
}
.step .ic {
  font-family: var(--font-mono);
  font-size: 11px;
  width: 11px;
  flex: 0 0 auto;
  color: var(--cyan);
}
.step .id {
  font-family: var(--font-mono);
  font-size: 11.5px;
  color: var(--text);
}
.step .det {
  font-family: var(--font-mono);
  font-size: 11px;
  color: var(--dim);
  margin-top: 2px;
  word-break: break-all;
}
.step[data-st="running"] {
  border-left-color: var(--cyan);
}
.step[data-st="running"] .ic {
  animation: pulse 1s infinite alternate;
}
.step[data-st="ok"] {
  border-left-color: var(--ok);
}
.step[data-st="ok"] .ic {
  color: var(--ok);
}
.step[data-st="denied"],
.step[data-st="failed"] {
  border-left-color: var(--err);
}
.step[data-st="denied"] .ic,
.step[data-st="failed"] .ic {
  color: var(--err);
}
</style>
