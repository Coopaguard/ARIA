/**
 * Updates from GitHub releases — the window side of `app/src/update.rs`.
 *
 * Checked at start and then every six hours: ARIA lives for days, so a check
 * at start alone would wait for a restart nobody makes. A failed check shows
 * nothing; only a real offer does.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { reactive } from "vue";

export interface UpdateAvailable {
  version: string;
  current: string;
  notes: string | null;
}

const EVERY = 6 * 60 * 60 * 1000;

export const update = reactive({
  offer: null as UpdateAvailable | null,
  /** The offer the user said « plus tard » to: not shown again until a newer one. */
  declined: null as string | null,
  /** Download progress while installing, in percent. */
  percent: null as number | null,
  checking: false,
  error: null as string | null,
});

export async function checkUpdate(): Promise<UpdateAvailable | null> {
  update.checking = true;
  try {
    update.offer = await invoke<UpdateAvailable | null>("update_check").catch(() => null);
    return update.offer;
  } finally {
    update.checking = false;
  }
}

export async function installUpdate(): Promise<void> {
  update.error = null;
  update.percent = 0;
  const unlisten = await listen<number>("update://progress", (event) => {
    update.percent = event.payload;
  });
  try {
    // Returns only on failure: otherwise ARIA ends and the installer runs.
    await invoke("update_install");
  } catch (e) {
    update.error = String(e);
    update.percent = null;
  } finally {
    unlisten();
  }
}

export function declineUpdate(): void {
  update.declined = update.offer?.version ?? null;
}

let started = false;

export function watchUpdates(): void {
  if (started) return;
  started = true;
  void checkUpdate();
  setInterval(() => void checkUpdate(), EVERY);
}
