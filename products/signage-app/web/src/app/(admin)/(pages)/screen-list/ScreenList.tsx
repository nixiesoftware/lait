/**
 * The fleet.
 *
 * A row per panel: what it is called, where it is, what it is tuned to, and
 * what it is showing right now. Filtering is by label, because labels are the
 * operator's own vocabulary and a fleet is sliced differently depending on the
 * day — by venue on Monday, by role during a sale, by everything at once in an
 * emergency. There is no group column, because a screen belonging to exactly
 * one set is the assumption this product removed.
 */

import { useMemo, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { Monitor, Plus } from "lucide-react";
import {
  Bezel,
  Confirm,
  DeviceRow,
  Empty,
  Page,
  PageHeader,
  PageSearch,
  PageStatus,
  Prompt,
  haptic,
  useLive,
  useOrbit,
  useToast,
  type MenuItem,
} from "@/ds";
import { createScreen, deleteScreen, tuneScreen } from "@/utils/screens/api";
import { useFleet } from "@/utils/screens/fleet";
import type { SignageScreen } from "@/utils/lait/types";

export default function ScreenList() {
  const navigate = useNavigate();
  const toast = useToast();
  const orbit = useOrbit();
  const { now } = useLive();
  const fleet = useFleet();
  const { screens, channels, loading, error, reload } = fleet;
  const [query, setQuery] = useState("");
  const [label, setLabel] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<SignageScreen | null>(null);


  /** Every label anybody has used, so the filter offers what exists. */
  const labels = useMemo(
    () => [...new Set(screens.flatMap((screen) => screen.labels ?? []))].sort(),
    [screens],
  );

  const shown = useMemo(() => {
    const needle = query.trim().toLowerCase();
    return screens.filter((screen) => {
      if (label && !(screen.labels ?? []).includes(label)) return false;
      if (!needle) return true;
      return (
        screen.name.toLowerCase().includes(needle) ||
        (screen.labels ?? []).some((held) => held.toLowerCase().includes(needle)) ||
        (screen.place?.region ?? "").toLowerCase().includes(needle)
      );
    });
  }, [screens, query, label]);

  const channelName = (id: string | null | undefined) =>
    channels.find((channel) => channel.id === id)?.name ?? null;

  const describe = (screen: SignageScreen) => {
    const parts: string[] = [];
    const tuned = channelName(screen.tuned);
    // Not tuned is a state, not a blank. It is the first thing to fix on a
    // panel that is showing nothing, and it should say so.
    parts.push(tuned ? `Tuned to ${tuned}` : "Not tuned");
    if (screen.place) {
      parts.push(screen.place.region ?? screen.place.timezone);
    } else {
      parts.push("No location");
    }
    const held = screen.labels ?? [];
    if (held.length > 0) parts.push(held.join(" · "));
    return parts.join(" — ");
  };

  const menuFor = (screen: SignageScreen): MenuItem[] => [
    ...channels.map((channel) => ({
      label: `Tune to ${channel.name}`,
      onPick: () => {
        void tuneScreen(screen.id, channel.id)
          .then(reload)
          .then(() => haptic("save"))
          .catch((err: unknown) =>
            toast.show("Could not tune", err instanceof Error ? err.message : String(err)),
          );
      },
    })),
    {
      label: "Untune",
      disabled: !screen.tuned,
      onPick: () => {
        void tuneScreen(screen.id, null).then(reload);
      },
    },
    {
      label: "Remove",
      danger: true,
      onPick: () => setRemoving(screen),
    },
  ];

  return (
    <Page>
      <PageHeader title="Screens" icon={<Monitor size={20} />}>
        <button
          type="button"
          className="ds-btn ds-btn-solid"
          onClick={() => setAdding(true)}
        >
          <Plus size={16} />
          New screen
        </button>
      </PageHeader>
      <PageSearch value={query} onChange={setQuery} placeholder="Filter screens…" />

      {labels.length > 0 && (
        <div className="ds-chips" role="tablist">
          <button
            type="button"
            role="tab"
            aria-selected={label === null}
            className={`ds-chip${label === null ? " is-on" : ""}`}
            onClick={() => setLabel(null)}
          >
            All {screens.length}
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
            </button>
          ))}
        </div>
      )}

      <PageStatus loading={loading} error={error ?? ""} />

      {!loading && shown.length === 0 ? (
        <Empty title={screens.length === 0 ? "No screens yet" : "Nothing matches"}>
          <p className="ds-hint">
            {screens.length === 0
              ? "A screen appears here once it is paired, or you can add one now and pair it later."
              : "No screen carries that label."}
          </p>
        </Empty>
      ) : (
        <div className="ds-device-list">
          {shown.map((screen) => (
            <DeviceRow
              key={screen.id}
              name={screen.name}
              meta={describe(screen)}
              menu={menuFor(screen)}
              onOpen={() =>
                void navigate({
                  to: "/screen-list/$id",
                  params: { id: screen.id },
                })
              }
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
            </DeviceRow>
          ))}
        </div>
      )}

      <Prompt
        open={adding}
        onOpenChange={setAdding}
        title="New screen"
        label="Name"
        confirmLabel="Create"
        onSubmit={(name: string) => {
          void createScreen(name.trim() || "Screen")
            .then(reload)
            .then(() => haptic("save"))
            .catch((err: unknown) =>
              toast.show("Could not create", err instanceof Error ? err.message : String(err)),
            );
        }}
      />

      <Confirm
        open={removing != null}
        onOpenChange={(open) => {
          if (!open) setRemoving(null);
        }}
        title={`Remove ${removing?.name ?? "this screen"}?`}
        description="The panel stops being addressable. Pairing and grants are Astrolabe's and are not touched here."
        confirmLabel="Remove"
        danger
        onConfirm={() => {
          const screen = removing;
          if (!screen) return;
          void deleteScreen(screen.id)
            .then(reload)
            .then(() => haptic("delete"))
            .catch((err: unknown) =>
              toast.show("Could not remove", err instanceof Error ? err.message : String(err)),
            );
        }}
      />
    </Page>
  );
}
