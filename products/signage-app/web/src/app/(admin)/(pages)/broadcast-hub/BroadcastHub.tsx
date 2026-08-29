/**
 * Transmissions, and who they reach.
 *
 * One rule governs this page: **you see the blast radius before you send, and
 * you see who you miss.** The composer is two questions drawn as objects —
 * what, as the programs themselves; whom, as the fleet itself — and one
 * deliberate act, in the colour of reach, wearing its count. There is no
 * Cancel, because nothing has happened until Send; and there is no dialog
 * after Stop, because a stopped broadcast is kept and can be put back.
 *
 * Facet chips carry their own size before you pick them, which is Partiful's
 * move and strictly better than making somebody commit to a filter to find out
 * how big it is.
 */

import { useMemo, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { Moon, Plus, Radio, Sun, X } from "lucide-react";
import {
  Ago,
  Bezel,
  ChoiceMenu,
  Cover,
  DayTrack,
  Empty,
  Footprint,
  OnAir,
  Page,
  PageHeader,
  PageStatus,
  haptic,
  useLive,
  useOrbit,
  useToast,
  useUndo,
  windowToday,
  timeOfDayIn,
  DAY_MS,
  type Segment,
} from "@/ds";
import { draftAudience } from "@/utils/apps/api";
import {
  putAudience,
  putBroadcast,
  removeBroadcast,
  useFleet,
  type Fleet,
} from "@/utils/screens/fleet";
import { Thumb } from "@/program-editor/Thumb";
import { screensReached } from "@/utils/lait/resolve";
import { mintBodyId } from "@/utils/lait/ids";
import { broadcastStatus, nextQuarterHour, scheduledWindow } from "@/utils/lait/schedule";
import type {
  BroadcastAction,
  Match,
  SignageAudience,
  SignageBroadcast,
  SignageProgram,
  SignageScreen,
} from "@/utils/lait/types";

export default function BroadcastHub({ screen: addressed }: { screen?: string } = {}) {
  const toast = useToast();
  const undo = useUndo();
  const navigate = useNavigate();
  const orbit = useOrbit();
  const { now: tick } = useLive();
  const fleet = useFleet();
  const { broadcasts, audiences, screens, programs, channels, loading, error } = fleet;
  const [composing, setComposing] = useState(Boolean(addressed));

  const refused = (what: string) => (err: unknown) => {
    haptic("error");
    toast.show(what, err instanceof Error ? err.message : String(err));
  };

  const describe = (action: BroadcastAction) => {
    switch (action.action) {
      case "play":
        return programs.find((p) => p.id === action.program)?.name ?? "a program";
      case "tune":
        return `tune to ${channels.find((c) => c.id === action.channel)?.name ?? "a channel"}`;
      case "blank":
        return "go dark";
      case "restore":
        return "all clear";
      case "kind":
        return action.kind;
    }
  };

  const live = broadcasts.filter(
    (entry) => entry.cancelled_at_unix_ms == null || tick < entry.cancelled_at_unix_ms,
  );
  const stopped = broadcasts
    .filter((entry) => entry.cancelled_at_unix_ms != null && tick >= entry.cancelled_at_unix_ms)
    .sort((a, b) => (b.cancelled_at_unix_ms ?? 0) - (a.cancelled_at_unix_ms ?? 0));

  /** Stop now. The record stays, and for a moment it can be put back on air. */
  const stop = (broadcast: SignageBroadcast) => {
    haptic("delete");
    const stoppedAt = Date.now();
    void putBroadcast({ ...broadcast, cancelled_at_unix_ms: stoppedAt })
      .then(() =>
        undo.offer(`Stopped ${broadcast.name}`, () =>
          putBroadcast({ ...broadcast, cancelled_at_unix_ms: null }),
        ),
      )
      .catch(refused("Could not stop"));
  };

  const forget = (broadcast: SignageBroadcast) => {
    haptic("delete");
    void removeBroadcast(broadcast.id)
      .then((was) => {
        if (was) undo.offer(`Deleted ${was.name}`, () => putBroadcast(was));
      })
      .catch(refused("Could not delete"));
  };

  return (
    <Page>
      <PageHeader title="Broadcasts" icon={<Radio size={20} />}>
        <button
          type="button"
          className={`ds-btn ${composing ? "ds-btn-ghost" : "ds-btn-solid"}`}
          onClick={() => setComposing((was) => !was)}
        >
          {composing ? <X size={16} /> : <Plus size={16} />}
          {composing ? "Close" : "New broadcast"}
        </button>
      </PageHeader>

      <PageStatus loading={loading} error={error ?? ""} />

      {composing && !loading && (
        <Composer
          addressed={addressed}
          fleet={fleet}
          orbit={orbit}
          onSent={() => {
            setComposing(false);
            if (addressed) void navigate({ to: "/broadcast-hub", search: {} });
          }}
          onRefused={refused("Could not send")}
        />
      )}

      {!loading && live.length === 0 && (
        <Empty title="Nothing is being broadcast">
          <p className="ds-hint">
            Every screen is showing the channel it is tuned to. A broadcast interrupts that for
            whoever it reaches, and stops on its own.
          </p>
          {screens.length > 0 && (
            <div className="ds-attached" style={{ justifyContent: "center" }}>
              {screens.map((screen) => (
                <button
                  type="button"
                  key={screen.id}
                  className="ds-attached-hit"
                  title={screen.name}
                  aria-label={screen.name}
                  onClick={() => void navigate({ to: "/screen-list/$id", params: { id: screen.id } })}
                >
                  <Bezel
                    size="sm"
                    screen={screen}
                    playback={fleet.playbackFor(screen, tick)}
                    programs={programs}
                    media={fleet.media}
                    presets={fleet.presets}
                    orbit={orbit}
                    now={tick}
                  />
                </button>
              ))}
            </div>
          )}
          {!composing && (
            <button type="button" className="ds-btn ds-btn-solid" onClick={() => setComposing(true)}>
              <Plus size={16} />
              New broadcast
            </button>
          )}
        </Empty>
      )}

      <div className="ds-stack">
        {live.map((broadcast) => {
          const rule = audiences.find((entry) => entry.id === broadcast.audience);
          const reached = rule ? screensReached(rule.rule, screens, audiences) : [];
          const span =
            broadcast.timing.timing === "window"
              ? windowToday(broadcast.timing, tick)
              : { start: 0, end: DAY_MS };
          const band: Segment[] = span
            ? [{ id: broadcast.id, start: span.start, end: span.end, tone: "band", title: broadcast.name }]
            : [];
          // A window still ahead is scheduled, not on air: the chip says which,
          // and only a broadcast that is reaching screens now wears the alarm.
          const status = broadcastStatus(
            broadcast.timing.timing === "window" ? span : undefined,
            broadcast.timing.timing === "window" ? timeOfDayIn(tick, broadcast.timing.timezone) : null,
          );
          const onAir = status.kind === "on_air";
          return (
            <div className={`ds-unit${onAir ? " is-onair" : ""} ds-transit ds-arrive`} key={broadcast.id}>
              <div className="ds-transit-head">
                <OnAir
                  label={status.kind === "on_air" ? "On air" : status.kind === "not_today" ? "Not today" : status.label}
                  tone={onAir ? "alarm" : "quiet"}
                />
                <div className="ds-unit-copy">
                  <strong>{broadcast.name}</strong>
                  <span>
                    {describe(broadcast.action)} · {rule?.name ?? "an audience"} · priority{" "}
                    {broadcast.timing.timing === "when"
                      ? broadcast.timing.priority
                      : (broadcast.timing.priority ?? 0)}
                  </span>
                </div>
                <button
                  type="button"
                  className="ds-btn ds-btn-danger"
                  onClick={() => stop(broadcast)}
                >
                  Stop
                </button>
              </div>
              {/* When it is open, laid over the day; whom it reaches, as the fleet. */}
              <DayTrack size="sm" segments={band} now={tick} />
              <Footprint
                screens={screens}
                reached={new Set(reached.map((s) => s.id))}
                onOpen={(screen) => void navigate({ to: "/screen-list/$id", params: { id: screen.id } })}
              />
            </div>
          );
        })}
      </div>

      {stopped.length > 0 && (
        <section className="ds-panel" style={{ marginTop: 18 }}>
          <h3>Stopped</h3>
          <p className="ds-hint">
            Kept rather than deleted, so “what interrupted the menus at 14:30 and who stopped it”
            stays answerable.
          </p>
          {stopped.map((broadcast) => (
            <div className="ds-row-between" key={broadcast.id}>
              <span style={{ fontSize: "var(--ds-fs-small)" }}>
                {broadcast.name} · {describe(broadcast.action)} · stopped{" "}
                <Ago at={broadcast.cancelled_at_unix_ms} />
              </span>
              <span className="ds-page-actions">
                <button
                  type="button"
                  className="ds-btn ds-btn-quiet"
                  onClick={() => {
                    haptic("save");
                    void putBroadcast({ ...broadcast, cancelled_at_unix_ms: null }).catch(
                      refused("Could not put it back on air"),
                    );
                  }}
                >
                  Send again
                </button>
                <button
                  type="button"
                  className="ds-btn ds-btn-quiet"
                  onClick={() => forget(broadcast)}
                >
                  Delete
                </button>
              </span>
            </div>
          ))}
        </section>
      )}
    </Page>
  );
}

type Reach = { reached: SignageScreen[]; missed: SignageScreen[] };

/** Three words, not a number. Higher wins when two broadcasts reach one screen. */
const URGENCY = [
  { id: "normal", label: "Normal", priority: 50 },
  { id: "urgent", label: "Urgent", priority: 80 },
  { id: "top", label: "Emergency", priority: 100 },
] as const;

function Composer({
  addressed,
  fleet,
  orbit,
  onSent,
  onRefused,
}: {
  /** A screen handed in from its own page: the rule starts with it. */
  addressed?: string;
  fleet: Fleet;
  orbit: string | null;
  onSent: () => void;
  onRefused: (err: unknown) => void;
}) {
  const { screens, audiences, programs, channels, media } = fleet;
  const [name, setName] = useState("");
  /** Explicit, never implied by an empty filter — Okta's everyone-vs-subset. */
  const [everyone, setEveryone] = useState(false);
  const [reuse, setReuse] = useState<string>("");
  const [labels, setLabels] = useState<string[]>([]);
  const [regions, setRegions] = useState<string[]>([]);
  /** Named screens, one at a time. A page hands one in; pressing panels adds more. */
  const [named, setNamed] = useState<string[]>(addressed ? [addressed] : []);
  const [action, setAction] = useState<BroadcastAction | null>(null);
  const [urgency, setUrgency] = useState<(typeof URGENCY)[number]["id"]>("normal");
  /** Now, or a window from a time for some minutes — a scheduled broadcast. */
  const [when, setWhen] = useState<"now" | "later">("now");
  const [startAt, setStartAt] = useState(() => nextQuarterHour());
  const [minutes, setMinutes] = useState("30");
  const [sending, setSending] = useState(false);

  const mediaMap = useMemo(() => new Map(media.map((entry) => [entry.id, entry])), [media]);

  /** Facets carry their own size, so nobody has to choose one to learn it. */
  const facets = useMemo(() => {
    const counts = new Map<string, number>();
    for (const screen of screens) {
      for (const label of screen.labels ?? []) {
        counts.set(label, (counts.get(label) ?? 0) + 1);
      }
    }
    return [...counts.entries()].sort(([a], [b]) => a.localeCompare(b));
  }, [screens]);

  const places = useMemo(() => {
    const counts = new Map<string, number>();
    for (const screen of screens) {
      const held = screen.place?.region;
      if (held) counts.set(held, (counts.get(held) ?? 0) + 1);
    }
    return [...counts.entries()].sort(([a], [b]) => a.localeCompare(b));
  }, [screens]);

  const rule: Match | null = useMemo(() => {
    if (reuse) {
      return audiences.find((entry) => entry.id === reuse)?.rule ?? null;
    }
    if (everyone) return { match: "all" };
    const slice: Match[] = [
      ...labels.map((label): Match => ({ match: "label", label })),
      ...regions.map((region): Match => ({ match: "place", place: { kind: "region", region } })),
    ];
    const sliced: Match | null =
      slice.length === 0 ? null : slice.length === 1 ? slice[0] : { match: "any_of", of: slice };
    // Named screens are reached whatever the slice says: "these, and also the lobby".
    const names: Match[] = named.map((screen) => ({ match: "screen", screen }));
    if (names.length === 0) return sliced;
    const any: Match[] = sliced ? [sliced, ...names] : names;
    return any.length === 1 ? any[0] : { match: "any_of", of: any };
  }, [reuse, everyone, labels, regions, named, audiences]);

  const reach: Reach = useMemo(() => {
    if (!rule) return { reached: [], missed: screens };
    const reached = screensReached(rule, screens, audiences);
    const hit = new Set(reached.map((screen) => screen.id));
    return { reached, missed: screens.filter((screen) => !hit.has(screen.id)) };
  }, [rule, screens, audiences]);

  const toggleNamed = (screen: SignageScreen) => {
    haptic("select");
    setNamed((held) =>
      held.includes(screen.id) ? held.filter((id) => id !== screen.id) : [...held, screen.id],
    );
  };

  const whatName = (() => {
    if (!action) return null;
    switch (action.action) {
      case "play":
        return programs.find((p) => p.id === action.program)?.name ?? "a program";
      case "tune":
        return `tune to ${channels.find((c) => c.id === action.channel)?.name ?? "a channel"}`;
      case "blank":
        return "Go dark";
      case "restore":
        return "All clear";
      case "kind":
        return action.kind;
    }
  })();

  const whoName = (() => {
    if (reuse) return audiences.find((entry) => entry.id === reuse)?.name ?? "an audience";
    if (everyone) return "everyone";
    const parts = [...labels, ...regions];
    if (named.length > 0) {
      parts.push(
        ...named.map((id) => screens.find((screen) => screen.id === id)?.name ?? "a screen"),
      );
    }
    return parts.join(", ");
  })();

  /** A name nobody has to invent: what, to whom. Editable, never required. */
  const suggested = whatName && whoName ? `${whatName} to ${whoName}` : "";

  const later = when === "later";
  const minutesValue = Number.parseInt(minutes, 10);
  const timed = !later || (/^\d{2}:\d{2}$/.test(startAt) && Number.isInteger(minutesValue) && minutesValue >= 1);
  const ready = !!rule && !!action && reach.reached.length > 0 && !sending && timed;

  const send = async () => {
    if (!rule || !action) return;
    setSending(true);
    try {
      // An audience that already says exactly this is reused rather than
      // minted again, so "everyone" is one audience however often it is sent to.
      const same = JSON.stringify(rule);
      const existing = reuse
        ? (audiences.find((entry) => entry.id === reuse) ?? null)
        : (audiences.find((entry) => JSON.stringify(entry.rule) === same) ?? null);
      const audience: SignageAudience =
        existing ?? draftAudience(everyone ? "Every screen" : whoName || "Audience", rule);
      if (!existing) await putAudience(audience);
      const priority = URGENCY.find((entry) => entry.id === urgency)?.priority ?? 50;
      await putBroadcast({
        id: mintBodyId(),
        name: name.trim() || suggested || "Broadcast",
        audience: audience.id,
        action,
        timing: later
          ? { timing: "window", ...scheduledWindow(startAt, minutesValue, priority) }
          : { timing: "when", of: { match: "all" }, priority },
        supersedes: [],
        cancelled_at_unix_ms: null,
      });
      haptic("save");
      onSent();
    } catch (err) {
      onRefused(err);
    } finally {
      setSending(false);
    }
  };

  const coverOf = (program: SignageProgram) =>
    program.items.slice(0, 4).map((item) => (
      <Thumb key={item.id} media={mediaMap.get(item.media)} orbit={orbit} />
    ));

  const isAct = (probe: BroadcastAction) =>
    action != null &&
    action.action === probe.action &&
    (probe.action !== "play" || (action.action === "play" && action.program === probe.program)) &&
    (probe.action !== "tune" || (action.action === "tune" && action.channel === probe.channel));

  const pickAct = (next: BroadcastAction) => {
    haptic("select");
    setAction(next);
  };

  return (
    <section className="ds-compose ds-arrive" aria-label="New broadcast">
      {/* ── What ─────────────────────────────────────────────────────── */}
      <div className="ds-compose-col">
        <h3>What</h3>
        <div className="ds-acts" role="radiogroup" aria-label="What it does">
          {programs.map((program) => {
            const probe: BroadcastAction = { action: "play", program: program.id };
            const on = isAct(probe);
            return (
              <button
                type="button"
                key={program.id}
                role="radio"
                aria-checked={on}
                className={`ds-act${on ? " is-on" : ""}`}
                onClick={() => pickAct(probe)}
              >
                <Cover>{coverOf(program)}</Cover>
                {program.name}
              </button>
            );
          })}
          <button
            type="button"
            role="radio"
            aria-checked={isAct({ action: "blank" })}
            className={`ds-act${isAct({ action: "blank" }) ? " is-on" : ""}`}
            onClick={() => pickAct({ action: "blank" })}
          >
            <span className="ds-act-mark">
              <Moon size={16} />
            </span>
            Go dark
          </button>
          <button
            type="button"
            role="radio"
            aria-checked={isAct({ action: "restore" })}
            className={`ds-act${isAct({ action: "restore" }) ? " is-on" : ""}`}
            onClick={() => pickAct({ action: "restore" })}
          >
            <span className="ds-act-mark is-restore">
              <Sun size={16} />
            </span>
            All clear
          </button>
          {channels.length > 0 && (
            <ChoiceMenu
              label="Tune to a channel"
              className={`ds-act${action?.action === "tune" ? " is-on" : ""}`}
              items={channels.map((channel) => ({
                id: channel.id,
                label: channel.name,
                on: action?.action === "tune" && action.channel === channel.id,
              }))}
              onPick={(id) => pickAct({ action: "tune", channel: id })}
            >
              <span className="ds-act-mark">
                <Radio size={16} />
              </span>
              {action?.action === "tune"
                ? `Tune to ${channels.find((c) => c.id === action.channel)?.name ?? "…"}`
                : "Tune to…"}
            </ChoiceMenu>
          )}
        </div>
        {programs.length === 0 && (
          <p className="ds-hint">No programs yet. Go dark and All clear still work.</p>
        )}
      </div>

      {/* ── Who ──────────────────────────────────────────────────────── */}
      <div className="ds-compose-col">
        <h3>Who</h3>
        <div className="ds-chips">
          <button
            type="button"
            className={`ds-chip${everyone ? " is-on" : ""}`}
            aria-pressed={everyone}
            onClick={() => {
              haptic("select");
              setReuse("");
              setEveryone(!everyone);
            }}
          >
            Every screen<em>{screens.length}</em>
          </button>
          {!everyone &&
            facets.map(([label, count]) => (
              <button
                type="button"
                key={label}
                className={`ds-chip${labels.includes(label) ? " is-on" : ""}`}
                aria-pressed={labels.includes(label)}
                onClick={() => {
                  haptic("select");
                  setReuse("");
                  setLabels(
                    labels.includes(label)
                      ? labels.filter((held) => held !== label)
                      : [...labels, label],
                  );
                }}
              >
                {label}
                <em>{count}</em>
              </button>
            ))}
          {!everyone &&
            places.map(([region, count]) => (
              <button
                type="button"
                key={`place:${region}`}
                className={`ds-chip${regions.includes(region) ? " is-on" : ""}`}
                aria-pressed={regions.includes(region)}
                onClick={() => {
                  haptic("select");
                  setReuse("");
                  setRegions(
                    regions.includes(region)
                      ? regions.filter((held) => held !== region)
                      : [...regions, region],
                  );
                }}
              >
                in {region}
                <em>{count}</em>
              </button>
            ))}
          {audiences.length > 0 && !everyone && (
            <ChoiceMenu
              label="Reuse an audience"
              className={`ds-chip${reuse ? " is-on" : ""}`}
              items={[
                ...audiences.map((audience) => ({
                  id: audience.id,
                  label: audience.name,
                  hint: `${fleet.reachedBy(audience.rule).size} reached`,
                  on: audience.id === reuse,
                })),
                ...(reuse ? [{ id: "", label: "Build one instead", on: false }] : []),
              ]}
              onPick={(id) => {
                haptic("select");
                setReuse(id);
                if (id) {
                  setLabels([]);
                  setRegions([]);
                  setNamed([]);
                }
              }}
            >
              {reuse
                ? (audiences.find((entry) => entry.id === reuse)?.name ?? "Audience")
                : "Reuse an audience…"}
            </ChoiceMenu>
          )}
        </div>

        {/* The fleet as the fleet, and as the control: press a panel to name it. */}
        <div className="ds-who">
          {screens.length > 0 && (
            <Footprint
              size="sm"
              named
              screens={screens}
              reached={rule ? new Set(reach.reached.map((screen) => screen.id)) : undefined}
              onOpen={reuse ? undefined : toggleNamed}
            />
          )}
          <div className="ds-reach-line">
            <strong>
              {reach.reached.length} of {screens.length}
            </strong>
            <span>{reach.reached.length === 1 ? "screen" : "screens"}</span>
            {rule && reach.missed.length > 0 && (
              <span className="is-miss">· {reach.missed.length} not reached</span>
            )}
            {!rule && <span>· press a panel, a label, or every screen</span>}
          </div>
        </div>
      </div>

      {/* ── The act ──────────────────────────────────────────────────── */}
      <div className="ds-compose-foot">
        <input
          className="ds-input ds-compose-name"
          value={name}
          placeholder={suggested || "Name it, or don't"}
          aria-label="Name"
          onChange={(event) => setName(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === "Enter" && ready) void send();
          }}
        />
        <div className="ds-urgency" role="radiogroup" aria-label="When">
          {(["now", "later"] as const).map((option) => (
            <button
              type="button"
              key={option}
              role="radio"
              aria-checked={when === option}
              className={when === option ? "is-on" : ""}
              onClick={() => setWhen(option)}
            >
              {option === "now" ? "Now" : "Later"}
            </button>
          ))}
        </div>
        {later && (
          <span className="ds-when">
            <span>at</span>
            <input
              className="ds-input"
              type="time"
              value={startAt}
              aria-label="Starts at"
              onChange={(event) => setStartAt(event.target.value)}
            />
            <span>for</span>
            <input
              className="ds-input ds-when-minutes"
              type="number"
              min={1}
              max={1440}
              value={minutes}
              aria-label="Minutes"
              onChange={(event) => setMinutes(event.target.value)}
            />
            <span>min</span>
          </span>
        )}
        <div className="ds-urgency" role="radiogroup" aria-label="Priority">
          {URGENCY.map((entry) => (
            <button
              type="button"
              key={entry.id}
              role="radio"
              aria-checked={urgency === entry.id}
              className={`${urgency === entry.id ? "is-on" : ""}${entry.id === "top" ? " is-top" : ""}`}
              onClick={() => setUrgency(entry.id)}
              title={`Priority ${entry.priority}`}
            >
              {entry.label}
            </button>
          ))}
        </div>
        <button
          type="button"
          className="ds-btn ds-btn-alarm"
          disabled={!ready}
          onClick={() => void send()}
        >
          <Radio size={16} />
          {!action
            ? "Choose what to send"
            : reach.reached.length === 0
              ? "Choose who"
              : `${later ? "Schedule for" : "Send to"} ${reach.reached.length} ${reach.reached.length === 1 ? "screen" : "screens"}`}
        </button>
      </div>
    </section>
  );
}
