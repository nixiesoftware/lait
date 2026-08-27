/**
 * Transmissions, and who they reach.
 *
 * One rule governs this page: **you see the blast radius before you send, and
 * you see who you miss.**
 *
 * Every audience builder in the reference set counts who a message reaches —
 * Shopify gives a percentage of base, Loops and Zendesk name the actual
 * matches, Apollo does chips per facet. None of them show the complement,
 * because for marketing an unreached contact is a wasted impression. For an
 * evacuation it is the whole failure. So this states both, and the miss is not
 * coloured as an error, because missing the office screen may well be intended.
 *
 * Facet chips carry their own size before you pick them, which is Partiful's
 * move and strictly better than making somebody commit to a filter to find out
 * how big it is.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { Megaphone, Plus, Radio, X } from "lucide-react";
import {
  Confirm,
  Empty,
  OnAir,
  Page,
  PageHeader,
  PageStatus,
  haptic,
  useLive,
  useRevision,
  useToast,
} from "@/ds";
import {
  cancelBroadcast,
  deleteBroadcast,
  draftAudience,
  fetchAudiences,
  fetchBroadcasts,
  fetchChannels,
  saveAudience,
  saveBroadcast,
} from "@/utils/apps/api";
import { fetchPrograms } from "@/utils/broadcasts/api";
import { fetchScreens } from "@/utils/screens/api";
import { screensReached } from "@/utils/lait/resolve";
import { mintBodyId } from "@/utils/lait/ids";
import type {
  BroadcastAction,
  Match,
  SignageAudience,
  SignageBroadcast,
  SignageChannel,
  SignageProgram,
  SignageScreen,
} from "@/utils/lait/types";

export default function BroadcastHub() {
  const toast = useToast();
  const revision = useRevision();
  const [broadcasts, setBroadcasts] = useState<SignageBroadcast[]>([]);
  const [audiences, setAudiences] = useState<SignageAudience[]>([]);
  const [screens, setScreens] = useState<SignageScreen[]>([]);
  const [programs, setPrograms] = useState<SignageProgram[]>([]);
  const [channels, setChannels] = useState<SignageChannel[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [composing, setComposing] = useState(false);
  const [stopping, setStopping] = useState<SignageBroadcast | null>(null);

  const reload = useCallback(async () => {
    try {
      setError(null);
      const [live, sets, fleet, shows, streams] = await Promise.all([
        fetchBroadcasts(),
        fetchAudiences(),
        fetchScreens(),
        fetchPrograms(),
        fetchChannels(),
      ]);
      setBroadcasts(live);
      setAudiences(sets);
      setScreens(fleet);
      setPrograms(shows);
      setChannels(streams);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not load broadcasts");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload, revision]);

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

  const live = broadcasts.filter((entry) => entry.cancelled_at_unix_ms == null);
  const stopped = broadcasts.filter((entry) => entry.cancelled_at_unix_ms != null);

  return (
    <Page>
      <PageHeader title="Broadcasts" icon={<Radio size={20} />}>
        <button
          type="button"
          className="ds-btn ds-btn-solid"
          onClick={() => setComposing(true)}
        >
          <Plus size={16} />
          New broadcast
        </button>
      </PageHeader>

      <PageStatus loading={loading} error={error ?? ""} />

      {!loading && broadcasts.length === 0 && (
        <Empty title="Nothing is being broadcast">
          <p className="ds-hint">
            Every screen is showing the channel it is tuned to. A broadcast
            interrupts that for whoever it reaches, and stops on its own.
          </p>
        </Empty>
      )}

      <div className="ds-stack">
        {live.map((broadcast) => {
          const rule = audiences.find((entry) => entry.id === broadcast.audience);
          const reached = rule ? screensReached(rule.rule, screens, audiences) : [];
          return (
            <div className="ds-unit is-onair" key={broadcast.id}>
              <OnAir label="ON AIR" tone="alarm" />
              <div className="ds-unit-copy">
                <strong>{broadcast.name}</strong>
                <span>
                  {describe(broadcast.action)} · reaching {reached.length} of{" "}
                  {screens.length}
                </span>
              </div>
              <button
                type="button"
                className="ds-btn ds-btn-quiet is-danger"
                onClick={() => setStopping(broadcast)}
              >
                Stop
              </button>
            </div>
          );
        })}
      </div>

      {stopped.length > 0 && (
        <section className="ds-panel" style={{ marginTop: 18 }}>
          <h3>Stopped</h3>
          <p className="ds-hint">
            Kept rather than deleted, so “what interrupted the menus at 14:30 and
            who stopped it” stays answerable.
          </p>
          {stopped.map((broadcast) => (
            <div className="ds-row-between" key={broadcast.id}>
              <span style={{ fontSize: "var(--ds-text-sm)" }}>{broadcast.name}</span>
              <button
                type="button"
                className="ds-btn ds-btn-quiet"
                onClick={() => void deleteBroadcast(broadcast.id).then(reload)}
              >
                Delete
              </button>
            </div>
          ))}
        </section>
      )}

      {composing && (
        <Composer
          screens={screens}
          audiences={audiences}
          programs={programs}
          channels={channels}
          onClose={() => setComposing(false)}
          onSend={async (audience, broadcast) => {
            try {
              if (audience) await saveAudience(audience);
              await saveBroadcast(broadcast);
              haptic("save");
              setComposing(false);
              await reload();
            } catch (err) {
              toast.show(
                "Could not send",
                err instanceof Error ? err.message : String(err),
              );
            }
          }}
        />
      )}

      <Confirm
        open={stopping != null}
        onOpenChange={(open) => {
          if (!open) setStopping(null);
        }}
        title={`Stop ${stopping?.name ?? "this broadcast"}?`}
        description="Screens go back to the channel they are tuned to. The record is kept."
        confirmLabel="Stop"
        danger
        onConfirm={() => {
          const broadcast = stopping;
          if (!broadcast) return;
          void cancelBroadcast(broadcast)
            .then(reload)
            .then(() => haptic("delete"));
        }}
      />
    </Page>
  );
}

type Reach = { reached: SignageScreen[]; missed: SignageScreen[] };

function Composer({
  screens,
  audiences,
  programs,
  channels,
  onClose,
  onSend,
}: {
  screens: SignageScreen[];
  audiences: SignageAudience[];
  programs: SignageProgram[];
  channels: SignageChannel[];
  onClose: () => void;
  onSend: (
    audience: SignageAudience | null,
    broadcast: SignageBroadcast,
  ) => void | Promise<void>;
}) {
  const { now } = useLive();
  const [name, setName] = useState("");
  /** Explicit, never implied by an empty filter — Okta's everyone-vs-subset. */
  const [everyone, setEveryone] = useState(false);
  const [reuse, setReuse] = useState<string>("");
  const [labels, setLabels] = useState<string[]>([]);
  const [region, setRegion] = useState("");
  const [action, setAction] = useState<BroadcastAction>({ action: "blank" });
  const [priority, setPriority] = useState(50);

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

  const regions = useMemo(() => {
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
    const terms: Match[] = labels.map((label) => ({ match: "label", label }));
    if (region) terms.push({ match: "place", place: { kind: "region", region } });
    if (terms.length === 0) return null;
    if (terms.length === 1) return terms[0];
    return { match: "all_of", of: terms };
  }, [reuse, everyone, labels, region, audiences]);

  const reach: Reach = useMemo(() => {
    if (!rule) return { reached: [], missed: screens };
    const reached = screensReached(rule, screens, audiences);
    const hit = new Set(reached.map((screen) => screen.id));
    return { reached, missed: screens.filter((screen) => !hit.has(screen.id)) };
  }, [rule, screens, audiences]);

  const share = screens.length === 0 ? 0 : reach.reached.length / screens.length;

  return (
    <section className="ds-composer" aria-label="New broadcast">
      <header>
        <strong>New broadcast</strong>
        <button type="button" className="ds-icon" onClick={onClose} aria-label="Close">
          <X size={18} />
        </button>
      </header>

      <label className="ds-field">
        <span className="ds-field-label">Name</span>
        <input
          className="ds-input"
          value={name}
          placeholder="Evacuation"
          onChange={(event) => setName(event.target.value)}
        />
      </label>

      <label className="ds-field">
        <span className="ds-field-label">What it does</span>
        <select
          className="ds-input"
          value={action.action}
          onChange={(event) => {
            const next = event.target.value;
            if (next === "play") setAction({ action: "play", program: programs[0]?.id ?? "" });
            else if (next === "tune") setAction({ action: "tune", channel: channels[0]?.id ?? "" });
            else if (next === "restore") setAction({ action: "restore" });
            else setAction({ action: "blank" });
          }}
        >
          <option value="play">Play a program</option>
          <option value="tune">Tune to a channel</option>
          <option value="blank">Go dark</option>
          <option value="restore">All clear</option>
        </select>
      </label>

      {action.action === "play" && (
        <label className="ds-field">
          <span className="ds-field-label">Program</span>
          <select
            className="ds-input"
            value={action.program}
            onChange={(event) => setAction({ action: "play", program: event.target.value })}
          >
            {programs.map((program) => (
              <option key={program.id} value={program.id}>
                {program.name}
              </option>
            ))}
          </select>
        </label>
      )}

      {action.action === "tune" && (
        <label className="ds-field">
          <span className="ds-field-label">Channel</span>
          <select
            className="ds-input"
            value={action.channel}
            onChange={(event) => setAction({ action: "tune", channel: event.target.value })}
          >
            {channels.map((channel) => (
              <option key={channel.id} value={channel.id}>
                {channel.name}
              </option>
            ))}
          </select>
        </label>
      )}

      <div className="ds-field">
        <span className="ds-field-label">Who it reaches</span>

        {audiences.length > 0 && (
          <select
            className="ds-input"
            value={reuse}
            onChange={(event) => {
              setReuse(event.target.value);
              setEveryone(false);
            }}
          >
            <option value="">Build one below</option>
            {audiences.map((audience) => (
              <option key={audience.id} value={audience.id}>
                Reuse: {audience.name}
              </option>
            ))}
          </select>
        )}

        {!reuse && (
          <>
            <div className="ds-chips" style={{ marginTop: 6 }}>
              <button
                type="button"
                className={`ds-chip${everyone ? " is-on" : ""}`}
                onClick={() => setEveryone(!everyone)}
              >
                Every screen<em>{screens.length}</em>
              </button>
            </div>
            {!everyone && (
              <>
                <div className="ds-chips" style={{ marginTop: 6 }}>
                  {facets.map(([label, count]) => (
                    <button
                      type="button"
                      key={label}
                      className={`ds-chip${labels.includes(label) ? " is-on" : ""}`}
                      onClick={() =>
                        setLabels(
                          labels.includes(label)
                            ? labels.filter((held) => held !== label)
                            : [...labels, label],
                        )
                      }
                    >
                      {label}
                      <em>{count}</em>
                    </button>
                  ))}
                </div>
                {regions.length > 0 && (
                  <select
                    className="ds-input"
                    style={{ marginTop: 6 }}
                    value={region}
                    onChange={(event) => setRegion(event.target.value)}
                  >
                    <option value="">Any region</option>
                    {regions.map(([held, count]) => (
                      <option key={held} value={held}>
                        {held} ({count})
                      </option>
                    ))}
                  </select>
                )}
              </>
            )}
          </>
        )}
      </div>

      {/* The blast radius, both halves. */}
      <div className="ds-reach">
        <div className="ds-reach-count">
          <Megaphone size={18} />
          <span>
            {reach.reached.length} of {screens.length}
          </span>
          <small>{Math.round(share * 100)}% of the fleet</small>
        </div>
        <div className="ds-reach-bar">
          <span style={{ width: `${share * 100}%` }} />
        </div>

        {reach.reached.length > 0 && (
          <div className="ds-reach-names">
            {reach.reached.map((screen) => (
              <span className="ds-tag is-reached" key={screen.id}>
                {screen.name}
              </span>
            ))}
          </div>
        )}

        {/* The half nobody else shows. Under-reach is the failure that matters
            when the message is "evacuate". */}
        {reach.missed.length > 0 && (
          <div className="ds-reach-miss">
            <strong>
              Not reached — {reach.missed.length}
            </strong>
            <div className="ds-reach-names">
              {reach.missed.map((screen) => (
                <span className="ds-tag is-miss" key={screen.id}>
                  {screen.name}
                </span>
              ))}
            </div>
          </div>
        )}

        {!rule && (
          <p className="ds-hint">
            Nothing chosen yet. Pick a slice, or say every screen — an empty
            filter is not treated as “everyone”.
          </p>
        )}
      </div>

      <label className="ds-field">
        <span className="ds-field-label">Priority</span>
        <input
          className="ds-input"
          type="number"
          min={0}
          max={100}
          value={priority}
          onChange={(event) => setPriority(Number(event.target.value))}
        />
        <span className="ds-field-hint">
          Higher wins when two broadcasts reach one screen. An emergency belongs
          near the top.
        </span>
      </label>

      <footer>
        <button type="button" className="ds-btn ds-btn-quiet" onClick={onClose}>
          Cancel
        </button>
        <button
          type="button"
          className="ds-btn ds-btn-alarm"
          disabled={!rule || reach.reached.length === 0}
          onClick={() => {
            if (!rule) return;
            const existing = reuse
              ? (audiences.find((entry) => entry.id === reuse) ?? null)
              : null;
            const audience = existing ?? draftAudience(name.trim() || "Audience", rule);
            void onSend(existing ? null : audience, {
              id: mintBodyId(),
              name: name.trim() || "Broadcast",
              audience: audience.id,
              action,
              timing: { timing: "when", of: { match: "all" }, priority },
              supersedes: [],
              cancelled_at_unix_ms: now * 0 || null,
            });
          }}
        >
          Send to {reach.reached.length}{" "}
          {reach.reached.length === 1 ? "screen" : "screens"}
        </button>
      </footer>
    </section>
  );
}
