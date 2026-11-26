<script setup lang="ts">
/**
 * The first-run screen (T12): five steps over the main window, once.
 *
 * **The models do not wait for it.** The voice and the ear start fetching their
 * weights the moment the application starts, with the shipped defaults — the
 * recommended ones. Every choice made here is applied live through the same
 * `set_config` the settings use, so the screen costs no time: the 2.4 GB of
 * the ear come down while someone reads the first step. The footer shows that
 * download on every step.
 *
 * **The steps are the settings themselves**, one group at a time
 * (`<Settings only="…">`), never a copy — AGENTS.md's first invariant. What is
 * here and nowhere else is what a newcomer needs before anything works: is
 * Claude Code installed, and — for a gated voice model, which the
 * French one no longer is — a Hugging Face token.
 *
 * Finishing writes `welcomed = true`. Nothing in the settings plays it again:
 * set `welcomed = false` in `aria.toml`.
 */
import { computed, onBeforeUnmount, ref, watch } from "vue";

import Settings from "./Settings.vue";
import {
  getConfig,
  hfToken,
  openLink,
  phononModels,
  providerInfo,
  saveConfig,
  saveHfToken,
} from "../lib/bridge";
import { LOAD_LABEL, type PhononModel, type ProviderInfo } from "../lib/contract";
import { store } from "../lib/store";

const emit = defineEmits<{ done: [] }>();

const STEPS = ["Bienvenue", "Agent", "Voix", "Micro", "Prêt"] as const;
const step = ref(0);

// ------------------------------------------------------------ Claude Code
const provider = ref<ProviderInfo | null>(null);
const checking = ref(false);
async function checkCli() {
  checking.value = true;
  try {
    provider.value = await providerInfo();
  } finally {
    checking.value = false;
  }
}
void checkCli();
const cliReady = computed(() => provider.value?.availability.state === "ready");

// --------------------------------------------------------- Hugging Face
/**
 * Whether the voice in force needs a token nobody has given yet.
 *
 * Re-read every few seconds while the voice step is open, because the model is
 * chosen in the embedded settings below and they do not report back.
 */
const needsToken = ref(false);
const token = ref("");
const tokenError = ref<string | null>(null);
const tokenSaved = ref(false);

async function checkToken() {
  try {
    const [config, models, has] = await Promise.all([getConfig(), phononModels(), hfToken()]);
    const french = config.voice.language.toLowerCase().startsWith("fr");
    const model: PhononModel | undefined =
      models.find((m) => m.repo === config.voice.phonon.model) ??
      (french ? models.find((m) => m.language === "fr") : undefined) ??
      models[0];
    needsToken.value = !!model && model.gated && !model.cached && !has;
  } catch {
    needsToken.value = false;
  }
}

async function sendToken() {
  tokenError.value = null;
  try {
    await saveHfToken(token.value);
    token.value = "";
    tokenSaved.value = true;
    await checkToken();
  } catch (e) {
    tokenError.value = String(e);
  }
}

let poll: number | undefined;
watch(
  step,
  (now) => {
    window.clearInterval(poll);
    if (STEPS[now] === "Voix") {
      void checkToken();
      poll = window.setInterval(checkToken, 3000);
    }
    if (STEPS[now] === "Bienvenue") void checkCli();
  },
  { immediate: true },
);
onBeforeUnmount(() => window.clearInterval(poll));

