/**
 * Whether a television is showing this screen yet — the whole answer, here.
 *
 * Signage has one word for hardware, and it is "screen": a screen is a real
 * screen, and the only question about it is whether a TV is showing it. This
 * row is every TV fact that bears on that question, each one a chip, each
 * chip one press: with no TV, pressing gets a code to type on one; while the
 * code waits, the chip is the code; once a TV has connected, the chip is its
 * state, and pressing it offers a fresh code or a disconnect. A TV asking to
 * connect by words shows its words right here — the press is the answer —
 * and an enrolled TV showing nothing offers itself to this screen. When the
 * host cannot be asked, the chip says so: that is a different absence from
 * "no TV yet", and folding them together is the false-disconnection defect.
 */

import { useEffect, useMemo, useState } from "react";
import { Tv, X } from "lucide-react";
import { ChoiceMenu, haptic, useToast } from "@/ds";
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
  type TvFleet,
} from "@/utils/tv/api";

export function ScreenTv({
  screenId,
  screenName,
  screenIds,
  fleet,
  error,
  refresh,
}: {
  screenId: string;
  screenName: string;
  /** Every screen that exists, so a TV showing a removed one reads as free. */
  screenIds: string[];
  /** The fleet the page holds — one poll, shared with the stage. */
  fleet: TvFleet | null;
  error: string | null;
  refresh: () => Promise<void>;
}) {
  const toast = useToast();
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
  const asking = fleet?.pairings ?? [];
  const free = useMemo(
    () =>
      (fleet?.receivers ?? []).filter((tv) => {
        const shown = screenOf(tv.assignment?.input);
        return shown === null || !screenIds.includes(shown);
      }),
    [fleet, screenIds],
  );

  // Until the host has answered, the fact is unknown, and an unknown is
  // absent — never "No TV yet". A host that *refused* is a third thing,
  // and it says so below.
  if (fleet === null && error === null) return null;

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

  if (fleet === null) {
    return (
      <button
        type="button"
        className="ds-tuned ds-tvchip is-warn"
        title={error ?? undefined}
        onClick={() => toast.show("TVs could not be asked", error ?? "No answer from the host.")}
      >
        <Tv size={14} />
        <i className="ds-tvchip-dot" aria-hidden />
        TVs can't be asked
      </button>
    );
  }

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
              {
                id: "recode",
                label: "Get a new code",
                hint: "Disconnects this TV and mints a code to type on the next one",
                disabled: busy,
              },
              { id: "forget", label: "Disconnect", hint: "It will ask for a code again", danger: true, disabled: busy },
            ]}
            onPick={(picked) =>
              picked === "recode"
                ? void act("Could not get a new code", async () => {
                    await forgetTv(tv.device);
                    await mintTvCode(screenId, screenName);
                  })
                : void act("Could not disconnect the TV", () => forgetTv(tv.device))
            }
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

      {asking.map((pairing) => (
        <ChoiceMenu
          key={pairing.pairing}
          label={`A ${platformName(pairing.platform)} TV is asking to connect`}
          className="ds-tuned ds-tvchip is-warn"
          align="start"
          items={[
            {
              id: "approve",
              label: "It shows these same words — it's this screen",
              hint: "Different words mean a different TV. Leave it for its own page.",
              disabled: busy,
            },
            { id: "reject", label: "Not mine", danger: true, disabled: busy },
          ]}
          onPick={(picked) =>
            picked === "approve"
              ? void act("Could not add the TV", () => approveTvPairing(pairing.pairing, screenName, screenId))
              : void act("Could not turn the TV away", () => rejectTvPairing(pairing.pairing))
          }
        >
          <Tv size={14} />
          <i className="ds-tvchip-dot" aria-hidden />
          A TV asks: <em className="ds-tv-words">{pairing.confirmation_phrase.join(" ")}</em>
        </ChoiceMenu>
      ))}

      {free.map((tv) => (
        <ChoiceMenu
          key={tv.device}
          label={`${tv.label} shows no screen`}
          className="ds-tuned ds-tvchip is-absent"
          items={[
            { id: "assign", label: `Show this screen on ${tv.label}`, disabled: busy },
          ]}
          onPick={() => void act("Could not attach the TV", () => assignTv(tv.device, screenId))}
        >
          <Tv size={14} />
          {tv.label} is free
        </ChoiceMenu>
      ))}

      {showing.length === 0 && waiting.length === 0 && asking.length === 0 && free.length === 0 && (
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
