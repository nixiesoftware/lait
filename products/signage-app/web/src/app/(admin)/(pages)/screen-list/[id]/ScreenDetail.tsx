/**
 * One panel: the horizon.
 *
 * The screen is the hero, drawn as a screen showing what it shows. Above it in
 * precedence — and so above it on the page — the claims on it, ranked the way
 * resolution ranks them: the broadcast that wins, the ones waiting behind it,
 * the channel it is tuned to, and nothing. Below it, its day; around it, the
 * audiences that include it. Facts are readouts, true of the panel, opened
 * into an inspector to change — never a form the page is made of.
 *
 * Nothing here has a Save button. Every field commits itself and says so on
 * itself. The acts live where the relation is drawn: tune on the channel rung,
 * broadcast from the sky.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import {
  ArrowLeft,
  Clock,
  MapPin,
  Megaphone,
  Radio,
  Trash2,
  Tv,
  X,
} from "lucide-react";
import {
  Ago,
  Bezel,
  CommitSelect,
  CommitText,
  Confirm,
  DayTrack,
  Footprint,
  Inspector,
  OnAir,
  Page,
  Picker,
  channelDay,
  litProps,
  useCommit,
  useFocus,
  useHoldable,
  useLive,
  useOrbit,
  useRevision,
  useToast,
  windowToday,
  type Segment,
} from "@/ds";
import { deleteScreen, fetchAsRun, saveScreen, tuneScreen } from "@/utils/screens/api";
import { useFleet } from "@/utils/screens/fleet";
import { explain, reaches, type Context } from "@/utils/lait/resolve";
import { KIND_PANELS } from "@/program-editor/kinds/registry";
import { CityPicker, type CitySelection } from "@/program-editor/fields/CityPicker";
import type {
  Place,
  SignageAsRun,
  SignageBroadcast,
  SignageScreen,
} from "@/utils/lait/types";

export default function ScreenDetail({ screenId }: { screenId: string }) {
  const navigate = useNavigate();
  const toast = useToast();
  const orbit = useOrbit();
  const { now } = useLive();
  const fleet = useFleet();
  const { held } = useFocus();
  const [removing, setRemoving] = useState(false);
  const [tuning, setTuning] = useState(false);
  const [inspecting, setInspecting] = useState<"place" | string | null>(null);
  const [asRun, setAsRun] = useState<SignageAsRun | null>(null);
  const revision = useRevision();

  const screen = fleet.screens.find((entry) => entry.id === screenId) ?? null;

  useEffect(() => {
    let live = true;
    void fetchAsRun(screenId)
      .then((row) => {
        if (live) setAsRun(row);
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [screenId, revision]);

  /** Every field on this page funnels through here. */
  const put = useCallback(
    async (next: SignageScreen) => {
      await saveScreen(next);
      await fleet.reload();
    },
    [fleet],
  );

  const playback = useMemo(
    () => (screen ? fleet.playbackFor(screen, now) : null),
    [fleet, screen, now],
  );

  const showingName = useMemo(() => {
    if (playback?.showing.showing !== "program") return undefined;
    const id = playback.showing.program;
    return fleet.programs.find((program) => program.id === id)?.name;
  }, [playback, fleet.programs]);

  /** Every broadcast with a claim on this screen, ranked as resolution ranks. */
  const claims = useMemo(() => {
    if (!screen) return [];
    const cx: Context = { nowUnixMs: now, observations: {} };
    const lookup = new Map(fleet.audiences.map((entry) => [entry.id, entry.rule]));
    const superseded = new Set(
      fleet.broadcasts
        .filter((entry) => entry.cancelled_at_unix_ms == null || now < entry.cancelled_at_unix_ms)
        .flatMap((entry) => entry.supersedes ?? []),
    );
    return fleet.broadcasts
      .filter((entry) => {
        if (entry.cancelled_at_unix_ms != null && now >= entry.cancelled_at_unix_ms) return false;
        if (superseded.has(entry.id)) return false;
        const rule = lookup.get(entry.audience);
        if (!rule || !reaches(rule, screen, cx, lookup)) return false;
        return entry.timing.timing === "when" ? reaches(entry.timing.of, screen, cx, lookup) : true;
      })
      .sort((a, b) => priorityOf(b) - priorityOf(a) || a.id.localeCompare(b.id));
  }, [fleet.broadcasts, fleet.audiences, screen, now]);

  /** The audiences that include this screen, drawn as their footprints. */
  const including = useMemo(() => {
    if (!screen) return [];
    return fleet.audiences
      .map((audience) => ({ audience, reached: fleet.reachedBy(audience.rule) }))
      .filter(({ reached }) => reached.has(screen.id));
  }, [fleet, screen]);

  const channel = screen?.tuned
    ? fleet.channels.find((entry) => entry.id === screen.tuned) ?? null
    : null;

  /** Today: the channel's day, with every open claim laid over it. */
  const day = useMemo<Segment[]>(() => {
    const segments: Segment[] = channel
      ? channelDay(
          channel,
          now,
          (program) =>
            void navigate({ to: "/broadcast-list/broadcast/$id", params: { id: program } }),
          (program) => fleet.programs.find((entry) => entry.id === program)?.name,
        )
      : [];
    claims.forEach((claim, index) => {
      const span =
        claim.timing.timing === "window"
          ? windowToday(claim.timing, now)
          : { start: 0, end: 24 * 60 * 60 * 1000 };
      if (!span) return;
      segments.push({
        id: claim.id,
        start: span.start,
        end: span.end,
        tone: "band",
        height: claims.length - index,
        title: claim.name,
        onOpen: () => void navigate({ to: "/broadcast-hub" }),
      });
    });
    return segments;
  }, [channel, claims, now, navigate]);

  const heard = asRun?.entries?.length
    ? asRun.entries[asRun.entries.length - 1]?.ended_unix_ms ?? null
    : null;

  if (fleet.loading && !screen) return <Page>Loading…</Page>;
  if (!screen) {
    return (
      <Page>
        <p className="ds-danger-text">{fleet.error ?? "That screen is not here."}</p>
      </Page>
    );
  }

  const winner = playback?.source;
  const interrupted = winner?.via === "broadcast";

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

      {fleet.error && <p className="ds-danger-text">{fleet.error}</p>}

      {/* ── The horizon: the screen, and what is true of it ─────────────── */}
      <section className="ds-horizon">
        <Bezel
          size="lg"
          screen={screen}
          playback={playback}
          programs={fleet.programs}
          media={fleet.media}
          presets={fleet.presets}
          orbit={orbit}
          now={now}
          heard={{ at: heard }}
        />
        <div className="ds-horizon-copy">
          <div className="ds-row-between" style={{ justifyContent: "flex-start" }}>
            {interrupted && <OnAir label="Interrupted" tone="alarm" />}
            {!interrupted && playback?.showing.showing === "program" && (
              <OnAir label="Showing" />
            )}
          </div>
          <p className="ds-horizon-why">{playback ? explain(playback, showingName) : "Unknown"}</p>
          <div className="ds-horizon-facts">
            <span>
              <MapPin size={14} />
              {screen.place
                ? `${screen.place.region ? `${screen.place.region} · ` : ""}${screen.place.timezone}`
                : "No place"}
            </span>
            <span>
              <Tv size={14} />
              {channel ? channel.name : "Not tuned"}
            </span>
            <span>
              <Clock size={14} />
              {heard == null ? (
                <span className="ds-ago is-never">never heard</span>
              ) : (
                <Ago at={heard} />
              )}
            </span>
          </div>
          <Labels screen={screen} put={put} />
        </div>
      </section>

      {/* ── The sky: what outranks what ─────────────────────────────────── */}
      <section className="ds-band">
        <h3>What reaches it</h3>
        <div className="ds-sky">
          {claims.map((claim, index) => {
            const audience = fleet.audiences.find((entry) => entry.id === claim.audience);
            const wins = index === 0;
            return (
              <div
                className="ds-sky-rung"
                key={claim.id}
                data-wins={wins || undefined}
                data-onair={wins || undefined}
                {...litProps(held, held?.kind === "broadcast" && held.id === claim.id)}
              >
                <span className="ds-sky-mark">
                  <Megaphone size={14} />
                </span>
                <button
                  type="button"
                  className="ds-sky-hit"
                  onClick={() => void navigate({ to: "/broadcast-hub" })}
                >
                  <span className="ds-sky-copy">
                    <strong>{claim.name}</strong>
                    <span>
                      {describeAction(claim, fleet)} · {audience?.name ?? "an audience"} · priority{" "}
                      {priorityOf(claim)}
                    </span>
                  </span>
                </button>
                <span className="ds-sky-acts" />
              </div>
            );
          })}

          <ChannelRung
            channel={channel}
            wins={claims.length === 0 && channel != null}
            programName={
              channel?.base
                ? fleet.programs.find((program) => program.id === channel.base)?.name
                : undefined
            }
            onTune={() => setTuning(true)}
            onOpen={() => void navigate({ to: "/channel-list" })}
          />

          {!channel && claims.length === 0 && (
            <div className="ds-sky-rung is-fallback" data-wins>
              <span className="ds-sky-mark">
                <X size={14} />
              </span>
              <span className="ds-sky-copy">
                <strong>Nothing reaches this screen</strong>
                <span>Tune it to a channel, or address a broadcast at it.</span>
              </span>
              <span className="ds-sky-acts">
                <button type="button" className="ds-btn ds-btn-ghost" onClick={() => setTuning(true)}>
                  Tune…
                </button>
              </span>
            </div>
          )}
        </div>
        <div className="ds-page-actions">
          <button
            type="button"
            className="ds-btn ds-btn-solid"
            onClick={() => void navigate({ to: "/broadcast-hub", search: { screen: screen.id } })}
          >
            <Radio size={15} />
            Broadcast to this screen
          </button>
        </div>
      </section>

      {/* ── Today ──────────────────────────────────────────────────────── */}
      <section className="ds-band">
        <h3>Today</h3>
        {day.length === 0 ? (
          <p className="ds-hint">Nothing is scheduled for it. Tune it to a channel with dayparts.</p>
        ) : (
          <DayTrack segments={day} now={now} timezone={screen.place?.timezone} />
        )}
      </section>

      {/* ── Who includes it ────────────────────────────────────────────── */}
      {including.length > 0 && (
        <section className="ds-band">
          <h3>Audiences that include it</h3>
          <div className="ds-audiences">
            {including.map(({ audience, reached }) => (
              <AudienceCard
                key={audience.id}
                id={audience.id}
                name={audience.name}
                screens={fleet.screens}
                reached={reached}
                onOpen={(target) =>
                  void navigate({ to: "/screen-list/$id", params: { id: target.id } })
                }
              />
            ))}
          </div>
        </section>
      )}

      {/* ── Facts: readouts, opened into an inspector ─────────────────── */}
      <section className="ds-band">
        <h3>What is true of it</h3>
        <div className="ds-readouts">
          <button type="button" className="ds-readout" onClick={() => setInspecting("place")}>
            <span>Place</span>
            <strong className={screen.place ? undefined : "is-absent"}>
              {screen.place
                ? `${screen.place.latitude.toFixed(4)}, ${screen.place.longitude.toFixed(4)} · ${screen.place.timezone}`
                : "Not placed"}
            </strong>
          </button>
          {KIND_PANELS.map((panel) => {
            const facts = screen.facts?.[panel.kind] ?? {};
            const said = Object.keys(facts).length;
            return (
              <button
                type="button"
                className="ds-readout"
                key={panel.kind}
                onClick={() => setInspecting(panel.kind)}
              >
                <span>{panel.label} at this venue</span>
                <strong className={said ? undefined : "is-absent"}>
                  {said ? summarizeFacts(panel.kind, facts) : "From the preset"}
                </strong>
              </button>
            );
          })}
        </div>
      </section>

      {/* ── What it says it played ────────────────────────────────────── */}
      <section className="ds-band">
        <h3>As run</h3>
        <AsRunLog record={asRun} programs={fleet.programs} media={fleet.media} />
      </section>

      {/* ── Overlays ───────────────────────────────────────────────────── */}
      <Picker
        open={tuning}
        onOpenChange={setTuning}
        title="Tune to"
        empty="No channels yet. Make one first."
        items={[
          ...fleet.channels.map((entry) => ({
            id: entry.id,
            label: entry.name,
            hint: `${fleet.tunedTo(entry.id).length} tuned`,
          })),
          ...(screen.tuned ? [{ id: "", label: "Nothing", hint: "Untune", danger: true }] : []),
        ]}
        onPick={(id) => {
          void tuneScreen(screen.id, id || null)
            .then(() => fleet.reload())
            .catch((err: unknown) =>
              toast.show("Could not tune", err instanceof Error ? err.message : String(err)),
            );
        }}
      />

      <Inspector
        open={inspecting === "place"}
        onOpenChange={(open) => !open && setInspecting(null)}
        title="Place"
        kicker={
          <span className="ds-hint">
            Where the panel physically is. Kinds that compute from a location read this and
            nothing else.
          </span>
        }
      >
        <PlaceFields screen={screen} put={put} />
      </Inspector>

      {KIND_PANELS.map((panel) => (
        <Inspector
          key={panel.kind}
          open={inspecting === panel.kind}
          onOpenChange={(open) => !open && setInspecting(null)}
          title={`${panel.label} at this venue`}
          kicker={
            <span className="ds-hint">
              What this congregation practises, as distinct from how the card looks. Two venues
              under one operator can differ here and still share a preset.
            </span>
          }
        >
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
                    await put({ ...screen, facts: { ...(screen.facts ?? {}), [panel.kind]: facts } });
                  }}
                />
              ) : null,
            )}
        </Inspector>
      ))}

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

