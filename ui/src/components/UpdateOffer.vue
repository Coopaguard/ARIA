<script setup lang="ts">
/**
 * The offer of a newer version, at the top of the main window.
 *
 * A card and not a modal: ARIA may be mid-sentence, and an update is never
 * urgent enough to stand between the user and the conversation.
 */
import { computed } from "vue";

import { declineUpdate, installUpdate, update } from "../lib/update";

const shown = computed(
  () => !!update.offer && (update.declined !== update.offer.version || update.percent !== null),
);
</script>

<template>
  <div v-if="shown && update.offer" class="update ring" role="status">
    <div class="what">
      <b>ARIA {{ update.offer.version }} est disponible</b>
      <span class="cur">version installée : {{ update.offer.current }}</span>
    </div>
    <p v-if="update.offer.notes && update.percent === null" class="notes">
      {{ update.offer.notes }}
    </p>
    <div v-if="update.percent !== null" class="bar" :aria-label="`Téléchargement ${update.percent} %`">
      <span :style="{ width: `${update.percent}%` }" />
    </div>
    <p v-if="update.percent !== null" class="hint">
      Téléchargement {{ update.percent }} % — ARIA redémarrera après l'installation,
      les tâches en cours s'arrêteront.
    </p>
    <p v-if="update.error" class="err">{{ update.error }}</p>
    <div v-if="update.percent === null" class="acts">
      <button class="hudbtn" @click="declineUpdate">plus tard</button>
      <button class="hudbtn go" @click="installUpdate">installer</button>
    </div>
  </div>
</template>

<style scoped>
.update {
  position: fixed;
  top: 64px;
  left: 50%;
  transform: translateX(-50%);
  z-index: 40;
  width: min(440px, calc(100vw - 32px));
  padding: 14px 16px;
  border-radius: 14px;
  background: var(--panel);
  backdrop-filter: blur(14px);
  box-shadow: var(--glow);
  display: flex;
  flex-direction: column;
  gap: 8px;
  font-size: 13px;
}
.what {
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.what b {
  color: var(--chrome);
  font-size: 14px;
}
.cur,
.hint {
  color: var(--dim);
  font-size: 12px;
}
.notes {
  font-family: var(--font-read);
  max-height: 120px;
  overflow: auto;
  white-space: pre-wrap;
  margin: 0;
}
.hint,
.err {
  margin: 0;
}
.err {
  color: var(--err);
  font-size: 12px;
}
.bar {
  height: 6px;
  border-radius: 3px;
  background: rgba(183, 164, 255, 0.15);
  overflow: hidden;
}
.bar span {
  display: block;
  height: 100%;
  background: var(--chrome);
  transition: width 0.2s;
}
.acts {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
.go {
  color: var(--chrome);
  border-color: var(--chrome);
}
</style>
