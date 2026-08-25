/**
 * One panel: what is true of it, what somebody calls it, and what it is
 * showing right now — with the reason.
 *
 * The page is divided the way the model is. **Place** and **Facts** are true of
 * the screen and are the only things here that can make a card correct or
 * wrong. **Labels** are the operator's vocabulary and mean nothing to the
 * substrate. **Tuning** is what it falls back to when nothing is being
 * broadcast at it. Nothing on this page assigns a program to a panel, because
 * that wire is what made addressing a fleet expensive.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { ArrowLeft, Trash2, X } from "lucide-react";
import { Confirm, Page, haptic, useToast } from "@/ds";
import { deleteScreen, fetchScreenPlays, saveScreen } from "@/utils/screens/api";
import { explain } from "@/utils/lait/resolve";
import { KIND_PANELS } from "@/program-editor/kinds/registry";
import type {
  Playback,
  SignageChannel,
  SignageProgram,
  SignageScreen,
} from "@/utils/lait/types";

export default function ScreenDetail({ screenId }: { screenId: string }) {
  const navigate = useNavigate();
  const toast = useToast();
  const [screen, setScreen] = useState<SignageScreen | null>(null);
  const [channels, setChannels] = useState<SignageChannel[]>([]);
  const [programs, setPrograms] = useState<SignageProgram[]>([]);
  const [playback, setPlayback] = useState<Playback | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [removing, setRemoving] = useState(false);
  const [label, setLabel] = useState("");

  const reload = useCallback(async () => {
    try {
      setError(null);
      const { inputs, playback } = await fetchScreenPlays(screenId);
      setScreen(inputs.screen);
      setChannels(inputs.channels);
      setPrograms(inputs.programs);
      setPlayback(playback);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not load this screen");
    } finally {
      setLoading(false);
    }
  }, [screenId]);

  useEffect(() => {
    void reload();
  }, [reload]);

  const commit = useCallback(
    async (next: SignageScreen) => {
      setScreen(next);
      try {
        await saveScreen(next);
        haptic("save");
        await reload();
      } catch (err) {
        toast.show("Could not save", err instanceof Error ? err.message : String(err));
        await reload();
      }
    },
    [reload, toast],
  );

  const showingName = useMemo(() => {
    if (playback?.showing.showing !== "program") return undefined;
    const id = playback.showing.program;
    return programs.find((program) => program.id === id)?.name;
  }, [playback, programs]);

  if (loading) return <Page>Loading…</Page>;
  if (!screen) {
    return (
      <Page>
        <p className="ds-danger-text">{error ?? "That screen is not here."}</p>
      </Page>
    );
  }

  const place = screen.place;

  return (
    <Page>
      <header className="ds-detail-bar">
        <button
          type="button"
          className="ds-icon"
          aria-label="Back"
          onClick={() => void navigate({ to: "/screen-list" })}
        >
          <ArrowLeft size={20} />
        </button>
        <input
          className="ds-detail-title"
          value={screen.name}
          aria-label="Screen name"
          onChange={(event) => setScreen({ ...screen, name: event.target.value })}
          onBlur={() => void commit(screen)}
        />
        <button
          type="button"
          className="ds-btn ds-btn-quiet is-danger"
          onClick={() => setRemoving(true)}
        >
          <Trash2 size={15} />
          Remove
        </button>
      </header>

      {error ? <p className="ds-danger-text">{error}</p> : null}

      {/* What it is doing, and why. The sentence an operator needs when a
          panel is showing the wrong thing — or nothing. */}
      <section className="ds-panel">
        <h3>Right now</h3>
        <p className="ds-showing">{playback ? explain(playback, showingName) : "Unknown"}</p>
        {playback?.showing.showing === "unaddressed" && !screen.tuned ? (
          <p className="ds-hint">
            Nothing reaches this screen. Tune it to a channel below, or address a
            broadcast at it.
          </p>
        ) : null}
      </section>

      <section className="ds-panel">
        <h3>Tuned to</h3>
        <select
          className="ds-input"
          value={screen.tuned ?? ""}
          onChange={(event) =>
            void commit({ ...screen, tuned: event.target.value || null })
          }
        >
          <option value="">Nothing</option>
          {channels.map((channel) => (
            <option key={channel.id} value={channel.id}>
              {channel.name}
            </option>
          ))}
        </select>
        <p className="ds-hint">
          What it shows when no broadcast is addressed at it.
        </p>
      </section>

      {/* Facts. The only fields here that can make a card correct or wrong. */}
      <section className="ds-panel">
        <h3>Place</h3>
        <p className="ds-hint">
          Where the panel physically is. Kinds that compute from a location —
          prayer times, weather, a local clock — read this and nothing else.
        </p>
        <div className="ds-place-grid">
          <label className="ds-field">
            <span>Latitude</span>
            <input
              className="ds-input"
              inputMode="decimal"
              placeholder="51.5074"
              value={place?.latitude ?? ""}
              onChange={(event) =>
                setScreen({
                  ...screen,
                  place: {
                    latitude: Number(event.target.value),
                    longitude: place?.longitude ?? 0,
                    timezone: place?.timezone ?? "",
                    region: place?.region ?? null,
                  },
                })
              }
              onBlur={() => void commit(screen)}
            />
          </label>
          <label className="ds-field">
            <span>Longitude</span>
            <input
              className="ds-input"
              inputMode="decimal"
              placeholder="-0.1278"
              value={place?.longitude ?? ""}
              onChange={(event) =>
                setScreen({
                  ...screen,
                  place: {
                    latitude: place?.latitude ?? 0,
                    longitude: Number(event.target.value),
                    timezone: place?.timezone ?? "",
                    region: place?.region ?? null,
                  },
                })
              }
              onBlur={() => void commit(screen)}
            />
          </label>
        </div>
        <label className="ds-field">
          <span>Time zone</span>
          <input
            className="ds-input"
            list="ds-zones"
            placeholder="Europe/London"
            value={place?.timezone ?? ""}
            onChange={(event) =>
              setScreen({
                ...screen,
                place: {
                  latitude: place?.latitude ?? 0,
                  longitude: place?.longitude ?? 0,
                  timezone: event.target.value,
                  region: place?.region ?? null,
                },
              })
            }
            onBlur={() => void commit(screen)}
          />
        </label>
        <ZoneOptions />
        <label className="ds-field">
          <span>Region</span>
          <input
            className="ds-input"
            placeholder="MI"
            value={place?.region ?? ""}
            onChange={(event) =>
              setScreen({
                ...screen,
                place: {
                  latitude: place?.latitude ?? 0,
                  longitude: place?.longitude ?? 0,
                  timezone: place?.timezone ?? "",
                  region: event.target.value || null,
                },
              })
            }
            onBlur={() => void commit(screen)}
          />
          <small className="ds-hint">
            So an audience can say &ldquo;every screen in Michigan&rdquo; without
            anybody maintaining a label that drifts from the coordinates above.
          </small>
        </label>
      </section>

      {KIND_PANELS.map((panel) => (
        <section className="ds-panel" key={panel.kind}>
          <h3>{panel.label} at this venue</h3>
          <p className="ds-hint">
            What this congregation practises, as distinct from how the card
            looks. Two venues under one operator can differ here and share a
            preset.
          </p>
          {panel.groups
            .flatMap((group) => group.fields)
            .filter((field) => field.control !== "place")
            .map((field, index) => {
              const key = "key" in field ? field.key : `${panel.kind}-${index}`;
              if (!("key" in field)) return null;
              const held = screen.facts?.[panel.kind]?.[field.key] ?? "";
              return (
                <label className="ds-field" key={key}>
                  <span>{field.label}</span>
                  <input
                    className="ds-input"
                    placeholder="from the preset"
                    value={held}
                    onChange={(event) => {
                      const kindFacts = {
                        ...(screen.facts?.[panel.kind] ?? {}),
                      };
                      if (event.target.value) kindFacts[field.key] = event.target.value;
                      else delete kindFacts[field.key];
                      setScreen({
                        ...screen,
                        facts: { ...(screen.facts ?? {}), [panel.kind]: kindFacts },
                      });
                    }}
                    onBlur={() => void commit(screen)}
                  />
                </label>
              );
            })}
        </section>
      ))}

      {/* Abstraction. Overlapping and arbitrary on purpose. */}
      <section className="ds-panel">
        <h3>Labels</h3>
        <p className="ds-hint">
          Yours. Overlapping and arbitrary — a screen can be
          <code> biz:acme</code>, <code>role:menu</code> and <code>rented</code> at
          once, and audiences address whichever slice matters today.
        </p>
        <div className="ds-label-row">
          {(screen.labels ?? []).map((held) => (
            <span className="ds-label" key={held}>
              {held}
              <button
                type="button"
                aria-label={`Remove ${held}`}
                onClick={() =>
                  void commit({
                    ...screen,
                    labels: (screen.labels ?? []).filter((other) => other !== held),
                  })
                }
              >
                <X size={12} />
              </button>
            </span>
          ))}
        </div>
        <form
          className="ds-label-add"
          onSubmit={(event) => {
            event.preventDefault();
            const next = label.trim();
            if (!next) return;
            void commit({
              ...screen,
              labels: [...new Set([...(screen.labels ?? []), next])].sort(),
            });
            setLabel("");
          }}
        >
          <input
            className="ds-input"
            value={label}
            placeholder="role:menu"
            onChange={(event) => setLabel(event.target.value)}
          />
          <button type="submit" className="ds-btn ds-btn-quiet">
            Add label
          </button>
        </form>
      </section>

      <Confirm
        open={removing}
        onOpenChange={setRemoving}
        title={`Remove ${screen.name}?`}
        description="The panel stops being addressable. Pairing and grants are Astrolabe's and are not touched here."
        confirmLabel="Remove"
        danger
        onConfirm={() => {
          void deleteScreen(screen.id)
            .then(() => navigate({ to: "/screen-list" }))
            .catch((err: unknown) =>
              toast.show("Could not remove", err instanceof Error ? err.message : String(err)),
            );
        }}
      />
    </Page>
  );
}

/** Whatever this browser can enumerate, offered as completions. */
function ZoneOptions() {
  const zones =
    typeof Intl.supportedValuesOf === "function" ? Intl.supportedValuesOf("timeZone") : [];
  if (zones.length === 0) return null;
  return (
    <datalist id="ds-zones">
      {zones.map((zone) => (
        <option key={zone} value={zone} />
      ))}
    </datalist>
  );
}