function priorityOf(broadcast: SignageBroadcast): number {
  return broadcast.timing.timing === "when"
    ? broadcast.timing.priority
    : (broadcast.timing.priority ?? 0);
}

function describeAction(
  broadcast: SignageBroadcast,
  fleet: { programs: { id: string; name: string }[]; channels: { id: string; name: string }[] },
): string {
  const action = broadcast.action;
  switch (action.action) {
    case "play":
      return fleet.programs.find((p) => p.id === action.program)?.name ?? "a program";
    case "tune":
      return `tune to ${fleet.channels.find((c) => c.id === action.channel)?.name ?? "a channel"}`;
    case "blank":
      return "go dark";
    case "restore":
      return "all clear";
    case "kind":
      return action.kind;
  }
}

function summarizeFacts(kind: string, facts: Record<string, string>): string {
  const panel = KIND_PANELS.find((entry) => entry.kind === kind);
  const fields = panel?.groups.flatMap((group) => group.fields) ?? [];
  return Object.entries(facts)
    .map(([key, value]) => {
      const field = fields.find((entry) => "key" in entry && entry.key === key);
      const options = field && "options" in field ? (field.options as { value: string; label: string }[] | undefined) : undefined;
      return options?.find((option) => option.value === value)?.label ?? value;
    })
    .join(" · ");
}

