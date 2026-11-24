/* ===========================================================================
   A.R.I.A. ORB — one implementation, shared by every view.

   Each view used to draw the orb its own way, and they drifted apart. This
   file is the single source: the main window, the overview's small orb and
   the standalone overview window all call it.

   This is the PRODUCT's copy, in the real front end. The interface mock
   is the same file under the mock-up, kept working on `file://` with no build
   step — scaffolding, frozen, and due to go when the overview window is ported
   (AGENTS.md: the scaffolding comes down as the feature lands). Until then the
   two are diffable on purpose: everything below is byte-for-byte the prototype's,
   and only the export at the bottom differs. Change the drawing here, never
   there.

   Imported as an ES module by `Orb.vue`. The body is untouched — it still
   closes over `global` and assigns `global.ARIAOrb`, so the prototype's
   `<script src>` loading keeps working — and the named export is added at the
   end, for the bundler.

       const orb = ARIAOrb(canvas, {
         palette: ARIAOrb.PALETTES.amber,
         energy:  () => 0..1,        // ARIAOrb.level(analyser) for mic input
         alive:   () => true,        // session active; everything dims if not
       });

   Shape: a dark glass sphere, segmented HUD arcs, a luminous ring, radial
   ticks, an iris, and a dark pupil that contracts as energy rises. This is
   not a plasma ball: the light comes from the ring and the iris, not from
   the centre.

   Three colours, each with one job — otherwise it is a rainbow. Gold carries
   the WHOLE body: sphere, ring, iris, ticks, speech fringe. Violet and green
   live ONLY on the rotating arc crowns. Nothing else carries them, and that
   is what makes them read as accents rather than a second theme fighting the
   orb for attention.
   =========================================================================== */
