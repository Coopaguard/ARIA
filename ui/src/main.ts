import { createApp } from "vue";

import App from "./App.vue";
import "./styles/tokens.css";
import "./styles/base.css";

/**
 * Snap the ring's width to whole **device** pixels.
 *
 * Seen on 2026-11-17: a 1.5 px ring under Windows display scaling lands
 * between device pixels, so the straight edges render thin and dim while the
 * curves, antialiased over two pixels, look thicker — an uneven frame on every
 * rounded surface. Rounding to a whole number of device pixels makes the
 * straight runs crisp and the same weight as the corners. Re-run when the
 * ratio changes, which it does when the window moves to another monitor.
 */
function snapRing() {
  const ratio = window.devicePixelRatio || 1;
  const device = Math.max(1, Math.round(1.5 * ratio));
  document.documentElement.style.setProperty("--ring-width", `${device / ratio}px`);
  window
    .matchMedia(`(resolution: ${ratio}dppx)`)
    .addEventListener("change", snapRing, { once: true });
}
snapRing();

createApp(App).mount("#app");
