/**
 * The television(s) that are this screen, as chips on the horizon.
 *
 * A Signage screen is a real screen, so the hero states the hardware that is
 * it beside its other facts — place, tuning, labels. Each TV is a chip that
 * carries its state; pressing it offers to detach or forget it. A code waiting
 * for a TV is a chip too, showing the words to type, with an × to withdraw.
 * "+ TV" becomes a name field when pressed, exactly as "+ label" does, and
 * Enter asks for a code. Nothing else appears here: a TV that is idle, or one
 * asking to connect by words, is offered a screen on the Screens page.
 */

import { useEffect, useMemo, useState } from "react";
import { Plus, Tv, X } from "lucide-react";
import { ChoiceMenu, haptic, useToast } from "@/ds";
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
} from "@/utils/tv/api";

export function TvChips({ screenId, screenName }: { screenId: string; screenName: string }) {
  const toast = useToast();
  const { fleet, error, refresh } = useTvs();
  const [adding, setAdding] = useState(false);
  const [draft, setDraft] = useState("");
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

  const mint = () => {
    const label = draft.trim() || (mine.length + codes.length === 0 ? screenName : `${screenName} ${mine.length + codes.length + 1}`);
    setDraft("");
    setAdding(false);
    void act("Could not get a code", () => mintTvCode(screenId, label));
  };

  return (
    <div className="ds-labels ds-tvchips" title={error ? `Couldn't ask about TVs: ${error}` : undefined}>
      {mine.map((tv) => {
        const status = tvStatus(tv, now);
        return (
          <ChoiceMenu
            key={tv.device}
            label={`${tv.label} — ${platformName(tv.platform)}, ${status.label}`}
            className={`ds-tuned ds-tvchip is-${status.tone}`}
            align="start"
            items={[
              { id: "detach", label: "Detach", hint: "Stays linked, shows nothing", disabled: busy },
              { id: "forget", label: "Forget", hint: "It will have to connect again", danger: true, disabled: busy },
            ]}
            onPick={(id) =>
              void (id === "detach"
                ? act("Could not take the TV off", () => unassignTv(tv.device))
                : act("Could not forget the TV", () => forgetTv(tv.device)))
            }
          >
            <Tv size={14} />
            {tv.label}
            <i className="ds-tvchip-dot" aria-hidden />
            <span className="ds-tvchip-state">{status.label}</span>
          </ChoiceMenu>
        );
      })}

      {codes.map((code) => {
        const left = minutesLeft(code.expires_at_unix_ms, now);
        return (
          <span
            key={code.rendezvous}
            className="ds-tuned ds-tvchip is-code"
            title={`${code.label}: on the TV, open Astrolabe and type this. It works once.`}
          >
            <Tv size={14} />
            <code className="ds-tv-code">{codeEntry(code)}</code>
            <span className="ds-tvchip-state">
              {code.state === "connecting" ? "connecting…" : left === 0 ? "expiring" : `${left} min`}
            </span>
            <button
              type="button"
              className="ds-tag-x"
              aria-label={`Withdraw the code for ${code.label}`}
              disabled={busy}
              onClick={() => void act("Could not withdraw the code", () => revokeTvCode(code.rendezvous))}
            >
              <X size={11} />
            </button>
          </span>
        );
      })}

      {mine.length === 0 && codes.length === 0 && !adding && (
        <span className="ds-tvchip-none">No TV is this screen yet</span>
      )}

      {adding ? (
        <input
          className="ds-tag-input"
          value={draft}
          placeholder={screenName}
          aria-label="TV name"
          autoFocus
          onChange={(event) => setDraft(event.target.value)}
          onBlur={() => { setDraft(""); setAdding(false); }}
          onKeyDown={(event) => {
            if (event.key === "Enter") {
              event.preventDefault();
              mint();
            } else if (event.key === "Escape") {
              setDraft("");
              setAdding(false);
            }
          }}
        />
      ) : (
        <button
          type="button"
          className="ds-tag-add"
          disabled={busy}
          title="Name the TV and press Enter for a code to type on it"
          onClick={() => setAdding(true)}
        >
          <Plus size={12} />
          TV
        </button>
      )}
    </div>
  );
}
