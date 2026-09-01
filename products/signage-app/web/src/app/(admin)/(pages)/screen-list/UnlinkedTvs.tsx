/**
 * Televisions that are no screen.
 *
 * Two kinds arrive here: a TV asking to connect by words — somebody at a
 * television typed this site's name — and a TV that shows no screen, because
 * it was linked without one or the screen it showed was removed. Each is one
 * question, asked once, on the page that lists screens: which screen is
 * this? Answering makes it that screen, and it leaves this list.
 */

import { Tv } from "lucide-react";
import { ChoiceMenu, haptic, useToast } from "@/ds";
import { approveTvPairing, assignTv, platformName, rejectTvPairing, screenOf, useTvs } from "@/utils/tv/api";
import type { SignageScreen } from "@/utils/lait/types";

export function UnlinkedTvs({ screens }: { screens: SignageScreen[] }) {
  const toast = useToast();
  const { fleet, error, refresh } = useTvs();
  const known = new Set(screens.map((screen) => screen.id));
  const idle = (fleet?.receivers ?? []).filter((tv) => {
    const shown = screenOf(tv.assignment?.input);
    return shown === null || !known.has(shown);
  });
  const pairings = fleet?.pairings ?? [];

  // A host that could not be asked is not a host with nothing to say.
  if (fleet === null && error !== null) {
    return (
      <div className="ds-tvs" style={{ marginBottom: 16 }}>
        <div className="ds-tv-row">
          <span className="ds-tv-copy">
            <strong>TVs could not be asked</strong>
            <span>{error}</span>
          </span>
        </div>
      </div>
    );
  }
  if (idle.length === 0 && pairings.length === 0) return null;

  const items = screens.map((screen) => ({ id: screen.id, label: screen.name }));
  const act = async (what: string, work: () => Promise<unknown>) => {
    try {
      await work();
      haptic("save");
      await refresh();
    } catch (err) {
      haptic("error");
      toast.show(what, err instanceof Error ? err.message : String(err));
    }
  };
  const nameFor = (screenId: string) => screens.find((screen) => screen.id === screenId)?.name ?? "TV";

  return (
    <div className="ds-tvs" style={{ marginBottom: 16 }}>
      {pairings.map((pairing) => (
        <div className="ds-tv-row is-asking" key={pairing.pairing}>
          <span className="ds-tv-copy">
            <strong>A {platformName(pairing.platform)} TV is asking to connect</strong>
            <span>
              It shows these words — the same words mean it is the one in front of you:{" "}
              <em className="ds-tv-words">{pairing.confirmation_phrase.join(" ")}</em>
            </span>
          </span>
          <span className="ds-tv-acts">
            <ChoiceMenu label="Which screen is it?" className="ds-btn ds-btn-solid" items={items} align="end"
              onPick={(screenId) => void act("Could not add the TV", () => approveTvPairing(pairing.pairing, nameFor(screenId), screenId))}>
              <Tv size={14} />
              It's this screen…
            </ChoiceMenu>
            <button type="button" className="ds-btn ds-btn-quiet"
              onClick={() => void act("Could not turn the TV away", () => rejectTvPairing(pairing.pairing))}>
              Not mine
            </button>
          </span>
        </div>
      ))}
      {idle.map((tv) => (
        <div className="ds-tv-row is-free" key={tv.device}>
          <span className="ds-tv-copy">
            <strong>{tv.label}</strong>
            <span>{platformName(tv.platform)} · shows no screen</span>
          </span>
          <span className="ds-tv-acts">
            <ChoiceMenu label={`Which screen is ${tv.label}?`} className="ds-btn" items={items} align="end"
              onPick={(screenId) => void act("Could not attach the TV", () => assignTv(tv.device, screenId))}>
              <Tv size={14} />
              It's this screen…
            </ChoiceMenu>
          </span>
        </div>
      ))}
    </div>
  );
}
