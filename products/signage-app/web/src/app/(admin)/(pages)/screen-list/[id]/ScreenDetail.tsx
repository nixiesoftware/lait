/**
 * One panel.
 *
 * Nothing on this page has a Save button, and nothing has an editing mode.
 * Every field commits itself and says so on itself. What that leaves reads as
 * a *description of a thing* rather than a form about it — which is what it
 * is, because a screen is somewhere, is called something, and is tuned to
 * something whether or not anybody is looking at this page.
 *
 * Divided the way the model is:
 *   **Right now** — resolved live, recomputed on the shared tick.
 *   **Place** and **Facts** — true of the panel, and the only fields here that
 *   can make a card correct or wrong.
 *   **Labels** — the operator's vocabulary, meaning nothing to the substrate.
 *   **Tuned** — the fallback when nothing is addressed at it.
 *   **As run** — what the panel says it actually played, in its own words.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { ArrowLeft, Trash2, X } from "lucide-react";
import {
  Ago,
  CommitText,
  Confirm,
  Field,
  OnAir,
  Page,
  useCommit,
  useLive,
  useRevision,
  useToast,
} from "@/ds";
import {
  deleteScreen,
  fetchAsRun,
  fetchScreenPlays,
  saveScreen,
} from "@/utils/screens/api";
import { explain, resolvePlayback, type ResolutionInputs } from "@/utils/lait/resolve";
import { KIND_PANELS } from "@/program-editor/kinds/registry";
import type { Place, SignageAsRun, SignageScreen } from "@/utils/lait/types";

const EMPTY: ResolutionInputs = {
  screen: null,
  channels: [],
  broadcasts: [],
  audiences: [],
  programs: [],
  media: [],
  presets: [],
};

export default function ScreenDetail({ screenId }: { screenId: string }) {
  const navigate = useNavigate();
  const toast = useToast();
  const { now } = useLive();
  const revision = useRevision();
  const [inputs, setInputs] = useState<ResolutionInputs>(EMPTY);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [removing, setRemoving] = useState(false);
  const [label, setLabel] = useState("");

  const reload = useCallback(async () => {
    try {
      setError(null);
      const { inputs } = await fetchScreenPlays(screenId);
      setInputs(inputs);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not load this screen");
    } finally {
      setLoading(false);
    }
  }, [screenId]);

  // Somebody else's change arrives the same way ours does.
  useEffect(() => {
    void reload();
  }, [reload, revision]);

  const screen = inputs.screen;

  /** Every field on this page funnels through here. */
  const put = useCallback(async (next: SignageScreen) => {
    await saveScreen(next);
    setInputs((held) => ({ ...held, screen: next }));
  }, []);

  /** Resolution, recomputed on the tick rather than on a fetch. */
  const playback = useMemo(
    () => (screen ? resolvePlayback(inputs, now) : null),
    [inputs, screen, now],
  );

  const showingName = useMemo(() => {
    if (playback?.showing.showing !== "program") return undefined;
    const id = playback.showing.program;
    return inputs.programs.find((program) => program.id === id)?.name;
  }, [playback, inputs.programs]);

  const tuned = useCommit<string>({
    committed: screen?.tuned ?? "",
    write: async (next) => {
      if (!screen) return;
      await put({ ...screen, tuned: next || null });
    },
  });

  if (loading) return <Page>Loading…</Page>;
  if (!screen) {
    return (
      <Page>
        <p className="ds-danger-text">{error ?? "That screen is not here."}</p>
      </Page>
    );
  }

  const place = screen.place;
  const writePlace = (patch: Partial<Place>) =>
    put({
      ...screen,
      place: {
        latitude: place?.latitude ?? 0,
        longitude: place?.longitude ?? 0,
        timezone: place?.timezone ?? "",
        region: place?.region ?? null,
        ...patch,
      },
    });

  const interrupted = playback?.source?.via === "broadcast";

  return (
    <Page>
      <header className="ds-row-between" style={{ marginBottom: 18 }}>
        <button
          type="button"
          className="ds-icon"
          aria-label="Back"
          onClick={() => void navigate({ to: "/screen-list" })}
        >
          <ArrowLeft size={20} />
        </button>
        <ScreenName screen={screen} put={put} />
        <button
          type="button"
          className="ds-btn ds-btn-quiet is-danger"
          onClick={() => setRemoving(true)}
        >
          <Trash2 size={15} />
          Remove
        </button>
      </header>

      {error && <p className="ds-danger-text">{error}</p>}

      <div className="ds-stack">
        <section className="ds-panel">
          <h3>
            Right now
            {interrupted && <OnAir label="INTERRUPTED" tone="alarm" />}
          </h3>
          <p style={{ margin: 0, fontSize: "var(--ds-fs-body)", lineHeight: 1.5 }}>
            {playback ? explain(playback, showingName) : "Unknown"}
          </p>
          {playback?.showing.showing === "unaddressed" && !screen.tuned && (
            <p className="ds-hint">
              Nothing reaches this screen. Tune it below, or address a broadcast
              at it.
            </p>
          )}
        </section>

        <section className="ds-panel">
          <h3>Tuned to</h3>
          <Field
            label="Channel"
            commit={tuned}
            hint="What it shows when no broadcast is addressed at it."
          >
            <select
              className="ds-input"
              value={tuned.value}
              onChange={(event) => tuned.setNow(event.target.value)}
            >
              <option value="">Nothing</option>
              {inputs.channels.map((channel) => (
                <option key={channel.id} value={channel.id}>
                  {channel.name}
                </option>
              ))}
            </select>
          </Field>
        </section>

        <section className="ds-panel">
          <h3>Place</h3>
          <p className="ds-hint">
            Where the panel physically is. Kinds that compute from a location —
            prayer times, weather, a local clock — read this and nothing else.
          </p>
          <div className="ds-pair">
            <CommitText
              label="Latitude"
              value={place ? String(place.latitude) : ""}
              placeholder="42.3314"
              inputMode="decimal"
              onWrite={(next) => writePlace({ latitude: Number(next) })}
            />
            <CommitText
              label="Longitude"
              value={place ? String(place.longitude) : ""}
              placeholder="-83.0458"
              inputMode="decimal"
              onWrite={(next) => writePlace({ longitude: Number(next) })}
            />
          </div>
          <CommitText
            label="Time zone"
            value={place?.timezone ?? ""}
            placeholder="America/Detroit"
            list="ds-zones"
            hint="Required. A coordinate without one computes a plausible timetable for the wrong offset, and nothing looks wrong."
            onWrite={(next) => writePlace({ timezone: next })}
          />
          <ZoneOptions />
          <CommitText
            label="Region"
            value={place?.region ?? ""}
            placeholder="MI"
            hint="So an audience can say “every screen in Michigan” without anybody maintaining a label that drifts from the coordinates above."
            onWrite={(next) => writePlace({ region: next || null })}
          />
        </section>

        {KIND_PANELS.map((panel) => (
          <section className="ds-panel" key={panel.kind}>
            <h3>{panel.label} at this venue</h3>
            <p className="ds-hint">
              What this congregation practises, as distinct from how the card
              looks. Two venues under one operator can differ here and still
              share a preset.
            </p>
            {panel.groups
              .flatMap((group) => group.fields)
              .map((field) =>
                "key" in field ? (
                  <CommitText
                    key={field.key}
                    label={field.label}
                    value={screen.facts?.[panel.kind]?.[field.key] ?? ""}
                    placeholder="from the preset"
                    onWrite={async (next) => {
                      const facts = { ...(screen.facts?.[panel.kind] ?? {}) };
                      if (next) facts[field.key] = next;
                      else delete facts[field.key];
                      await put({
                        ...screen,
                        facts: { ...(screen.facts ?? {}), [panel.kind]: facts },
                      });
                    }}
                  />
                ) : null,
              )}
          </section>
        ))}

        <section className="ds-panel">
          <h3>Labels</h3>
          <p className="ds-hint">
            Yours. A screen can be <code>biz:acme</code>, <code>role:menu</code>{" "}
            and <code>rented</code> at once, and audiences address whichever
            slice matters today.
          </p>
          <div className="ds-chips">
            {(screen.labels ?? []).map((held) => (
              <span className="ds-tag" key={held}>
                {held}
                <button
                  type="button"
                  aria-label={`Remove ${held}`}
                  className="ds-icon"
                  style={{ width: 18, height: 18 }}
                  onClick={() =>
                    void put({
                      ...screen,
                      labels: (screen.labels ?? []).filter((other) => other !== held),
                    })
                  }
                >
                  <X size={11} />
                </button>
              </span>
            ))}
          </div>
          <form
            style={{ display: "flex", gap: 8 }}
            onSubmit={(event) => {
              event.preventDefault();
              const next = label.trim();
              if (!next) return;
              void put({
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
            <button type="submit" className="ds-btn ds-btn-ghost">
              Add
            </button>
          </form>
        </section>

        <section className="ds-panel">
          <h3>As run</h3>
          <AsRunList screenId={screen.id} />
        </section>
      </div>

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
              toast.show(
                "Could not remove",
                err instanceof Error ? err.message : String(err),
              ),
            );
        }}
      />
    </Page>
  );
}

