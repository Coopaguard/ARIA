<script setup lang="ts">
/**
 * The hooks tab: what watches on the user's behalf, and what it last found.
 *
 * Read from the core, never kept: `hooks.changed` bumps a counter and the list
 * is read again, because the folder is the truth and the user edits it. That
 * includes a trial in progress: the run waiting for a verdict is on the row.
 *
 * A trial is the validation (2026-11-28): one run, the user says whether it is
 * what they expected, then the next run, judged the same way. Two « c'est bon »
 * and the hook runs on its own.
 *
 * A hook **modified since it was validated** stands out until it is dealt
 * with: it is suspended, and the only way back is a new trial.
 */
import { computed, onMounted, ref, watch } from "vue";

import {
  hookAskSecret,
  hookDelete,
  hookSetEnabled,
  hookTrialAnswer,
  hookTrialStep,
  hooksList,
} from "../lib/bridge";
import type { HookResult, HookRow, HookStatus } from "../lib/contract";
import { store } from "../lib/store";
import SecretPrompt from "./SecretPrompt.vue";

const rows = ref<HookRow[]>([]);
const open = ref<string | null>(null);
/** The hook whose run is in progress. */
const running = ref<string | null>(null);
const problem = ref<string | null>(null);
/** What the last trial ended on, per hook, until the next one starts. */
const outcome = ref<Record<string, string>>({});
/** A delete waits for a second click. */
const deleting = ref<string | null>(null);

async function refresh() {
  rows.value = await hooksList();
}

onMounted(refresh);
watch(() => store.hooksVersion, refresh);

const STATUS: Record<HookStatus, string> = {
  new: "à essayer",
  active: "actif",
  off: "coupé",
  failing: "en échec",
  modified: "modifié — suspendu",
  broken: "illisible",
};

/** Sorted so what needs the user comes first. */
const sorted = computed(() => {
  const weight: Record<HookStatus, number> = {
    modified: 0,
    failing: 1,
    broken: 2,
    new: 3,
    active: 4,
    off: 5,
  };
  return [...rows.value].sort(
    (a, b) =>
      Number(!!b.awaiting) - Number(!!a.awaiting) ||
      weight[a.status] - weight[b.status] ||
      a.name.localeCompare(b.name),
  );
});

function every(secs: number | null): string {
  if (!secs) return "";
  if (secs % 86_400 === 0) return secs === 86_400 ? "tous les jours" : `tous les ${secs / 86_400} jours`;
  if (secs % 3600 === 0) return secs === 3600 ? "toutes les heures" : `toutes les ${secs / 3600} h`;
  return `toutes les ${Math.round(secs / 60)} min`;
}

function when(iso: string | null): string {
  if (!iso) return "—";
  const at = new Date(iso);
  if (Number.isNaN(at.getTime())) return "—";
  const time = at.toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" });
  const today = new Date().toDateString() === at.toDateString();
  return today ? time : `${at.toLocaleDateString("fr-FR", { day: "numeric", month: "short" })} ${time}`;
}

function record(r: HookResult): string {
  if (r.result === "nothing") return "rien de nouveau";
  if (r.result === "event") return r.say || r.why;
  return r.message;
}

async function act(what: () => Promise<void>) {
  problem.value = null;
  try {
    await what();
  } catch (e) {
    problem.value = String(e);
  }
  await refresh();
}

/** One run of the trial; its result then waits on the row for a verdict. */
async function step(row: HookRow) {
  if (running.value) return;
  running.value = row.id;
  open.value = row.id;
  problem.value = null;
  const { [row.id]: _gone, ...rest } = outcome.value;
  outcome.value = rest;
  try {
    const ran = await hookTrialStep(row.id);
    if (ran.refused) outcome.value = { ...outcome.value, [row.id]: ran.refused };
  } catch (e) {
    problem.value = String(e);
  } finally {
    running.value = null;
    await refresh();
  }
}

