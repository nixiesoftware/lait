/**
 * The Worker composition root's ROUTER — the half that owns the frames, split
 * from the half that owns the wasm.
 *
 * `workerLink` and `workerSession` (page side) post two frame families over one
 * port: the link lane (`rpc`/`abort`/`events`/`close` ↔ `reply`/`ring`/
 * `liveness`) and the session lane (`session:open`/`watch`/`mutate`/`close` ↔
 * `session:event`/`session:reply`). This router receives both on the Worker's
 * port and drives the composed engine handle to answer them, plus the two
 * STREAMING lanes the page cannot pull for itself: the doorbell (drainRing →
 * `ring`) and live carets (drainCaret → `session:event`).
 *
 * It is deliberately separable from `engine.worker.ts` (which loads the wasm,
 * fetches the runner, and calls `boot`) so the routing — every row of the table
 * below — is unit-testable over a `MessageChannel` with a stub handle, no wasm.
 *
 * | inbound `lait`     | action                                             |
 * |--------------------|----------------------------------------------------|
 * | `rpc`              | `handleLink` → post its `reply` frame              |
 * | `abort`            | `handleLink` (best-effort; no answer)              |
 * | `events`           | register sub; post `liveness:"live"`; pump rings   |
 * | `close`            | drop that events sub                               |
 * | `session:open`     | `handleSession` → post its frames; start caret loop|
 * | `session:watch`    | `handleSession` AND `watchCaret` (send-half caret) |
 * | `session:mutate`   | `handleSession` → post `session:reply`; pump rings |
 * | `session:close`    | `handleSession`; retire the sid from the caret map |
 *
 * One Live connection answers N watching sessions, and `drainCaret` returns a
 * whole-table view with `issue:null` — the engine leaves the sid and the
 * watched reff for this router to attach (it knows which session asked what).
 */

/** The composed engine surface the router drives — the wasm handle's methods,
 *  as an interface so a test can stub it. */
export interface EngineHandle {
  handleLink(frameJson: string): string;
  handleSession(frameJson: string): string;
  watchCaret(questionJson: string): Promise<boolean>;
  drainRing(): string | undefined;
  drainCaret(): Promise<string | undefined>;
  repull(): Promise<number>;
}

/** The port the router talks over — the Worker's global scope, or one end of a
 *  MessageChannel in a test. */
export interface RouterPort {
  postMessage(message: unknown): void;
  addEventListener(
    type: "message",
    listener: (event: MessageEvent) => void,
  ): void;
  start?(): void;
}

/** Tunables, so a test can disable the background poll and drive pumps by hand. */
export interface RouterOptions {
  /** Convergence poll period (ms). 0 disables the timer (tests pump manually). */
  pollMs?: number;
}

/**
 * Wire `handle` to `port`. Returns a stop function that tears down the loops
 * and timers. The composition root calls this once after `boot`; a test calls
 * it against a stub handle and a `MessageChannel`.
 */