/** The title is a field like any other, and commits like one. */
function ScreenName({
  screen,
  put,
}: {
  screen: SignageScreen;
  put: (next: SignageScreen) => Promise<void>;
}) {
  const name = useCommit<string>({
    committed: screen.name,
    write: (next) => put({ ...screen, name: next.trim() || screen.name }),
  });
  return (
    <span style={{ flex: 1, display: "flex", alignItems: "center", gap: 10 }}>
      <input
        className="ds-title-input"
        value={name.value}
        aria-label="Screen name"
        onChange={(event) => name.set(event.target.value)}
        onBlur={() => {
          if (name.state === "pending") name.setNow(name.value);
        }}
      />
      {name.state !== "settled" && (
        <span className={`ds-commit is-${name.state}`}>
          {name.state === "refused" ? name.error : "saving"}
        </span>
      )}
    </span>
  );
}

/**
 * What the panel says it played.
 *
 * Empty means the screen has not spoken — which is not the same as it having
 * played nothing, and the copy says so. Every telemetry table in this industry
 * is written by the server *about* a player; this one is written by the player,
 * under its own identity, which is the only version that attests anything.
 */
function AsRunList({ screenId }: { screenId: string }) {
  const revision = useRevision();
  const [record, setRecord] = useState<SignageAsRun | null>(null);

  useEffect(() => {
    let live = true;
    void fetchAsRun(screenId)
      .then((row) => {
        if (live) setRecord(row);
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [screenId, revision]);

  const entries = (record?.entries ?? []).slice(-8).reverse();
  if (entries.length === 0) {
    return (
      <p className="ds-hint">
        Nothing reported. A panel writes this itself, so an empty list means this
        screen has not spoken — not that it played nothing.
      </p>
    );
  }

  return (
    <div className="ds-stack">
      <p className="ds-hint">
        Last reported <Ago at={entries[0]?.ended_unix_ms} />
      </p>
      {entries.map((entry, index) => (
        <div className="ds-row-between" key={`${entry.item}-${index}`}>
          <span style={{ fontSize: "var(--ds-fs-small)" }}>{entry.item}</span>
          <Ago at={entry.ended_unix_ms} />
        </div>
      ))}
    </div>
  );
}

function ZoneOptions() {
  const zones =
    typeof Intl.supportedValuesOf === "function"
      ? Intl.supportedValuesOf("timeZone")
      : [];
  if (zones.length === 0) return null;
  return (
    <datalist id="ds-zones">
      {zones.map((zone) => (
        <option key={zone} value={zone} />
      ))}
    </datalist>
  );
}
