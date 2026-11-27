<script setup lang="ts">
/**
 * The form a hook's secret is typed into.
 *
 * ARIA's own, and that is the whole point: a tile is written by the model, and
 * its script could carry what is typed out through `ui/open-link`. This form
 * is fixed code. What is typed goes over the IPC to the system store, and
 * nowhere else: not into the event stream, not to the session that asked,
 * which only learns that it was saved.
 */
import { ref } from "vue";

import { secretCancel, secretSubmit } from "../lib/bridge";
import type { SecretAsk } from "../lib/contract";

const props = defineProps<{ ask: SecretAsk }>();

const value = ref("");
const busy = ref(false);
const problem = ref<string | null>(null);

async function save() {
  if (!value.value || busy.value) return;
  busy.value = true;
  problem.value = null;
  try {
    await secretSubmit(props.ask.id, value.value);
    // The form goes with `secret.closed`; the value goes now.
    value.value = "";
  } catch (e) {
    problem.value = String(e);
  } finally {
    busy.value = false;
  }
}

async function cancel() {
  value.value = "";
  await secretCancel(props.ask.id);
}
</script>

<template>
  <form class="secret" autocomplete="off" @submit.prevent="save">
    <div class="who">Secret pour le hook « {{ ask.hook_name }} »</div>
    <label class="label" :for="`secret-${ask.id}`">{{ ask.label }}</label>
    <input
      :id="`secret-${ask.id}`"
      v-model="value"
      type="password"
      autocomplete="new-password"
      spellcheck="false"
      :disabled="busy"
    />
    <div class="note">
      Rangé dans le trousseau du système. Ni ARIA ni ses sessions ne le voient : seul le script le reçoit.
    </div>
    <div class="err" v-if="problem">{{ problem }}</div>
    <div class="acts">
      <button type="submit" class="mini go" :disabled="!value || busy">enregistrer</button>
      <button type="button" class="mini" :disabled="busy" @click="cancel">annuler</button>
    </div>
  </form>
</template>

<style scoped>
.secret {
  border: 1px solid rgba(64, 220, 255, 0.45);
  border-radius: var(--r-sm);
  background: rgba(8, 14, 24, 0.7);
  padding: 10px;
  margin-bottom: 10px;
}

.who {
  color: var(--dim);
  font-family: var(--font-mono);
  font-size: 11px;
  letter-spacing: 0.08em;
  margin-bottom: 4px;
  overflow-wrap: anywhere;
}

.label {
  display: block;
  color: var(--chrome);
  font-size: 13px;
  margin-bottom: 6px;
  overflow-wrap: anywhere;
}

input {
  width: 100%;
  box-sizing: border-box;
  background: rgba(0, 0, 0, 0.35);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  color: var(--text);
  font-family: var(--font-mono);
  padding: 6px 8px;
}

.note {
  color: var(--dim);
  font-size: 11px;
  margin-top: 6px;
}

.err {
  color: #ff8d7a;
  font-size: 11.5px;
  margin-top: 6px;
  overflow-wrap: anywhere;
}

.acts {
  display: flex;
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

.mini:disabled {
  opacity: 0.45;
  cursor: default;
}

.mini.go {
  color: var(--cyan);
  border-color: rgba(64, 220, 255, 0.4);
}
</style>