export function engineRouter(
  handle: EngineHandle,
  port: RouterPort,
  options: RouterOptions = {},
): () => void {
  const pollMs = options.pollMs ?? 2000;
  // Events subscriptions: link-lane id → live. Rings fan to all of them.
  const events = new Set<number>();
  // Open session ids.
  const sessions = new Set<number>();
  // The latest (session, issue) a `session:watch` named. A drained caret is a
  // whole-table view (`issue:null`); the viewer's live.ts DROPS any live event
  // whose issue ≠ the question it asked, so the router stamps the watched reff.
  // The page's LivePlane watches one issue at a time (last-wins), and the tab
  // only receives carets for the field it subscribed — so the single latest
  // watch is the right label. (GAP 1: the engine leaves the reff to the Worker,
  // which knows it; the body→reff direction the engine lacks is not needed.)
  let watch: { sid: number; issue: string } | null = null;
  let stopped = false;
  let caretLoop: Promise<void> | null = null;
  let timer: ReturnType<typeof setInterval> | null = null;

  const post = (frame: unknown) => {
    if (!stopped) port.postMessage(frame);
  };

  /** Drain every pending doorbell ring and fan it to all events subscribers. */
  const pumpRings = () => {
    if (events.size === 0) return;
    for (;;) {
      let ring: string | undefined;
      try {
        ring = handle.drainRing();
      } catch {
        // A dormant observe stream ends the pump; the events lane goes quiet.
        return;
      }
      if (ring === undefined) return;
      const parsed = JSON.parse(ring);
      for (const id of events) post({ lait: "ring", id, ring: parsed });
    }
  };

  /** Await peer carets forever, stamping each with the watching session(s). */
  const runCaretLoop = async () => {
    for (;;) {
      if (stopped) return;
      let event: string | undefined;
      try {
        event = await handle.drainCaret();
      } catch {
        return; // the Live session ended; carets stop.
      }
      if (event === undefined) return; // no Live plane / connection closed.
      if (stopped) return;
      // No one is watching an issue yet — nowhere to route a caret.
      if (!watch) continue;
      const parsed = JSON.parse(event);
      // Stamp the watched reff so the page's live.ts admits it (it drops a
      // whole-table `issue:null` view against an asked-for issue).
      parsed.issue = watch.issue;
      post({ lait: "session:event", sid: watch.sid, event: parsed });
    }
  };

  const ensureCaretLoop = () => {
    if (!caretLoop) caretLoop = runCaretLoop();
  };

  port.addEventListener("message", (message: MessageEvent) => {
    const frame = message.data as { lait?: string } | null;
    if (!frame || typeof frame !== "object" || typeof frame.lait !== "string") {
      return;
    }
    const json = JSON.stringify(frame);
    switch (frame.lait) {
      case "rpc": {
        const reply = handle.handleLink(json);
        if (reply !== "null") post(JSON.parse(reply));
        // A world write may have committed; converge it out and reflect it back.
        void convergeAndPump();
        return;
      }
      case "abort": {
        // Best-effort: the engine has no cancellation, so this only drops any
        // late-reply bookkeeping. handleLink returns "null" for it.
        handle.handleLink(json);
        return;
      }
      case "events": {
        const id = (frame as { id: number }).id;
        events.add(id);
        post({ lait: "liveness", id, liveness: "live" });
        pumpRings();
        return;
      }
      case "close": {
        events.delete((frame as { id: number }).id);
        return;
      }
      case "session:open": {
        const sid = (frame as { sid: number }).sid;
        sessions.add(sid);
        for (const f of JSON.parse(handle.handleSession(json))) post(f);
        return;
      }
      case "session:watch": {
        // One frame, both lanes: the session host records the watch (silently),
        // and the caret send-half publishes this cursor.
        for (const f of JSON.parse(handle.handleSession(json))) post(f);
        const sid = (frame as { sid: number }).sid;
        const question = (frame as { question?: { issue?: string } | null }).question;
        if (question) {
          // Record which issue this session now watches, so drained carets are
          // stamped with its reff; publish this cursor as the send-half caret;
          // and start the caret receive loop now there is a watch to route to
          // (carets only arrive for a subscribed scope, so not before a watch).
          if (typeof question.issue === "string") watch = { sid, issue: question.issue };
          void handle.watchCaret(JSON.stringify(question));
          ensureCaretLoop();
        }
        return;
      }
      case "session:mutate": {
        for (const f of JSON.parse(handle.handleSession(json))) post(f);
        void convergeAndPump(); // a mutate commits; push it out + reflect.
        return;
      }
      case "session:close": {
        const sid = (frame as { sid: number }).sid;
        sessions.delete(sid);
        if (watch?.sid === sid) watch = null;
        handle.handleSession(json);
        return;
      }
    }
  });
  port.start?.();

  /** Push local writes out, pull peers' in, then surface both as rings. */
  const convergeAndPump = async () => {
    try {
      await handle.repull();
    } catch {
      // A failed converge is "could not be asked", not a write failure; the
      // next poll retries. The ring pump still surfaces any local commit.
    }
    pumpRings();
  };

  if (pollMs > 0) {
    timer = setInterval(() => void convergeAndPump(), pollMs);
  }

  return () => {
    stopped = true;
    if (timer) clearInterval(timer);
    events.clear();
    sessions.clear();
  };
}
