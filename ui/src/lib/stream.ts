/**
 * The event stream, and the part that is easy to get wrong: resuming.
 *
 * The core keeps one ordered stream with a sequence number per event, so a
 * window that was closed, reloaded or briefly disconnected **replays what it
 * missed** rather than starting from a blank screen. This file is the client
 * half of that, and it has exactly three jobs:
 *
 *  1. remember the last sequence actually applied;
 *  2. reconnect with it, and apply what comes back;
 *  3. when the core says that position has fallen off its ring, ask for a fresh
 *     snapshot instead of pretending.
 *
 * Point 3 is the one worth being stubborn about. Rendering whatever the ring
 * still holds after a gap produces a plausible, wrong history — a hole in the
 * activity feed that nobody can see. So a gap resets the state from a snapshot,
 * loudly.
 */

import type { Envelope, Resume, Seq } from "./contract";

export interface StreamHandlers {
  /** Apply one event. Called in sequence order, replay and live alike. */
  apply(envelope: Envelope): void;
  /**
   * The microphone level, 0 to 1.
   *
   * **Not an event**, and the socket marks it as such: no sequence, never
   * replayed, not in the audit. It arrives twenty times a second and is
   * worthless one frame later, so folding it into the ordered stream would
   * push every real event out of the replay ring in a minute.
   */
  meter(level: number): void;
  /**
   * The stream could not resume from where we were. Re-read the snapshot and
   * drop what is on screen.
   */
  resync(reason: { asked: Seq; oldest: Seq }): void;
  /** Connection state, for the interface to show. */
  connected(open: boolean): void;
  /**
   * The stream has never managed to open, `attempts` times running.
   *
   * Without this the retry loop is silent for ever: the window shows "core
   * injoignable" and there is no way, from the screen, to tell a core that is
   * still starting from one that is refusing the token. Called on every failed
   * attempt so the count itself says which of the two it is.
   */
  trouble(attempts: number, url: string): void;
}

/** Backoff between reconnection attempts, in milliseconds. */
const RETRY_FLOOR = 300;
const RETRY_CEILING = 5_000;

export class EventStream {
  private socket: WebSocket | null = null;
  private retry = RETRY_FLOOR;
  private closed = false;
  /** The last sequence actually applied — the whole point of this class. */
  private seq: Seq = 0;
  /** Whether the socket has ever been open, which is what makes a close
   *  ordinary rather than a failure to report. */
  private everOpened = false;
  private failures = 0;

  constructor(
    private readonly url: (since: Seq) => string,
    private readonly handlers: StreamHandlers,
  ) {}

  /**
   * Start, or restart after a snapshot.
   *
   * `since` is where the caller is: after a fresh snapshot it is the snapshot's
   * own sequence, so the stream picks up exactly where the snapshot ends.
   */
  open(since: Seq = this.seq): void {
    this.closed = false;
    this.seq = since;
    this.connect();
  }

  close(): void {
    this.closed = true;
    this.socket?.close();
    this.socket = null;
  }

  /** The last sequence applied, for a caller that needs to resume from it. */
  position(): Seq {
    return this.seq;
  }

  private connect(): void {
    const address = this.url(this.seq);
    const socket = new WebSocket(address);
    this.socket = socket;

    socket.onopen = () => {
      this.retry = RETRY_FLOOR;
      this.everOpened = true;
      this.failures = 0;
      this.handlers.connected(true);
    };

    socket.onmessage = (message) => {
      let payload: Resume | Envelope | { type: "meter"; level: number };
      try {
        payload = JSON.parse(message.data as string);
      } catch {
        // A frame we cannot parse is our own contract breaking. Say so rather
        // than skip it silently — a stream that drops frames quietly is a
        // stream that lies about the state of the machine.
        console.error("unparseable frame on the event stream", message.data);
        return;
      }

      // Checked first and returned early: it carries no sequence, so it must
      // never reach `take`, which would read `undefined` as a position.
      if (payload.type === "meter") {
        this.handlers.meter(payload.level);
        return;
      }
      if (payload.type === "resume.gap") {
        this.handlers.resync({ asked: payload.asked, oldest: payload.oldest });
        return;
      }
      if (payload.type === "resume.replay") {
        for (const envelope of payload.events) this.take(envelope);
        return;
      }
      this.take(payload);
    };

    socket.onclose = () => {
      this.handlers.connected(false);
      if (this.closed) return;
      // Only when it has never worked. A socket that closes after having been
      // open is the window reloading, which is ordinary and says nothing.
      if (!this.everOpened) {
        this.failures += 1;
        this.handlers.trouble(this.failures, address);
      }
      // The core outlives the window, so a closed socket usually means the
      // window reloaded, not that anything failed. Reconnect with the position
      // we hold and the replay fills the gap.
      window.setTimeout(() => this.connect(), this.retry);
      this.retry = Math.min(this.retry * 2, RETRY_CEILING);
    };

    // `onerror` is always followed by `onclose`, which does the reconnecting.
    // Handled only so the console does not report it as unhandled.
    socket.onerror = () => {};
  }

  private take(envelope: Envelope): void {
    // Idempotent on purpose: a replay may overlap what the live stream already
    // delivered, and applying an event twice would double a message fragment
    // or an activity step.
    if (envelope.seq <= this.seq) return;
    this.seq = envelope.seq;
    this.handlers.apply(envelope);
  }
}