/** The channel rung: the standing claim, and the act that changes it. */
function ChannelRung({
  channel,
  wins,
  programName,
  onTune,
  onOpen,
}: {
  channel: { id: string; name: string } | null;
  wins: boolean;
  programName?: string;
  onTune: () => void;
  onOpen: () => void;
}) {
  const { held } = useFocus();
  const hold = useHoldable("channel", channel?.id ?? "");
  if (!channel) return null;
  return (
    <div
      className="ds-sky-rung"
      data-wins={wins || undefined}
      {...hold.bind}
      {...litProps(held, held?.kind === "channel" && held.id === channel.id)}
    >
      <span className="ds-sky-mark">
        <Tv size={14} />
      </span>
      <button type="button" className="ds-sky-hit" onClick={onOpen}>
        <span className="ds-sky-copy">
          <strong>{channel.name}</strong>
          <span>{programName ? `carries ${programName}` : "carries nothing yet"}</span>
        </span>
      </button>
      <span className="ds-sky-acts">
        <button type="button" className="ds-btn ds-btn-quiet" onClick={onTune}>
          Tune…
        </button>
      </span>
    </div>
  );
}

function AudienceCard({
  id,
  name,
  screens,
  reached,
  onOpen,
}: {
  id: string;
  name: string;
  screens: SignageScreen[];
  reached: Set<string>;
  onOpen: (screen: SignageScreen) => void;
}) {
  const hold = useHoldable("audience", id);
  return (
    <div className="ds-audience" {...hold.bind}>
      <strong>{name}</strong>
      <Footprint screens={screens} reached={reached} onOpen={onOpen} />
    </div>
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

/** Labels are identity, so they sit under the name, not in a settings panel. */
function Labels({
  screen,
  put,
}: {
  screen: SignageScreen;
  put: (next: SignageScreen) => Promise<void>;
}) {
  const [label, setLabel] = useState("");
  return (
    <div className="ds-stack" style={{ gap: 8 }}>
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
        style={{ display: "flex", gap: 8, maxWidth: 360 }}
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
          placeholder="Add a label"
          aria-label="Add a label"
          onChange={(event) => setLabel(event.target.value)}
        />
        <button type="submit" className="ds-btn ds-btn-ghost">
          Add
        </button>
      </form>
    </div>
  );
}

/** Where it is: a city picked, or coordinates typed; the zone required. */
function PlaceFields({
  screen,
  put,
}: {
  screen: SignageScreen;
  put: (next: SignageScreen) => Promise<void>;
}) {
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
  const pick = (city: CitySelection) =>
    void writePlace({
      latitude: city.latitude,
      longitude: city.longitude,
      timezone: city.timezone,
      region: city.admin1 ?? place?.region ?? null,
    });
  const zones =
    typeof Intl.supportedValuesOf === "function" ? Intl.supportedValuesOf("timeZone") : [];
  return (
    <div className="ds-stack">
      <CityPicker
        currentLatitude={place?.latitude ?? null}
        currentLongitude={place?.longitude ?? null}
        initialLabel={place?.region ?? null}
        onSelect={pick}
      />
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
      <CommitSelect
        label="Time zone"
        value={place?.timezone ?? ""}
        options={[{ value: "", label: "Choose a zone" }, ...zones.map((zone) => ({ value: zone, label: zone }))]}
        hint="Required. A coordinate without one computes a plausible timetable for the wrong offset, and nothing looks wrong."
        onWrite={(next) => writePlace({ timezone: next })}
      />
      <CommitText
        label="Region"
        value={place?.region ?? ""}
        placeholder="MI"
        hint="So an audience can say “every screen in Michigan” without anybody maintaining a label."
        onWrite={(next) => writePlace({ region: next || null })}
      />
    </div>
  );
}

/**
 * What the panel says it played, in its own words.
 *
 * Empty means the screen has not spoken — which is not the same as it having
 * played nothing. Names are resolved; a panel reports ids.
 */
function AsRunLog({
  record,
  programs,
  media,
}: {
  record: SignageAsRun | null;
  programs: { id: string; name: string; items: { id: string; media: string }[] }[];
  media: { id: string; name: string }[];
}) {
  const entries = (record?.entries ?? []).slice(-8).reverse();
  if (entries.length === 0) {
    return (
      <p className="ds-hint">
        Nothing reported. A panel writes this itself, so an empty list means this screen has not
        spoken — not that it played nothing.
      </p>
    );
  }
  const nameOf = (entry: { program: string; item: string }) => {
    const program = programs.find((p) => p.id === entry.program);
    const item = program?.items.find((i) => i.id === entry.item);
    const file = item ? media.find((m) => m.id === item.media) : null;
    return { program: program?.name ?? entry.program, item: file?.name ?? entry.item };
  };
  return (
    <div className="ds-log">
      {entries.map((entry, index) => {
        const names = nameOf(entry);
        return (
          <div className="ds-log-row" key={`${entry.item}-${index}`}>
            <Clock size={13} />
            <span>
              {names.item} <em>· {names.program}</em>
            </span>
            <Ago at={entry.ended_unix_ms} />
          </div>
        );
      })}
    </div>
  );
}
