/**
 * The televisions on one screen.
 *
 * A TV belongs to a screen, so this is where one is added, named, watched
 * and taken off — in Signage's words, with no World or surface to choose.
 * Adding one is a code shown here, big, that a person types on the television;
 * the row appears the moment it connects. A TV asking to connect by words is
 * offered to this screen. A TV nobody holds can be pointed here. Everything a
 * TV shows is decided elsewhere on this page; this section is only who is
 * watching.
 */

import { useEffect, useMemo, useState } from "react";
import { Plus, Tv, X } from "lucide-react";
import { haptic, useToast } from "@/ds";
import {
  approveTvPairing,
  assignTv,
  codeEntry,
  forgetTv,
  minutesLeft,
  mintTvCode,
  platformName,
  rejectTvPairing,
  revokeTvCode,
  screenOf,
  tvStatus,
  unassignTv,
  useTvs,
  type TvCode,
  type TvPairing,
  type TvReceiver,
} from "@/utils/tv/api";

export function TvSection({ screenId, screenName }: { screenId: string; screenName: string }) {
  const toast = useToast();
  const { fleet, error, refresh } = useTvs();
  const [naming, setNaming] = useState<null | "tv" | string>(null);
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
  const free = useMemo(() => (fleet?.receivers ?? []).filter((tv) => tv.assignment === null), [fleet]);
  const elsewhere = useMemo(
    () => (fleet?.receivers ?? []).filter((tv) => tv.assignment !== null && screenOf(tv.assignment.input) !== screenId),
    [fleet, screenId],
  );
  const codes = useMemo(
    () => (fleet?.codes ?? []).filter((code) => screenOf(code.input) === screenId && code.state !== "connected"),
    [fleet, screenId],
  );
  const pairings = fleet?.pairings ?? [];

  const refused = (what: string) => (err: unknown) => {
    haptic("error");
    toast.show(what, err instanceof Error ? err.message : String(err));
  };
  const act = async (what: string, work: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await work();
      haptic("save");
      await refresh();
    } catch (err) {
      refused(what)(err);
    } finally {
      setBusy(false);
    }
  };

  const mint = () =>
    act("Could not get a code", async () => {
      await mintTvCode(screenId, name.trim() || `${screenName} TV`);
      setNaming(null);
      setName("");
    });
  const approve = (pairing: TvPairing) =>
    act("Could not add the TV", async () => {
      await approveTvPairing(pairing.pairing, name.trim() || `${screenName} TV`, screenId);
      setNaming(null);
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

      {pairings.map((pairing) => (
        <div className="ds-tv-row is-asking" key={pairing.pairing}>
          <span className="ds-tv-copy">
            <strong>A {platformName(pairing.platform)} TV is asking to connect</strong>
            <span>
              Its screen shows these words — the same words mean it is this one:{" "}
              <em className="ds-tv-words">{pairing.confirmation_phrase.join(" ")}</em>
            </span>
          </span>
          <span className="ds-tv-acts">
            {naming === pairing.pairing ? (
              <NameField name={name} setName={setName} placeholder={`${screenName} TV`} busy={busy}
                onDone={() => void approve(pairing)} onCancel={() => setNaming(null)} verb="Add here" />
            ) : (
              <>
                <button type="button" className="ds-btn ds-btn-solid" disabled={busy} onClick={() => { setNaming(pairing.pairing); setName(""); }}>
                  It's this one
                </button>
                <button type="button" className="ds-btn ds-btn-quiet" disabled={busy}
                  onClick={() => void act("Could not turn the TV away", () => rejectTvPairing(pairing.pairing))}>
                  Not mine
                </button>
              </>
            )}
          </span>
        </div>
      ))}

      {free.map((tv) => (
        <div className="ds-tv-row is-free" key={tv.device}>
          <span className="ds-tv-copy">
            <strong>{tv.label}</strong>
            <span>{platformName(tv.platform)} · showing nothing yet</span>
          </span>
          <span className="ds-tv-acts">
            <button type="button" className="ds-btn" disabled={busy}
              onClick={() => void act("Could not point the TV here", () => assignTv(tv.device, screenId))}>
              Point it here
            </button>
          </span>
        </div>
      ))}

      {mine.length === 0 && codes.length === 0 && pairings.length === 0 && free.length === 0 && (
        <p className="ds-hint">No TV shows this screen yet. Add one: you'll get a code to type on the TV.</p>
      )}

      <div className="ds-page-actions">
        {naming === "tv" ? (
          <NameField name={name} setName={setName} placeholder={`${screenName} TV`} busy={busy}
            onDone={() => void mint()} onCancel={() => setNaming(null)} verb="Get a code" />
        ) : (
          <button type="button" className="ds-btn ds-btn-solid" disabled={busy} onClick={() => { setNaming("tv"); setName(""); }}>
            <Plus size={15} />
            Add a TV
          </button>
        )}
        {elsewhere.length > 0 && (
          <span className="ds-hint">
            {elsewhere.length === 1 ? "1 other TV shows" : `${elsewhere.length} other TVs show`} another screen; open that screen to move it.
          </span>
        )}
      </div>
    </div>
  );
}

function NameField({ name, setName, placeholder, busy, onDone, onCancel, verb }: {
  name: string; setName(next: string): void; placeholder: string; busy: boolean; onDone(): void; onCancel(): void; verb: string;
}) {
  return (
    <span className="ds-tv-name">
      <input className="ds-input" value={name} placeholder={placeholder} aria-label="TV name" autoFocus
        onChange={(event) => setName(event.target.value)}
        onKeyDown={(event) => { if (event.key === "Enter") onDone(); if (event.key === "Escape") onCancel(); }} />
      <button type="button" className="ds-btn ds-btn-solid" disabled={busy} onClick={onDone}>{verb}</button>
      <button type="button" className="ds-icon" aria-label="Cancel" onClick={onCancel}><X size={16} /></button>
    </span>
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
        <button type="button" className="ds-btn ds-btn-quiet" disabled={busy} onClick={onUnpoint}>Show nothing</button>
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
