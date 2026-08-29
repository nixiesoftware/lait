/**
 * The fleet.
 *
 * A row per panel: drawn as a screen showing what it shows, named, placed,
 * and wearing the channel it is tuned to as a control you can press. Tuning is
 * two gestures — open, pick — and the pick is the outcome: the row changes on
 * the same frame, and the World is told afterwards. Removing is one press,
 * and for a few seconds afterwards the bar at the foot of the page offers to
 * put it back. Nothing asks "are you sure".
 *
 * Filtering is by label, because labels are the operator's own vocabulary and
 * a fleet is sliced differently depending on the day. There is no group
 * column, because a screen belonging to exactly one set is the assumption
 * this product removed.
 */

import { useMemo, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { MapPin, Monitor, Plus, Tv } from "lucide-react";
import { screenOf, tvStatus, useTvs } from "@/utils/tv/api";
import { UnlinkedTvs } from "./UnlinkedTvs";
import {
  Bezel,
  ChoiceMenu,
  Empty,
  ItemMenu,
  MoreMenu,
  Page,
  PageHeader,
  PageSearch,
  PageStatus,
  haptic,
  useLive,
  useOrbit,
  useToast,
  useUndo,
  type MenuItem,
} from "@/ds";
import { putScreen, removeScreen, tune, useFleet } from "@/utils/screens/fleet";
import { mintBodyId } from "@/utils/lait/ids";
import type { SignageScreen } from "@/utils/lait/types";

export default function ScreenList() {
  const { fleet: tvs } = useTvs();
  const tvSummary = (screenId: string): { label: string; tone: string } | null => {
    const mine = (tvs?.receivers ?? []).filter((tv) => screenOf(tv.assignment?.input) === screenId);
    if (mine.length === 0) return null;
    const rank = { crit: 3, warn: 2, neutral: 1, good: 0 } as const;
    const worst = mine.map((tv) => tvStatus(tv)).sort((a, b) => rank[b.tone] - rank[a.tone])[0];
    const count = mine.length === 1 ? "1 TV" : `${mine.length} TVs`;
    return { label: worst.tone === "good" ? count : `${count} · ${worst.label}`, tone: worst.tone };
  };
  const navigate = useNavigate();
  const toast = useToast();
  const undo = useUndo();
  const orbit = useOrbit();
  const { now } = useLive();
  const fleet = useFleet();
  const { screens, channels, loading, error } = fleet;
  const [query, setQuery] = useState("");
  const [label, setLabel] = useState<string | null>(null);

  /** Every label anybody has used, so the filter offers what exists. */
  const labels = useMemo(
    () => [...new Set(screens.flatMap((screen) => screen.labels ?? []))].sort(),
    [screens],
  );

  const shown = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return screens
      .filter((screen) => {
        if (label && !(screen.labels ?? []).includes(label)) return false;
        if (!needle) return true;
        return (
          screen.name.toLowerCase().includes(needle) ||
          (screen.labels ?? []).some((held) => held.toLowerCase().includes(needle)) ||
          (screen.place?.region ?? "").toLowerCase().includes(needle)
        );
      })
      .sort((a, b) => a.name.localeCompare(b.name));
  }, [screens, query, label]);

  const open = (screen: SignageScreen) =>
    void navigate({ to: "/screen-list/$id", params: { id: screen.id } });

  const refused = (what: string) => (err: unknown) => {
    haptic("error");
    toast.show(what, err instanceof Error ? err.message : String(err));
  };

  /**
   * One press makes a screen. It exists on this frame, under a name the page
   * hands it, and the person lands on it with the name selected — renaming is
   * the next keystroke, not a dialog before the first.
   */
  const create = () => {
    const screen: SignageScreen = {
      id: mintBodyId(),
      name: "New screen",
      place: null,
      facts: {},
      sync: null,
      labels: [],
      tuned: null,
    };
    void putScreen(screen).catch(refused("Could not add a screen"));
    haptic("save");
    open(screen);
  };

  const remove = (screen: SignageScreen) => {
    haptic("delete");
    void removeScreen(screen.id)
      .then((was) => {
        if (was) undo.offer(`Removed ${was.name}`, () => putScreen(was));
      })
      .catch(refused("Could not remove"));
  };

  const retune = (screen: SignageScreen, channel: string | null) => {
    haptic("select");
    void tune(screen.id, channel).catch(refused("Could not tune"));
  };

  const menuFor = (screen: SignageScreen): MenuItem[] => [
    { label: "Open", onPick: () => open(screen) },
    ...channels.map((channel) => ({
      label: `Tune to ${channel.name}`,
      disabled: screen.tuned === channel.id,
      onPick: () => retune(screen, channel.id),
    })),
    { label: "Untune", disabled: !screen.tuned, onPick: () => retune(screen, null) },
    { label: "Remove", danger: true, onPick: () => remove(screen) },
  ];

  return (
    <Page>
      <PageHeader title="Screens" icon={<Monitor size={20} />}>
        <button type="button" className="ds-btn ds-btn-solid" onClick={create}>
          <Plus size={16} />
          New screen
        </button>
      </PageHeader>

      {screens.length > 0 && (
        <PageSearch value={query} onChange={setQuery} placeholder="Filter screens…" />
      )}

      {labels.length > 0 && (
        <div className="ds-chips" role="tablist" style={{ marginBottom: 16 }}>
          <button
            type="button"
            role="tab"
            aria-selected={label === null}
            className={`ds-chip${label === null ? " is-on" : ""}`}
            onClick={() => setLabel(null)}
          >
            All <em>{screens.length}</em>
          </button>
          {labels.map((held) => (
            <button
              type="button"
              key={held}
              role="tab"
              aria-selected={label === held}
              className={`ds-chip${label === held ? " is-on" : ""}`}
              onClick={() => setLabel(label === held ? null : held)}
            >
              {held}
              <em>{screens.filter((screen) => (screen.labels ?? []).includes(held)).length}</em>
            </button>
          ))}
        </div>
      )}

      <PageStatus loading={loading} error={error ?? ""} />

      <UnlinkedTvs screens={screens} />

      {!loading && shown.length === 0 ? (
        <Empty title={screens.length === 0 ? "No screens yet" : "Nothing matches"}>
          <p className="ds-hint">
            {screens.length === 0
              ? "A screen appears here once it is paired, or you can add one now and pair it later."
              : "No screen carries that label."}
          </p>
          {screens.length === 0 && (
            <button type="button" className="ds-btn ds-btn-solid" onClick={create}>
              <Plus size={16} />
              New screen
            </button>
          )}
        </Empty>
      ) : (
        <div className="ds-device-list">
          {shown.map((screen) => {
            const channel = channels.find((entry) => entry.id === screen.tuned) ?? null;
            return (
              <ItemMenu key={screen.id} items={menuFor(screen)} className="ds-device">
                <button
                  type="button"
                  className="ds-attached-hit"
                  aria-label={`Open ${screen.name}`}
                  onClick={() => open(screen)}
                >
                  <Bezel
                    size="sm"
                    screen={screen}
                    playback={fleet.playbackFor(screen, now)}
                    programs={fleet.programs}
                    media={fleet.media}
                    presets={fleet.presets}
                    orbit={orbit}
                    now={now}
                  />
                </button>
                <button type="button" className="ds-row-hit" onClick={() => open(screen)}>
                  <span className="ds-row-copy">
                    <strong>{screen.name}</strong>
                    <span>
                      <MapPin size={12} />
                      {screen.place ? (
                        screen.place.region ?? screen.place.timezone
                      ) : (
                        <span style={{ color: "var(--ds-miss)" }}>Not placed</span>
                      )}
                      {(screen.labels ?? []).length > 0 && (
                        <span className="ds-device-tags">
                          {(screen.labels ?? []).map((held) => (
                            <span className="ds-tag" key={held}>
                              {held}
                            </span>
                          ))}
                        </span>
                      )}
                      {tvSummary(screen.id) && (
                        <span className={`ds-tag ds-tv-count is-${tvSummary(screen.id)!.tone}`}>
                          <Tv size={11} />
                          {tvSummary(screen.id)!.label}
                        </span>
                      )}
                    </span>
                  </span>
                </button>
                <ChoiceMenu
                  label={`Tune ${screen.name}`}
                  className={`ds-tuned${channel ? "" : " is-absent"}`}
                  align="end"
                  items={[
                    ...channels.map((entry) => ({
                      id: entry.id,
                      label: entry.name,
                      hint: `${fleet.tunedTo(entry.id).length} tuned`,
                      on: entry.id === screen.tuned,
                    })),
                    { id: "", label: "Nothing", hint: "Untuned", on: !screen.tuned, danger: !!screen.tuned },
                  ]}
                  onPick={(id) => retune(screen, id || null)}
                >
                  <Tv size={14} />
                  {channel ? channel.name : "Not tuned"}
                </ChoiceMenu>
                <MoreMenu
                  items={[
                    { label: "Open", onPick: () => open(screen) },
                    { label: "Remove", danger: true, onPick: () => remove(screen) },
                  ]}
                />
              </ItemMenu>
            );
          })}
        </div>
      )}
    </Page>
  );
}
