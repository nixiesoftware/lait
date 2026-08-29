/**
 * Whether a television is showing this screen yet.
 *
 * Signage has one word for hardware, and it is "screen": a screen is a real
 * screen, and the only question about it is whether a TV is showing it. So
 * this is one fact on the horizon, beside its place and its tuning. With no
 * TV, pressing it gets a code to type on one; while the code waits, the chip
 * is the code; once a TV has connected, the chip is its state, and pressing it
 * offers to disconnect. A TV that is asking to connect by words, or one that
 * shows no screen, is told which screen it is on the Screens page.
 */

import { useEffect, useMemo, useState } from "react";
import { Tv, X } from "lucide-react";
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
  useTvs,
} from "@/utils/tv/api";

export function ScreenTv({ screenId, screenName }: { screenId: string; screenName: string }) {
  const toast = useToast();
  const { fleet, refresh } = useTvs();
  const [busy, setBusy] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 15_000);
    return () => clearInterval(timer);
  }, []);

  const showing = useMemo(
    () => (fleet?.receivers ?? []).filter((tv) => screenOf(tv.assignment?.input) === screenId),
    [fleet, screenId],
  );
  const waiting = useMemo(
    () => (fleet?.codes ?? []).filter((code) => screenOf(code.input) === screenId && code.state !== "connected"),
    [fleet, screenId],
  );

  // Until the host has answered, the fact is unknown, and an unknown is
  // absent — never "No TV yet".
  if (fleet === null) return null;

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

  return (
    <>
      {showing.map((tv) => {
        const status = tvStatus(tv, now);
        return (
          <ChoiceMenu
            key={tv.device}
            label={`The ${platformName(tv.platform)} TV showing this screen: ${status.label}`}
            className={`ds-tuned ds-tvchip is-${status.tone}`}
            items={[
              { id: "forget", label: "Disconnect", hint: "It will ask for a code again", danger: true, disabled: busy },
            ]}
            onPick={() => void act("Could not disconnect the TV", () => forgetTv(tv.device))}
          >
            <Tv size={14} />
            <i className="ds-tvchip-dot" aria-hidden />
            {status.label}
          </ChoiceMenu>
        );
      })}

      {waiting.map((code) => {
        const left = minutesLeft(code.expires_at_unix_ms, now);
        return (
          <span
            key={code.rendezvous}
            className="ds-tuned ds-tvchip is-code"
            title="On the TV, open Astrolabe and type this. It works once."
          >
            <Tv size={14} />
            {code.state === "connecting" ? (
              "A TV is connecting…"
            ) : (
              <>
                Type <code className="ds-tv-code">{codeEntry(code)}</code> on the TV
                <span className="ds-tvchip-state">{left === 0 ? "expiring" : `${left} min`}</span>
              </>
            )}
            <button
              type="button"
              className="ds-tag-x"
              aria-label="Withdraw the code"
              disabled={busy}
              onClick={() => void act("Could not withdraw the code", () => revokeTvCode(code.rendezvous))}
            >
              <X size={11} />
            </button>
          </span>
        );
      })}

      {showing.length === 0 && waiting.length === 0 && (
        <button
          type="button"
          className="ds-tuned ds-tvchip is-absent"
          disabled={busy}
          title="Press for a code to type on the TV"
          onClick={() => void act("Could not get a code", () => mintTvCode(screenId, screenName))}
        >
          <Tv size={14} />
          No TV yet
        </button>
      )}
    </>
  );
}
