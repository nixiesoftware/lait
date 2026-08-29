/**
 * Alive.
 *
 * Two things make this product move without anybody touching it, and the
 * interface has to show both.
 *
 *   **The clock.** What a screen shows is a function of the time. A broadcast
 *   window opens, a channel daypart turns over, a prayer time arrives. Nothing
 *   is pressed and the answer changes. So there is one tick, shared, and every
 *   resolved value recomputes from it.
 *
 *   **Everyone else.** Another operator sends a broadcast; a panel reports what
 *   it played. `GET /api/events` has carried that since before this product had
 *   a client, and nothing had ever subscribed to it — the whole plane was
 *   unused. It carries dirty *flags*, never state, so a doorbell means re-read,
 *   never patch.
 *
 * Both are context rather than per-component effects, because a hundred rows
 * each holding their own interval is a hundred clocks disagreeing by a frame,
 * and a hundred EventSources is a hundred connections.
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

type Live = {
  /** Shared wall clock. Everything time-dependent reads this, so the whole
   *  interface agrees about what "now" is. */
  now: number;
  /** Bumped when anything in this Space changed — ours or somebody else's. */
  revision: number;
  /** True between a doorbell arriving and the next paint, so a surface can
   *  say *why* it just changed rather than appearing to twitch. */
  arriving: boolean;
  /** The stream is attached. False is "we are not being told", which is a
   *  different fact from "nothing has happened" and must not read as calm. */
  attached: boolean;
  /** Re-read now, without waiting for a doorbell. */
  poke: () => void;
};

const LiveContext = createContext<Live>({
  now: Date.now(),
  revision: 0,
  arriving: false,
  attached: false,
  poke: () => {},
});

export function useLive(): Live {
  return useContext(LiveContext);
}

/**
 * Subscribe to whatever a hook needs re-read on.
 *
 * `revision` is deliberately the whole Space rather than a per-plane flag: the
 * doorbell groups invalidations by World, and this client is one World's, so
 * finer granularity would be precision the wire does not actually carry.
 */
export function useRevision(): number {
  return useLive().revision;
}

export function LiveProvider({
  tickMs = 1000,
  children,
}: {
  tickMs?: number;
  children: ReactNode;
}) {
  const [now, setNow] = useState(() => Date.now());
  const [revision, setRevision] = useState(0);
  const [arriving, setArriving] = useState(false);
  const [attached, setAttached] = useState(false);
  const settle = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), tickMs);
    return () => clearInterval(id);
  }, [tickMs]);

  const poke = useCallback(() => setRevision((n) => n + 1), []);

  useEffect(() => {
    // Same-origin in production; the vite proxy fakes that in dev, so the
    // credential rides either way and no token is ever visible to script.
    let source: EventSource | null = null;
    let closed = false;
    let retry: ReturnType<typeof setTimeout> | null = null;

    const attach = () => {
      if (closed) return;
      source = new EventSource("/api/events", { withCredentials: true });
      source.onopen = () => setAttached(true);

      const rang = () => {
        setRevision((n) => n + 1);
        setArriving(true);
        if (settle.current) clearTimeout(settle.current);
        settle.current = setTimeout(() => setArriving(false), 900);
      };

      // A doorbell is a dirty flag; a lag means we missed some, and the
      // response to both is identical — re-read the authoritative projection.
      // Surfacing lag rather than hiding it is what makes dropped frames
      // recoverable by construction.
      source.addEventListener("doorbell", rang);
      source.addEventListener("lagged", rang);

      source.onerror = () => {
        setAttached(false);
        source?.close();
        source = null;
        // Not being told is not the same as nothing happening, so reconnect
        // rather than sitting quiet and looking settled.
        if (!closed) retry = setTimeout(attach, 3000);
      };
    };

    attach();
    return () => {
      closed = true;
      if (retry) clearTimeout(retry);
      if (settle.current) clearTimeout(settle.current);
      source?.close();
    };
  }, []);

  const value = useMemo(
    () => ({ now, revision, arriving, attached, poke }),
    [now, revision, arriving, attached, poke],
  );

  return <LiveContext.Provider value={value}>{children}</LiveContext.Provider>;
}

/**
 * On air, with a running clock.
 *
 * A static badge says a state; a badge with elapsed time says a *duration*,
 * and duration is what tells an operator whether the thing interrupting their
 * fleet started a moment ago or has been up all afternoon. Substack and Suno
 * both do this and it is the difference between "live" as a label and "live"
 * as a fact you can act on.
 */
export function OnAir({
  since,
  label = "On air",
  tone = "live",
}: {
  since?: number | null;
  label?: string;
  /** `quiet` is a state that is not happening yet, or has finished. */
  tone?: "live" | "alarm" | "quiet";
}) {
  const { now } = useLive();
  const elapsed = since == null ? null : Math.max(0, now - since);
  return (
    <span className={`ds-onair is-${tone}`}>
      <span className="ds-onair-dot" aria-hidden />
      {label}
      {elapsed != null && <em>{clock(elapsed)}</em>}
    </span>
  );
}

/** A value that is computed rather than stored, marked as moving. */
export function LiveValue({ children }: { children: ReactNode }) {
  return (
    <span className="ds-livevalue">
      <span className="ds-livevalue-dot" aria-hidden />
      {children}
    </span>
  );
}

/**
 * How long ago, in the words Twingate uses — "just now" carries the aliveness
 * that a timestamp does not.
 */
export function Ago({ at }: { at: number | null | undefined }) {
  const { now } = useLive();
  if (at == null) return <span className="ds-ago is-never">never</span>;
  const delta = Math.max(0, now - at);
  if (delta < 45_000) return <span className="ds-ago is-now">just now</span>;
  const mins = Math.round(delta / 60_000);
  if (mins < 60) return <span className="ds-ago">{mins}m ago</span>;
  const hours = Math.round(mins / 60);
  if (hours < 48) return <span className="ds-ago">{hours}h ago</span>;
  return <span className="ds-ago">{Math.round(hours / 24)}d ago</span>;
}

function clock(ms: number): string {
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}
