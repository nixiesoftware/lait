/**
 * The empty screen draws its own on-screen display, and it says one thing.
 *
 * A real television with nothing to show draws its activation, center-glass,
 * and nothing else — configuration lives on the panel's edge buttons, not on
 * the picture. So while nothing is addressed to the screen, the glass
 * carries only the pairing fact: the one solid button when no TV shows it,
 * the code theater-size while one waits, the six words while a TV asks, the
 * TV's status once one is connected, and the amber could-not-be-asked
 * readout when the host will not answer. Everything else a person changes
 * about the screen is on the rail beside it. The moment anything reaches
 * the screen, the stage yields and the glass is content.
 */

import { useState } from "react";
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
import type { SignageScreen } from "@/utils/lait/types";

export function ScreenStage({
  screen,
  fleet,
  error,
  refresh,
  screenIds,
  now,
}: {
  screen: SignageScreen;
  fleet: TvFleet | null;
  error: string | null;
  refresh: () => Promise<void>;
  screenIds: string[];
  now: number;
}) {
  const toast = useToast();
  const [busy, setBusy] = useState(false);

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

  const showing = (fleet?.receivers ?? []).filter(
    (tv) => screenOf(tv.assignment?.input) === screen.id,
  );
  const waiting = (fleet?.codes ?? []).filter(
    (code) => screenOf(code.input) === screen.id && code.state !== "connected",
  );
  const asking = fleet?.pairings ?? [];
  const free = (fleet?.receivers ?? []).filter((tv) => {
    const shown = screenOf(tv.assignment?.input);
    return shown === null || !screenIds.includes(shown);
  });

  /** Center of the glass: where a TV draws its own activation. */
  const center = () => {
    // The host could not be asked — a different absence from "no TV yet",
    // said where the eye already is.
    if (fleet === null && error !== null) {
      return (
        <span className="ds-stage-center is-warn" role="status" title={error}>
          <Tv size={22} aria-hidden />
          <strong>TVs can't be asked</strong>
          <span>{error}</span>
        </span>
      );
    }
    // Not yet answered: an unknown is absent, never "No TV yet".
    if (fleet === null) return null;

    const code = waiting[0];
    if (code) {
      const left = minutesLeft(code.expires_at_unix_ms, now);
      return (
        <span className="ds-stage-center">
          {code.state === "connecting" ? (
            <strong>A TV is connecting…</strong>
          ) : (
            <>
              <span className="ds-stage-eyebrow">On the TV, open Astrolabe and type</span>
              <code className="ds-stage-code">{codeEntry(code)}</code>
              <span className="ds-stage-meta">
                {left === 0 ? "expiring" : `good for ${left} min`} · works once
                <button
                  type="button"
                  className="ds-btn ds-btn-quiet"
                  disabled={busy}
                  onClick={() =>
                    void act("Could not withdraw the code", () => revokeTvCode(code.rendezvous))
                  }
                >
                  <X size={12} aria-hidden />
                  Withdraw
                </button>
              </span>
            </>
          )}
        </span>
      );
    }

    const pairing = asking[0];
    if (pairing) {
      return (
        <span className="ds-stage-center">
          <span className="ds-stage-eyebrow">
            A {platformName(pairing.platform)} TV is asking to connect. It shows
          </span>
          <em className="ds-stage-words">{pairing.confirmation_phrase.join(" ")}</em>
          <span className="ds-stage-meta">
            <button
              type="button"
              className="ds-btn ds-btn-solid"
              disabled={busy}
              onClick={() =>
                void act("Could not add the TV", () =>
                  approveTvPairing(pairing.pairing, screen.name, screen.id),
                )
              }
            >
              Same words — it's this screen
            </button>
            <button
              type="button"
              className="ds-btn ds-btn-quiet"
              disabled={busy}
              onClick={() =>
                void act("Could not turn the TV away", () => rejectTvPairing(pairing.pairing))
              }
            >
              Not mine
            </button>
          </span>
        </span>
      );
    }

    const tv = showing[0];
    if (tv) {
      const status = tvStatus(tv, now);
      return (
        <span className="ds-stage-center">
          <ChoiceMenu
            label={`The ${platformName(tv.platform)} TV showing this screen: ${status.label}`}
            className={`ds-tuned ds-tvchip ds-stage-chip is-${status.tone}`}
            items={[
              {
                id: "recode",
                label: "Get a new code",
                hint: "Disconnects this TV and mints a code for the next one",
                disabled: busy,
              },
              { id: "forget", label: "Disconnect", hint: "It will ask for a code again", danger: true, disabled: busy },
            ]}
            onPick={(picked) =>
              picked === "recode"
                ? void act("Could not get a new code", async () => {
                    await forgetTv(tv.device);
                    await mintTvCode(screen.id, screen.name);
                  })
                : void act("Could not disconnect the TV", () => forgetTv(tv.device))
            }
          >
            <Tv size={16} aria-hidden />
            <i className="ds-tvchip-dot" aria-hidden />
            {status.label}
          </ChoiceMenu>
          <span>The {platformName(tv.platform)} TV showing this screen</span>
        </span>
      );
    }

    const idle = free[0];
    if (idle) {
      return (
        <span className="ds-stage-center">
          <span className="ds-stage-eyebrow">{idle.label} is free</span>
          <button
            type="button"
            className="ds-btn ds-btn-solid"
            disabled={busy}
            onClick={() =>
              void act("Could not attach the TV", () => assignTv(idle.device, screen.id))
            }
          >
            <Tv size={15} aria-hidden />
            Show this screen on it
          </button>
        </span>
      );
    }

    return (
      <span className="ds-stage-center">
        <button
          type="button"
          className="ds-btn ds-btn-solid"
          disabled={busy}
          onClick={() => void act("Could not get a code", () => mintTvCode(screen.id, screen.name))}
        >
          <Tv size={15} aria-hidden />
          Pair a TV
        </button>
      </span>
    );
  };

  return <span className="ds-stage">{center()}</span>;
}