(function (global) {
  "use strict";

  const TAU = Math.PI * 2;
  const rgba = (c, a) => "rgba(" + c[0] + "," + c[1] + "," + c[2] + "," + a + ")";
  const clamp01 = v => v < 0 ? 0 : v > 1 ? 1 : v;

  /* Three energy levels. The orb never goes fully dark: even muted, it
     breathes. The audio level does not drive energy directly, it is MAPPED
     into the listening → speaking band: a normal voice should fill that
     band, neither saturate it nor get lost in it. */
  const LEVELS = {
    muted:     0.16,   // audio channel closed
    listening: 0.21,   // channel open, silence
    speaking:  0.46,   // speech peak
  };

  /* --- palettes -----------------------------------------------------------
     primary  the body: ring, halo, sphere rim, fringe
     arc1     THE VIOLET ACCENT — outer arc crown, and nothing else
     arc2     THE GREEN ACCENT  — inner arc crown, and nothing else
     iris     the central disc, in the body's hue
     glass    the dark sphere
     accent   bright rims and ticks                                          */
  const PALETTES = {
    amber: {
      name:"amber + accents",
      primary:[255,179,71], arc1:[176,120,255], arc2:[46,205,152],
      iris:[228,152,58],    glass:[58,38,14],   accent:[255,240,214],
    },
    amberPlain: {
      name:"amber only",
      primary:[255,179,71], arc1:[214,140,54],  arc2:[255,200,120],
      iris:[228,152,58],    glass:[58,38,14],   accent:[255,240,214],
    },
    cyan: {
      name:"cyan (original)",
      primary:[64,220,255], arc1:[140,120,255], arc2:[46,205,152],
      iris:[46,200,210],    glass:[18,58,84],   accent:[235,252,255],
    },
    amethyst: {
      name:"violet",
      primary:[169,123,255], arc1:[90,200,255], arc2:[46,205,152],
      iris:[140,96,230],     glass:[46,30,86],  accent:[240,230,255],
    },
    emerald: {
      name:"green",
      primary:[46,230,168], arc1:[176,120,255], arc2:[120,235,255],
      iris:[32,190,150],    glass:[12,66,54],   accent:[226,255,244],
    },
    // The 2026-11-17 visual rework: the ring's gradient — indigo body, pink
    // and cyan crowns — so the orb belongs to the same family as the frames.
    aurora: {
      name:"aurora",
      primary:[123,108,255], arc1:[255,95,180], arc2:[63,208,255],
      iris:[100,84,230],     glass:[26,20,64],  accent:[232,228,255],
    },
  };

  /* =========================================================================
     AUDIO LEVEL — one measurement, so every view reacts the same way.

     We read the time-domain signal and take its RMS, not the spectrum
     average: the spectrum average crushes speech down to 0.05–0.25, which is
     below the resting floor, and the orb stops moving altogether. RMS with
     gain and a softened curve puts a normal voice in 0.4–0.9.
     ========================================================================= */
  function level(analyser, gain) {
    if (!analyser) return 0;
    const n = analyser.fftSize;
    const buf = new Uint8Array(n);
    analyser.getByteTimeDomainData(buf);
    let sum = 0;
    for (let i = 0; i < n; i++) { const v = (buf[i] - 128) / 128; sum += v * v; }
    const rms = Math.sqrt(sum / n);
    return clamp01(Math.pow(rms * (gain || 6), 0.75));
  }

  function ARIAOrb(canvas, opts) {
    const o = opts || {};
    const ctx = canvas.getContext("2d");
    let palette = o.palette || PALETTES.amber;
    /* `energy` returns raw VOICE ACTIVITY in 0..1 — mic level or simulation —
       not the final energy: the orb is what maps it into the level band. */
    const getVoice     = o.energy    || (() => 0);
    const getAlive     = o.alive     || (() => true);
    const getListening = o.listening || (() => true);
    /* ARIA's OWN voice, separate from the microphone (2026-11-17). Until then
       one energy drove everything, and the window fed it a constant 0.8 while
       ARIA spoke — so the user speaking and ARIA speaking moved the same parts
       the same way. `speaking` is a flag: no output level reaches the window
       yet, so the orb shapes a syllabic envelope of its own while it is on. */
    const getSpeaking  = o.speaking  || (() => false);
    const L = o.levels || LEVELS;

    let t = 0, raf = null, smooth = L.muted, say = 0;

    function size() {
      const r = canvas.getBoundingClientRect();
      if (!r.width || !r.height) return false;
      const dpr = global.devicePixelRatio || 1;
      const w = Math.max(1, Math.round(r.width * dpr));
      const h = Math.max(1, Math.round(r.height * dpr));
      if (canvas.width !== w || canvas.height !== h) { canvas.width = w; canvas.height = h; }
      return true;
    }

    function frame() {
      raf = global.requestAnimationFrame(frame);
      if (o.paused && o.paused()) return;
      if (!size()) return;
      t += 0.016;

      /* mapping: muted → muted, open and silent → listening, and the voice
         pushes up from there towards speaking. */
      const base = getListening() ? L.listening : L.muted;
      const target = base + (L.speaking - base) * clamp01(getVoice());
      smooth += (target - smooth) * 0.18;

      /* ARIA's envelope: on/off eased, times a syllable rhythm (~4.3 Hz with
         a slower phrase swell), so it reads as speech and not as a pulse. */
      const on = getSpeaking() ? 1 : 0;
      const syllable = 0.55 + 0.30 * Math.abs(Math.sin(t * Math.PI * 4.3))
                            + 0.15 * Math.sin(t * 1.3);
      say += (on * syllable - say) * (on ? 0.22 : 0.08);

      render(ctx, canvas, palette, t, smooth, getAlive() ? 1 : 0.3, L, say);
    }

    size();
    raf = global.requestAnimationFrame(frame);

    return {
      setPalette(p) { palette = p; },
      /* current energy, after smoothing and mapping into the level band */
      value() { return smooth; },
      resize: size,
      stop() { if (raf) global.cancelAnimationFrame(raf); raf = null; },
    };
  }

  /* =========================================================================
     THE DRAWING
     ========================================================================= */
  function render(ctx, canvas, P, t, e, alive, levels, said) {
    const L = levels || LEVELS;
    /* Two sources, two sets of parts (2026-11-17):
         the MICROPHONE (`e`, via `lift`)  → the ticks meter and the pupil:
                                             the eye that listens;
         ARIA'S VOICE   (`say`)           → ring swell and glow, wave fringe,
                                             arc speed: the mouth that speaks.
       Neither touches the other's parts, so who is talking reads at once. */
    const say = clamp01(said || 0);
    /* Two readings of the energy, and they do not serve the same purpose.

       `awake`: 0 when the channel is muted, 1 at the speech peak. Drives what
       is lit permanently — halo, ring brightness.

       `lift`:  0 while listening in silence, 1 at the peak. Drives the SPEECH
       effects. Without this second reading, the listening level would already
       show them at half strength and speaking would change nothing.

       The useful band is only 0.30 wide: any effect computed on raw `e` would
       be invisible. Everything therefore goes through one of these two. */
    const awake = clamp01((e - L.muted)     / (L.speaking - L.muted));
    const lift  = clamp01((e - L.listening) / (L.speaking - L.listening));

    const w = canvas.width, h = canvas.height;
    const cx = w / 2, cy = h / 2;
    const dpr = global.devicePixelRatio || 1;

    /* The canvas is transparent, but anything touching its edge is CUT OFF
       there — and a clipped gradient draws the canvas rectangle. That is what
       made a square appear to sit behind the orb: the halo had a radius of
       2.5 R, a diameter 1.45 times wider than the canvas.

       Two guards: R is calibrated so the sphere fits inside the inscribed
       circle, and the halo is bounded to that circle, where its alpha is
       already zero. Nothing may exceed RMAX. */
    const RMAX = Math.min(w, h) * 0.5;
    /* R = radius of the luminous ring. Everything else is a multiple of it. */
    const R = Math.min(w, h) * 0.265 * (1 + 0.045 * say + 0.006 * Math.sin(t * 1.7));

    ctx.clearRect(0, 0, w, h);
    ctx.save();
    ctx.translate(cx, cy);

    /* --- 1. diffuse golden halo ------------------------------------------
       Bounded to the inscribed circle: beyond it the canvas edge would clip
       the gradient and draw a square. */
    const haloR = Math.min(R * 2.5, RMAX);
    let g = ctx.createRadialGradient(0, 0, Math.min(R * 0.9, haloR * 0.5), 0, 0, haloR);
    g.addColorStop(0, rgba(P.primary, (0.09 + 0.17 * awake) * alive));
    g.addColorStop(1, rgba(P.primary, 0));
    ctx.fillStyle = g;
    ctx.beginPath(); ctx.arc(0, 0, haloR, 0, TAU); ctx.fill();

    /* --- 2. the dark glass sphere ----------------------------------------
       Dark at the centre, slightly brighter towards the edge, with a rim at
       the very edge: that rim is what makes it read as a sphere rather than
       a disc. It stays in the body's hue — the colour accents are reserved
       for the arcs. */
    const SR = R * 1.72;
    g = ctx.createRadialGradient(0, 0, R * 0.2, 0, 0, SR);
    g.addColorStop(0.00,  rgba([4, 8, 14], 0.92));
    g.addColorStop(0.55,  rgba(P.glass, 0.30 * alive + 0.10));
    g.addColorStop(0.90,  rgba(P.glass, 0.52 * alive + 0.12));
    g.addColorStop(0.975, rgba(P.primary, 0.28 * alive));
    g.addColorStop(1.00,  rgba(P.primary, 0));
    ctx.fillStyle = g;
    ctx.beginPath(); ctx.arc(0, 0, SR, 0, TAU); ctx.fill();

    /* specular highlight, top left — the sphere catches the light */
    ctx.save();
    ctx.beginPath(); ctx.arc(0, 0, SR, 0, TAU); ctx.clip();
    g = ctx.createRadialGradient(-SR * 0.42, -SR * 0.46, 0, -SR * 0.42, -SR * 0.46, SR * 0.85);
    g.addColorStop(0, rgba([255, 255, 255], 0.07 * alive));
    g.addColorStop(1, rgba([255, 255, 255], 0));
    ctx.fillStyle = g;
    ctx.fillRect(-SR, -SR, SR * 2, SR * 2);
    ctx.restore();

    /* --- 3. the rotating bars: THIS is where the accents live -------------
       Four crowns of broken arcs, at different speeds and in opposite
       directions. Violet is the outermost, green the innermost, and gold
       holds the two middle crowns: the accents frame the body instead of
       cutting across it. Both coloured crowns carry a glow, without which
       they vanish against the ring's brightness. */
    const ARCS = [
      { r:1.62, w:2.6, speed: 0.15, col:P.arc1,    a:0.66, glow:7,
        segs:[[0.02,0.26],[0.33,0.47],[0.56,0.74],[0.80,0.94]] },
      { r:1.46, w:5.2, speed:-0.10, col:P.primary, a:0.72, glow:0,
        segs:[[0.06,0.20],[0.27,0.33],[0.40,0.62],[0.70,0.88]] },
      { r:1.34, w:1.6, speed: 0.21, col:P.primary, a:0.30, glow:0,
        segs:[[0.10,0.38],[0.52,0.66],[0.74,0.96]] },
      { r:1.23, w:2.6, speed:-0.27, col:P.arc2,    a:0.62, glow:7,
        segs:[[0.00,0.12],[0.18,0.24],[0.44,0.58],[0.64,0.70],[0.86,0.98]] },
    ];
    for (const arc of ARCS) {
      ctx.save();
      /* speech speeds the mechanism up, but only barely: at the peak the
         rotation is just 50 % faster than at rest. At double, it made you
         dizzy the moment you spoke. */
      ctx.rotate(t * arc.speed * (1 + say * 0.5));
      if (arc.glow) {
        ctx.shadowColor = rgba(arc.col, 0.8 * alive);
        ctx.shadowBlur = arc.glow * dpr * (R / 90) * (1 + say);
      }
      ctx.strokeStyle = rgba(arc.col, (arc.a + 0.25 * say) * alive);
      ctx.lineWidth = arc.w * dpr * (R / 90);
      ctx.lineCap = "butt";
      for (const [s, en] of arc.segs) {
        ctx.beginPath();
        ctx.arc(0, 0, R * arc.r, s * TAU, en * TAU);
        ctx.stroke();
      }
      ctx.restore();
    }

    /* --- 4. the luminous ring: the brightest element, gold --------------- */
    ctx.save();
    ctx.shadowColor = rgba(P.primary, 0.85 * alive);
    ctx.shadowBlur = (12 + 18 * awake + 22 * say) * dpr * (R / 90);
    ctx.strokeStyle = rgba(P.primary, (0.84 + 0.16 * Math.max(awake, say)) * alive);
    ctx.lineWidth = R * 0.30;
    ctx.beginPath(); ctx.arc(0, 0, R * 1.00, 0, TAU); ctx.stroke();
    /* bright rim on the ring's inner edge */
    ctx.shadowBlur = 0;
    ctx.strokeStyle = rgba(P.accent, 0.5 * alive);
    ctx.lineWidth = Math.max(1, dpr * 0.8);
    ctx.beginPath(); ctx.arc(0, 0, R * 0.855, 0, TAU); ctx.stroke();
    ctx.restore();

    /* --- 5. radial ticks, top and bottom ---------------------------------
       They light up from the middle of each band outwards as the voice
       rises: a level meter that costs no extra interface element. Intensity
       only, never hue. */
    const BANDS = [[-0.40, -0.10], [0.10, 0.40]];   /* in turns, 0 = right */
    const n = 11;
    ctx.save();
    ctx.lineWidth = Math.max(1, dpr * 1.1 * (R / 90));
    for (const [from, to] of BANDS) {
      for (let i = 0; i < n; i++) {
        const k = i / (n - 1);
        const a = (from + (to - from) * k) * TAU;
        const hot = lift > Math.abs(k - 0.5) * 1.8;
        ctx.strokeStyle = hot
          ? rgba(P.accent, (0.88 + 0.12 * lift) * alive)
          : rgba(P.accent, (0.34 + 0.16 * lift) * alive);
        const r0 = R * 0.70, r1 = R * (hot ? 0.845 : 0.825);
        ctx.beginPath();
        ctx.moveTo(r0 * Math.cos(a), r0 * Math.sin(a));
        ctx.lineTo(r1 * Math.cos(a), r1 * Math.sin(a));
        ctx.stroke();
      }
    }
    ctx.restore();

    /* --- 6. the iris, in the body's hue ----------------------------------
       Bright at the edge, dark towards the centre: the opposite of a plasma
       ball, and that is what makes it read as an eye. */
    const IR = R * 0.68;
    g = ctx.createRadialGradient(0, 0, IR * 0.08, 0, 0, IR);
    g.addColorStop(0.00, rgba([6, 12, 16], 0.95));
    g.addColorStop(0.35, rgba(P.iris, 0.55 * alive + 0.08));
    g.addColorStop(0.80, rgba(P.iris, 0.90 * alive + 0.05));
    g.addColorStop(1.00, rgba(P.accent, 0.75 * alive));
    ctx.fillStyle = g;
    ctx.beginPath(); ctx.arc(0, 0, IR, 0, TAU); ctx.fill();

    /* very faint concentric circles inside the iris */
    ctx.strokeStyle = rgba([0, 0, 0], 0.22);
    ctx.lineWidth = Math.max(1, dpr * 0.8);
    for (const k of [0.40, 0.62, 0.84]) {
      ctx.beginPath(); ctx.arc(0, 0, IR * k, 0, TAU); ctx.stroke();
    }

    /* radial striations of the iris, very light */
    ctx.save();
    ctx.rotate(t * 0.05);
    ctx.strokeStyle = rgba([0, 0, 0], 0.16);
    ctx.lineWidth = Math.max(1, dpr * 0.7);
    for (let i = 0; i < 48; i++) {
      const a = (i / 48) * TAU;
      ctx.beginPath();
      ctx.moveTo(IR * 0.42 * Math.cos(a), IR * 0.42 * Math.sin(a));
      ctx.lineTo(IR * 0.92 * Math.cos(a), IR * 0.92 * Math.sin(a));
      ctx.stroke();
    }
    ctx.restore();

    /* --- 7. the pupil: it contracts as the voice rises ------------------- */
    const PR = IR * (0.32 - 0.16 * lift);
    g = ctx.createRadialGradient(0, 0, 0, 0, 0, PR * 1.5);
    g.addColorStop(0.0, rgba([0, 0, 0], 0.95));
    g.addColorStop(0.7, rgba([0, 0, 0], 0.75));
    g.addColorStop(1.0, rgba([0, 0, 0], 0));
    ctx.fillStyle = g;
    ctx.beginPath(); ctx.arc(0, 0, PR * 1.5, 0, TAU); ctx.fill();

    /* --- 8. wave fringe ---------------------------------------------------
       Its alpha starts from the FLOOR, not from zero: at rest it barely
       ripples, and it is the voice that brings it to life. Without that, the
       0.16 floor would show it permanently at full strength. */
    const fringe = 0.10 + say * 0.62;
    if (fringe > 0.04) {
      ctx.strokeStyle = rgba(P.accent, Math.min(0.78, fringe) * alive);
      ctx.lineWidth = Math.max(1, dpr * 1.2);
      ctx.beginPath();
      for (let i = 0; i <= 160; i++) {
        const a = (i / 160) * TAU;
        const rr = R * (1.20
          + 0.015 * Math.sin(a * 6 + t * 1.4)          /* resting breath */
          + 0.10 * say * Math.sin(a * 9 + t * 6)
          + 0.04 * say * Math.sin(a * 23 - t * 3));
        const x = rr * Math.cos(a), y = rr * Math.sin(a);
        i ? ctx.lineTo(x, y) : ctx.moveTo(x, y);
      }
      ctx.closePath(); ctx.stroke();
    }

    ctx.restore();
  }

  ARIAOrb.PALETTES = PALETTES;
  ARIAOrb.LEVELS = LEVELS;
  ARIAOrb.level = level;
  ARIAOrb.render = render;
  global.ARIAOrb = ARIAOrb;
})(window);

/* The one line that differs from the prototype's copy: the bundler wants a
   binding, not a global. `window.ARIAOrb` is still set above, so nothing that
   reads it breaks. */
export const ARIAOrb = window.ARIAOrb;
