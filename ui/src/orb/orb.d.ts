/**
 * Types for `orb.js`, which stays plain JavaScript on purpose.
 *
 * The orb is the one component that was written three times before the views
 * drifted apart, so there is exactly one implementation and it is shared with
 * the mock-up. Rewriting it in TypeScript would fork it; declaring its shape
 * here types every caller without touching a line of the drawing.
 */

/** RGB, 0–255. */
export type Rgb = [number, number, number];

/**
 * A palette. Three colours with one job each, and the constraint is not
 * decorative: gold carries the whole body, violet and green live only on the
 * rotating arc crowns. Anything else carrying them turns the orb into a
 * rainbow.
 */
export interface Palette {
  name: string;
  /** The body: ring, halo, sphere rim, fringe. */
  primary: Rgb;
  /** The violet accent — outer arc crown, and nothing else. */
  arc1: Rgb;
  /** The green accent — inner arc crown, and nothing else. */
  arc2: Rgb;
  iris: Rgb;
  glass: Rgb;
  accent: Rgb;
}

/**
 * The three energy levels.
 *
 * The orb never goes fully dark: even muted, it breathes. The audio level does
 * not drive energy directly, it is *mapped* into the listening → speaking band,
 * so a normal voice fills that band without saturating it or getting lost.
 */
export interface Levels {
  muted: number;
  listening: number;
  speaking: number;
}

export interface OrbOptions {
  palette?: Palette;
  /** Raw voice activity in 0..1 — the orb maps it into the level band. */
  energy?: () => number;
  /**
   * How bright, 0 to 1. Multiplies nearly every stroke the renderer lays
   * down, which is what it always did — the boolean it used to be declared as
   * gave the orb two settings, black or full.
   */
  alive?: () => number;
  listening?: () => boolean;
  /**
   * ARIA is talking. Drives the ring, the fringe and the arcs — never the
   * ticks or the pupil, which are the microphone's (`energy`).
   */
  speaking?: () => boolean;
  /** Skip the frame entirely, for a hidden view. */
  paused?: () => boolean;
  levels?: Levels;
}

/**
 * The handle `ARIAOrb()` returns. It starts drawing immediately — there is no
 * `start()`, because a constructed orb that is not animating is a bug nobody
 * notices.
 */
export interface OrbHandle {
  setPalette(palette: Palette): void;
  /** Current energy, after smoothing and mapping into the level band. */
  value(): number;
  resize(): boolean;
  stop(): void;
}

interface OrbFactory {
  (canvas: HTMLCanvasElement, options?: OrbOptions): OrbHandle;
  PALETTES: Record<
    "amber" | "amberPlain" | "cyan" | "amethyst" | "emerald" | "aurora",
    Palette
  >;
  LEVELS: Levels;
  /** One measurement, so every view reacts the same way: RMS of the
   *  time-domain signal, not the spectrum average. */
  level(analyser: AnalyserNode | null, gain?: number): number;
}

export const ARIAOrb: OrbFactory;