// ------------------------------------------------------------- progress
function size(bytes: number): string {
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1).replace(".", ",")} Go`;
  return `${Math.round(bytes / 1e6)} Mo`;
}

const download = computed(() => {
  const d = store.download;
  if (!d) return null;
  const share = d.size > 0 ? Math.min(1, d.received / d.size) : 0;
  return {
    line:
      `Téléchargement ${d.count > 1 ? `${d.index}/${d.count} ` : ""}: ${d.file}` +
      (d.size > 0 ? ` — ${size(d.received)} sur ${size(d.size)}` : ""),
    share,
  };
});

const loading = computed(() =>
  store.loading.pending.map((what) => LOAD_LABEL[what]).join(", "),
);
const ready = computed(() => !store.download && store.loading.pending.length === 0);

// --------------------------------------------------------------- finish
const finishing = ref(false);
async function finish() {
  finishing.value = true;
  try {
    // Read fresh: the embedded settings have written the file at every step.
    const config = await getConfig();
    await saveConfig({ ...config, welcomed: true });
    emit("done");
  } finally {
    finishing.value = false;
  }
}
</script>

<template>
  <div class="scrim" role="dialog" aria-modal="true" aria-labelledby="frTitle">
    <div class="card ring">
      <header>
        <h2 id="frTitle">Premier démarrage</h2>
        <ol class="steps">
          <li
            v-for="(name, i) in STEPS"
            :key="name"
            :data-state="i === step ? 'on' : i < step ? 'done' : 'todo'"
          >
            <button :disabled="i > step" @click="step = i">{{ name }}</button>
          </li>
        </ol>
      </header>

      <div class="body">
        <!-- 1 ─ Welcome, and the one thing that cannot be fixed from here. -->
        <section v-if="step === 0">
          <p class="lead">Bonjour, je suis <b>ARIA</b>.</p>
          <p>
            Tu me parles, je réponds à voix haute, et je confie les tâches
            longues à des agents qui travaillent pendant que tu fais autre chose.
            Quatre réglages, et c'est parti. Les modèles se téléchargent déjà en
            arrière-plan.
          </p>
          <div class="check" :data-ok="cliReady ? '1' : '0'">
            <template v-if="checking && !provider">Recherche de Claude Code…</template>
            <template v-else-if="cliReady">
              ✓ Claude Code est installé
              <span class="muted">({{ provider?.display_name }})</span>
            </template>
            <template v-else>
              <b>Claude Code est introuvable.</b> C'est lui qui fait réfléchir
              ARIA. Installe-le en suivant
              <a href="https://claude.com/claude-code" @click.prevent="openLink('https://claude.com/claude-code')">claude.com/claude-code</a>,
              puis connecte-toi en lançant <code>claude</code> une fois dans un
              terminal.
              <button class="hudbtn" :disabled="checking" @click="checkCli">
                {{ checking ? "…" : "vérifier à nouveau" }}
              </button>
            </template>
          </div>
        </section>

        <!-- 2 ─ The main agent. -->
        <section v-else-if="step === 1">
          <p>
            Qui te répond. Le mode recommandé transcrit ta voix et la confie à
            Claude Code ; un modèle local tourne entièrement sur cet ordinateur.
          </p>
          <Settings only="agent" />
        </section>

        <!-- 3 ─ The voice, and the token the French ones need. -->
        <section v-else-if="step === 2">
          <div v-if="needsToken" class="check" data-ok="0">
            <b>Le modèle de voix choisi demande un compte Hugging Face.</b>
            <ol class="howto">
              <li>
                Accepte la licence du modèle sur
                <a href="https://huggingface.co/gradium" target="_blank" rel="noreferrer">huggingface.co/gradium</a>.
              </li>
              <li>
                Crée un jeton en lecture sur
                <a href="https://huggingface.co/settings/tokens" target="_blank" rel="noreferrer">huggingface.co/settings/tokens</a>,
                puis colle-le ici.
              </li>
            </ol>
            <div class="pair">
              <input
                v-model="token"
                class="txt"
                type="password"
                placeholder="hf_…"
                spellcheck="false"
                aria-label="Jeton Hugging Face"
                @keydown.enter="sendToken"
              />
              <button class="hudbtn" :disabled="!token.trim()" @click="sendToken">
                enregistrer
              </button>
            </div>
            <p v-if="tokenError" class="err">{{ tokenError }}</p>
          </div>
          <p v-else-if="tokenSaved" class="check" data-ok="1">
            ✓ Jeton enregistré : la voix se charge.
          </p>
          <Settings only="voice" />
        </section>

        <!-- 4 ─ The microphone. -->
        <section v-else-if="step === 3">
          <p>
            Pour qu'ARIA t'entende bien et ignore le bruit de la pièce. Facultatif,
            mais conseillé.
          </p>
          <Settings only="listen" />
        </section>

        <!-- 5 ─ Ready, or nearly. -->
        <section v-else>
          <p class="lead" v-if="ready">Tout est prêt.</p>
          <p class="lead" v-else>Presque prêt.</p>
          <ul class="tips">
            <li>Dis « <b>Aria</b> » suivi de ta demande, ou écris-la en bas.</li>
            <li>
              <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>C</kbd> affiche ou cache ARIA
              depuis n'importe où.
            </li>
            <li>Fermer la fenêtre ne l'arrête pas : elle reste près de l'horloge.</li>
            <li>Tout se change ensuite dans les réglages, menu de gauche.</li>
          </ul>
          <p v-if="!ready" class="muted">
            Tu peux commencer maintenant : le reste finit de se charger en
            arrière-plan.
          </p>
        </section>
      </div>

      <footer>
        <div class="progress" aria-live="polite">
          <template v-if="download">
            <div class="bar"><i :style="{ width: `${download.share * 100}%` }" /></div>
            <span>{{ download.line }}</span>
          </template>
          <span v-else-if="loading">Chargement : {{ loading }}…</span>
          <span v-else class="muted">Modèles prêts.</span>
        </div>
        <div class="nav">
          <button v-if="step > 0" class="hudbtn" @click="step -= 1">précédent</button>
          <button
            v-if="step < STEPS.length - 1"
            class="hudbtn primary"
            @click="step += 1"
          >
            suivant
          </button>
          <button
            v-else
            class="hudbtn primary"
            :disabled="finishing"
            @click="finish"
          >
            commencer
          </button>
        </div>
      </footer>
    </div>
  </div>
</template>

<style scoped>
.scrim {
  position: fixed;
  inset: 0;
  z-index: 50;
  display: grid;
  place-items: center;
  padding: 24px;
  background: rgba(1, 4, 9, 0.82);
  backdrop-filter: blur(6px);
}

.card {
  width: min(680px, 100%);
  max-height: calc(100vh - 48px);
  display: flex;
  flex-direction: column;
  border-radius: var(--r-lg);
  background: #050a12;
  box-shadow: var(--glow);
  overflow: hidden;
}

header {
  padding: 18px 22px 12px;
  border-bottom: 1px solid rgba(183, 164, 255, 0.14);
}
h2 {
  margin: 0 0 12px;
  font-family: var(--font-mono);
  font-size: 12px;
  font-weight: 400;
  letter-spacing: 0.3em;
  text-transform: uppercase;
  color: var(--chrome);
}

.steps {
  display: flex;
  gap: 6px;
  margin: 0;
  padding: 0;
  list-style: none;
}
.steps li {
  flex: 1;
}
.steps button {
  width: 100%;
  min-height: 28px;
  padding: 4px 6px;
  border: 0;
  border-bottom: 2px solid rgba(183, 164, 255, 0.18);
  background: none;
  color: var(--dim);
  font-family: var(--font-hud);
  font-size: 13px;
  cursor: pointer;
}
.steps button:disabled {
  cursor: default;
}
.steps [data-state="done"] button {
  color: var(--text);
  border-bottom-color: rgba(183, 164, 255, 0.5);
}
.steps [data-state="on"] button {
  color: var(--chrome);
  border-bottom-color: var(--chrome);
}

.body {
  flex: 1;
  overflow-y: auto;
  padding: 18px 22px;
  font-family: var(--font-read);
  font-size: 14px;
  line-height: 1.55;
  color: var(--text);
}
.body p {
  margin: 0 0 12px;
}
.lead {
  font-size: 18px;
}
.lead b {
  color: var(--amber);
  font-weight: 600;
}
.muted {
  color: var(--dim);
}

/* A prerequisite, said once and plainly: green when met, red when not. */
.check {
  margin: 14px 0;
  padding: 12px 14px;
  border-radius: var(--r-md);
  border: 1px solid rgba(255, 84, 112, 0.45);
  background: rgba(255, 84, 112, 0.06);
}
.check[data-ok="1"] {
  border-color: rgba(67, 255, 164, 0.4);
  background: rgba(67, 255, 164, 0.05);
}
.check pre,
.check code {
  font-family: var(--font-mono);
  font-size: 12px;
}
.check pre {
  margin: 8px 0 10px;
  padding: 8px 10px;
  border-radius: var(--r-sm);
  background: rgba(255, 255, 255, 0.05);
  user-select: all;
}
.howto {
  margin: 8px 0 10px;
  padding-left: 20px;
}
.check a {
  color: var(--cyan);
}
.pair {
  display: flex;
  gap: 8px;
}
.txt {
  flex: 1;
  min-height: 28px;
  padding: 4px 10px;
  border-radius: var(--r-sm);
  border: 1px solid rgba(183, 164, 255, 0.3);
  background: rgba(255, 255, 255, 0.04);
  color: var(--text);
  font-family: var(--font-mono);
  font-size: 12px;
}
.err {
  margin-top: 8px;
  color: var(--err);
}

.tips {
  margin: 0 0 14px;
  padding-left: 20px;
}
.tips li {
  margin-bottom: 6px;
}
kbd {
  padding: 1px 6px;
  border-radius: 5px;
  border: 1px solid rgba(183, 164, 255, 0.35);
  font-family: var(--font-mono);
  font-size: 12px;
}

/* The embedded settings bring their own padding for the menu; here the card
   already has it. */
.body :deep(.settings) {
  padding: 0;
}

footer {
  display: flex;
  align-items: center;
  gap: 16px;
  padding: 12px 22px 16px;
  border-top: 1px solid rgba(183, 164, 255, 0.14);
}
.progress {
  flex: 1;
  min-width: 0;
  font-family: var(--font-mono);
  font-size: 11px;
  color: var(--text);
}
.progress span {
  display: block;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.bar {
  height: 4px;
  margin-bottom: 6px;
  border-radius: 2px;
  background: rgba(255, 255, 255, 0.08);
  overflow: hidden;
}
.bar i {
  display: block;
  height: 100%;
  background: var(--ring);
  transition: width 0.3s;
}
.nav {
  display: flex;
  gap: 8px;
}
.primary {
  border-color: var(--chrome);
  color: var(--chrome);
}
</style>
