/**
 * The television(s) that are this screen.
 *
 * A Signage screen is a real screen, so this section speaks only of the
 * hardware that is it: the TV showing it — or the panels of a wall, which
 * play in step — and a code waiting for one to connect. Nothing else appears
 * here. A TV that is idle, or one asking to connect by words, is a fleet
 * fact and is offered a screen on the Screens page, not on another screen's.
 */

import { useEffect, useMemo, useState } from "react";
import { Plus, Tv, X } from "lucide-react";
import { haptic, useToast } from "@/ds";
import {
  codeEntry,
  forgetTv,
  minutesLeft,
  mintTvCode,
  platformName,
  revokeTvCode,
  screenOf,
  tvStatus,
  unassignTv,
  useTvs,
  type TvCode,
  type TvReceiver,
} from "@/utils/tv/api";

export function TvSection({ screenId, screenName }: { screenId: string; screenName: string }) {
  const toast = useToast();
  const { fleet, error, refresh } = useTvs();
  const [naming, setNaming] = useState(false);
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 15_000);
    return () => clearInterval(timer);
  }, []);

  const mine = useMemo(
    () => (fleet?.receivers ?? []).filter((tv) => screenOf(tv.assignment?.input) === screenId),
    [fleet, screenId],
  );
  const codes = useMemo(
    () => (fleet?.codes ?? []).filter((code) => screenOf(code.input) === screenId && code.state !== "connected"),
    [fleet, screenId],
  );

  const act = async (what: string, work: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await work();
      haptic("save");
      await refresh();
    } catch (err) {
      haptic("error");
      toast.show(what, err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  const mint = () =>
    act("Could not get a code", async () => {
      await mintTvCode(screenId, name.trim() || screenName);
      setNaming(false);
      setName("");
    });

  return (
    <div className="ds-tvs">
      {error && <p className="ds-hint">Couldn't ask about TVs: {error}</p>}

      {mine.map((tv) => (
        <TvRow key={tv.device} tv={tv} now={now} busy={busy}
          onUnpoint={() => void act("Could not take the TV off", () => unassignTv(tv.device))}
          onForget={() => void act("Could not forget the TV", () => forgetTv(tv.device))} />
      ))}

      {codes.map((code) => (
        <CodeRow key={code.rendezvous} code={code} now={now} busy={busy}
          onWithdraw={() => void act("Could not withdraw the code", () => revokeTvCode(code.rendezvous))} />
      ))}

      {mine.length === 0 && codes.length === 0 && (
        <p className="ds-hint">No TV is this screen yet. Add one: you'll get a code to type on the TV.</p>
      )}

      <div className="ds-page-actions">
        {naming ? (
          <span className="ds-tv-name">
            <input className="ds-input" value={name} placeholder={screenName} aria-label="TV name" autoFocus
              onChange={(event) => setName(event.target.value)}
              onKeyDown={(event) => { if (event.key === "Enter") void mint(); if (event.key === "Escape") setNaming(false); }} />
            <button type="button" className="ds-btn ds-btn-solid" disabled={busy} onClick={() => void mint()}>Get a code</button>
            <button type="button" className="ds-icon" aria-label="Cancel" onClick={() => setNaming(false)}><X size={16} /></button>
          </span>
        ) : (
          <button type="button" className="ds-btn ds-btn-solid" disabled={busy} onClick={() => { setNaming(true); setName(""); }}>
            <Plus size={15} />
            {mine.length === 0 ? "Add a TV" : "Add another TV"}
          </button>
        )}
        {mine.length > 1 && <span className="ds-hint">These TVs play this screen in step.</span>}
      </div>
    </div>
  );
}

function TvRow({ tv, now, busy, onUnpoint, onForget }: {
  tv: TvReceiver; now: number; busy: boolean; onUnpoint(): void; onForget(): void;
}) {
  const status = tvStatus(tv, now);
  return (
    <div className="ds-tv-row">
      <span className="ds-tv-mark"><Tv size={16} /></span>
      <span className="ds-tv-copy">
        <strong>{tv.label}</strong>
        <span>{platformName(tv.platform)} · <span className={`ds-tv-status is-${status.tone}`}>{status.label}</span></span>
      </span>
      <span className="ds-tv-acts">
        <button type="button" className="ds-btn ds-btn-quiet" disabled={busy} onClick={onUnpoint}>Detach</button>
        <button type="button" className="ds-btn ds-btn-quiet is-danger" disabled={busy} onClick={onForget}>Forget</button>
      </span>
    </div>
  );
}

function CodeRow({ code, now, busy, onWithdraw }: { code: TvCode; now: number; busy: boolean; onWithdraw(): void }) {
  const left = minutesLeft(code.expires_at_unix_ms, now);
  return (
    <div className="ds-tv-row is-code">
      <span className="ds-tv-copy">
        <strong>{code.label} — {code.state === "connecting" ? "connecting…" : "waiting for the TV"}</strong>
        <span>
          On the TV, open Astrolabe and type{" "}
          <code className="ds-tv-code">{codeEntry(code)}</code>
          {" "}· works once · {left === 0 ? "expiring" : `${left} min left`}
        </span>
      </span>
      <span className="ds-tv-acts">
        <button type="button" className="ds-btn ds-btn-quiet" disabled={busy} onClick={onWithdraw}>Withdraw</button>
      </span>
    </div>
  );
}