/** The user's verdict on the run shown. Right, and a run is left: it starts. */
async function judge(row: HookRow, right: boolean) {
  problem.value = null;
  try {
    const next = await hookTrialAnswer(row.id, right);
    if (next === "run_again") {
      await step(row);
      return;
    }
    outcome.value = {
      ...outcome.value,
      [row.id]:
        next === "validated"
          ? "Validé : il tourne seul désormais."
          : "Essai arrêté. Dis à ARIA ce que tu attendais, elle corrigera le script.",
    };
  } catch (e) {
    problem.value = String(e);
  }
  await refresh();
}

async function remove(row: HookRow) {
  if (deleting.value !== row.id) {
    deleting.value = row.id;
    return;
  }
  deleting.value = null;
  await act(() => hookDelete(row.id));
}

/** A trial is offered to a hook that is not running as validated. */
function triable(row: HookRow): boolean {
  return ["new", "modified"].includes(row.status) && !row.awaiting;
}
</script>

<template>
  <div class="sec">
    <div class="muted small">
      Des scripts qui surveillent pour toi et ne réveillent ARIA que s'il y a du nouveau. Demande-en un à
      voix haute : « préviens-moi quand… ». Ils vivent dans <code>~/.aria/hooks</code>.
    </div>

    <SecretPrompt v-for="ask in store.secretAsks" :key="ask.id" :ask="ask" />

    <div class="err" v-if="problem">{{ problem }}</div>
    <div class="muted" v-if="!rows.length">Aucun hook pour l'instant.</div>

    <div
      v-for="row in sorted"
      :key="row.id"
      class="hook"
      :class="[row.status, { expanded: open === row.id }]"
    >
      <button class="head" @click="open = open === row.id ? null : row.id">
        <span class="t">{{ row.name }}</span>
        <span class="meta">
          <span class="st">{{ STATUS[row.status] }}</span>
          <span class="dot" v-if="row.every_secs">·</span>
          <span v-if="row.every_secs">{{ every(row.every_secs) }}</span>
          <span class="dot" v-if="row.waiting">·</span>
          <span class="wait" v-if="row.waiting">{{ row.waiting }} en attente</span>
        </span>
      </button>

      <div class="why" v-if="row.status === 'modified'">
        Son script a changé depuis que tu l'as validé : il ne tourne plus. Essaie-le : deux passages, et tu
        juges chacun.
      </div>
      <div class="why" v-else-if="row.status === 'failing'">
        Coupé après trois échecs de suite. Le dernier est ci-dessous ; réactive-le une fois réglé.
      </div>
      <div class="why" v-else-if="row.problem">{{ row.problem }}</div>

      <div class="body" v-if="open === row.id">
        <div class="line">
          <span class="k">dernier passage</span><span>{{ when(row.last_run) }}</span>
        </div>
        <div class="line" v-if="row.next_run">
          <span class="k">prochain</span><span>{{ when(row.next_run) }}</span>
        </div>

        <div class="line" v-for="key in row.secrets" :key="key.name">
          <span class="k">secret {{ key.name }}</span>
          <span :class="key.stored ? 'stored' : 'missing'">{{ key.stored ? "enregistré" : "à saisir" }}</span>
          <button
            class="mini inline"
            :disabled="store.secretAsks.some((a) => a.hook === row.id && a.name === key.name)"
            @click="act(() => hookAskSecret(row.id, key.name))"
          >
            {{ key.stored ? "remplacer" : "saisir" }}
          </button>
        </div>

        <ul class="recent" v-if="row.recent.length">
          <li v-for="(r, i) in row.recent" :key="i" :class="r.result">
            <span class="at">{{ when(r.at) }}</span>
            <span class="r">{{ record(r) }}</span>
          </li>
        </ul>

        <div class="trial" v-if="row.awaiting">
          <div class="line">
            <span class="k">essai {{ row.awaiting.number }}/2</span>
            <span>{{ record(row.awaiting) }}</span>
          </div>
          <div class="note">Est-ce le résultat attendu ?</div>
          <div class="acts">
            <button class="mini go" :disabled="running !== null" @click="judge(row, true)">c'est bon</button>
            <button class="mini" :disabled="running !== null" @click="judge(row, false)">à corriger</button>
          </div>
        </div>
        <div class="trial" v-else-if="running === row.id">
          <div class="note">Passage en cours…</div>
        </div>
        <div class="trial" v-else-if="outcome[row.id]">
          <div class="note">{{ outcome[row.id] }}</div>
        </div>

        <div class="acts">
          <button
            class="mini"
            v-if="triable(row)"
            :disabled="running !== null"
            @click="step(row)"
          >
            essayer
          </button>
          <button class="mini" v-if="row.status === 'active'" @click="act(() => hookSetEnabled(row.id, false))">
            couper
          </button>
          <button
            class="mini"
            v-if="row.status === 'off' || row.status === 'failing'"
            @click="act(() => hookSetEnabled(row.id, true))"
          >
            réactiver
          </button>
          <button class="mini danger" @click="remove(row)">
            {{ deleting === row.id ? "confirmer la suppression" : "supprimer" }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.sec {
  padding: 16px 14px 4px;
}

.muted.small {
  font-size: 11.5px;
  margin-bottom: 10px;
}

code {
  font-family: var(--font-mono);
  font-size: 11px;
}

.err {
  color: #ff8d7a;
  font-size: 11.5px;
  margin: 6px 0 8px;
  overflow-wrap: anywhere;
}

.hook {
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  background: rgba(8, 14, 24, 0.5);
  margin-bottom: 7px;
}

/* What needs the user stands out until it is dealt with. */
.hook.modified,
.hook.failing {
  border-color: rgba(255, 141, 122, 0.55);
}

.hook .head {
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

.hook .t {
  color: var(--chrome);
  font-family: var(--font-hud);
  font-size: 13.5px;
  line-height: 1.25;
  overflow-wrap: anywhere;
}

.hook .meta {
  display: flex;
  flex-wrap: wrap;
  gap: 5px;
  color: var(--dim);
  font-family: var(--font-mono);
  font-size: 11px;
}

.hook .dot {
  opacity: 0.5;
}

.hook.active .st {
  color: var(--cyan);
}

.hook.modified .st,
.hook.failing .st,
.hook.broken .st {
  color: #ff8d7a;
}

.hook .wait {
  color: var(--chrome);
}

.why {
  padding: 0 10px 8px;
  font-size: 11.5px;
  color: var(--text);
  overflow-wrap: anywhere;
}

.body {
  border-top: 1px solid var(--line);
  padding: 8px 10px;
}

.line {
  display: flex;
  gap: 8px;
  font-size: 11.5px;
  padding: 2px 0;
  overflow-wrap: anywhere;
}

.line .k {
  flex: 0 0 96px;
  color: var(--dim);
  font-family: var(--font-mono);
  font-size: 11px;
}

.line .stored {
  color: var(--cyan);
}

.line .missing {
  color: #ff8d7a;
}

.mini.inline {
  margin-left: auto;
  padding: 1px 6px;
}

.recent {
  list-style: none;
  margin: 6px 0 0;
  padding: 0;
}

.recent li {
  display: flex;
  gap: 8px;
  font-size: 11.5px;
  padding: 2px 0;
}

.recent .at {
  flex: 0 0 96px;
  color: var(--dim);
  font-family: var(--font-mono);
  font-size: 11px;
}

.recent .r {
  overflow-wrap: anywhere;
}

.recent li.nothing .r {
  color: var(--dim);
}

.recent li.failed .r {
  color: #ff8d7a;
}

.trial .note {
  color: var(--dim);
  font-size: 11.5px;
  margin-top: 4px;
  overflow-wrap: anywhere;
}

.trial {
  margin-top: 8px;
  padding-top: 6px;
  border-top: 1px dashed var(--line);
}

.acts {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  margin-top: 8px;
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

.mini:hover:not(:disabled) {
  color: var(--chrome);
  border-color: rgba(183, 164, 255, 0.4);
}

.mini:disabled {
  opacity: 0.45;
  cursor: default;
}

.mini.go {
  color: var(--cyan);
  border-color: rgba(64, 220, 255, 0.4);
}

.mini.danger:hover {
  color: #ff8d7a;
  border-color: rgba(255, 141, 122, 0.45);
}
</style>
