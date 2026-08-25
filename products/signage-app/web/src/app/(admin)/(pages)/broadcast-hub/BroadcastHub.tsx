/**
 * Transmissions, and who they reach.
 *
 * The one rule this page exists to enforce: **you see the blast radius before
 * you send.** An expressive audience is worth having — the alternative is
 * groups that rot as the fleet grows — but expressiveness without a preview is
 * dangerous precisely because the emergency case is supposed to reach
 * everything. So the screen count, with names, is on the composer, live, and it
 * is computed by the same evaluator that will decide.
 *
 * The count is a lower bound and says so. The World holds no clock and no
 * observations, so a reactive term reaches nobody from here.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { Megaphone, Plus, Radio, X } from "lucide-react";
import {
  Confirm,
  Empty,
  Page,
  PageHeader,
  PageStatus,
  haptic,
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
  }, [reload]);

  const audienceName = (id: string) =>
    audiences.find((entry) => entry.id === id)?.name ?? "an audience";

  const describeAction = (action: BroadcastAction) => {
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

      {!loading && broadcasts.length === 0 ? (
        <Empty title="Nothing is being broadcast">
          <p className="ds-hint">
            Screens are showing whatever channel they are tuned to. A broadcast
            interrupts that for whoever it reaches, and stops on its own.
          </p>
        </Empty>
      ) : null}

      {live.length > 0 && (
        <section className="ds-panel">
          <h3>Live</h3>
          {live.map((broadcast) => {
            const rule = audiences.find((entry) => entry.id === broadcast.audience);
            const reached = rule ? screensReached(rule.rule, screens, audiences) : [];
            return (
              <div className="ds-broadcast" key={broadcast.id}>
                <div className="ds-broadcast-copy">
                  <strong>{broadcast.name}</strong>
                  <span>
                    {describeAction(broadcast.action)} · {audienceName(broadcast.audience)} ·{" "}
                    {reached.length === 1 ? "1 screen" : `${reached.length} screens`}
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
        </section>
      )}

      {stopped.length > 0 && (
        <section className="ds-panel">
          <h3>Stopped</h3>
          <p className="ds-hint">
            Kept rather than deleted, so &ldquo;what interrupted the menus and who
            stopped it&rdquo; stays answerable.
          </p>
          {stopped.map((broadcast) => (
            <div className="ds-broadcast is-quiet" key={broadcast.id}>
              <div className="ds-broadcast-copy">
                <strong>{broadcast.name}</strong>
                <span>{audienceName(broadcast.audience)}</span>
              </div>
              <button
                type="button"
                className="ds-btn ds-btn-quiet"
                onClick={() => {
                  void deleteBroadcast(broadcast.id).then(reload);
                }}
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
              await saveAudience(audience);
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
          void cancelBroadcast(broadcast).then(reload).then(() => haptic("delete"));
        }}
      />
    </Page>
  );
}

/**
 * Compose one transmission: what it does, who it reaches, how loudly.
 *
 * The audience is built as chips over what the fleet actually carries, and the
 * matching screens are named underneath as they are chosen.
 */
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
  onSend: (audience: SignageAudience, broadcast: SignageBroadcast) => void | Promise<void>;
}) {
  const [name, setName] = useState("");
  const [everyone, setEveryone] = useState(false);
  const [labels, setLabels] = useState<string[]>([]);
  const [region, setRegion] = useState("");
  const [action, setAction] = useState<BroadcastAction>({ action: "blank" });
  const [priority, setPriority] = useState(50);

  const available = useMemo(
    () => [...new Set(screens.flatMap((screen) => screen.labels ?? []))].sort(),
    [screens],
  );
  const regions = useMemo(
    () =>
      [...new Set(screens.map((screen) => screen.place?.region).filter(Boolean))].sort() as string[],
    [screens],
  );

  /** Implicit AND across the chosen terms; `all` short-circuits everything. */
  const rule: Match = useMemo(() => {
    if (everyone) return { match: "all" };
    const terms: Match[] = labels.map((label) => ({ match: "label", label }));
    if (region) terms.push({ match: "place", place: { kind: "region", region } });
    if (terms.length === 0) return { match: "all" };
    if (terms.length === 1) return terms[0];
    return { match: "all_of", of: terms };
  }, [everyone, labels, region]);

  const reached = useMemo(
    () => screensReached(rule, screens, audiences),
    [rule, screens, audiences],
  );

  const nothingChosen = !everyone && labels.length === 0 && !region;

  return (
    <section className="ds-composer" aria-label="New broadcast">
      <header>
        <strong>New broadcast</strong>
        <button type="button" className="ds-icon" onClick={onClose} aria-label="Close">
          <X size={18} />
        </button>
      </header>

      <label className="ds-field">
        <span>Name</span>
        <input
          className="ds-input"
          value={name}
          placeholder="Evacuation"
          onChange={(event) => setName(event.target.value)}
        />
      </label>

      <label className="ds-field">
        <span>What it does</span>
        <select
          className="ds-input"
          value={action.action}
          onChange={(event) => {
            const next = event.target.value;
            if (next === "play") {
              setAction({ action: "play", program: programs[0]?.id ?? "" });
            } else if (next === "tune") {
              setAction({ action: "tune", channel: channels[0]?.id ?? "" });
            } else if (next === "restore") {
              setAction({ action: "restore" });
            } else {
              setAction({ action: "blank" });
            }
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
          <span>Program</span>
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
          <span>Channel</span>
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

      <div className="ds-field is-block">
        <span>Who it reaches</span>
        <button
          type="button"
          className={`ds-chip${everyone ? " is-on" : ""}`}
          onClick={() => setEveryone(!everyone)}
        >
          Every screen
        </button>
        {!everyone && (
          <>
            <div className="ds-chips">
              {available.map((label) => (
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
                </button>
              ))}
            </div>
            {regions.length > 0 && (
              <select
                className="ds-input"
                value={region}
                onChange={(event) => setRegion(event.target.value)}
              >
                <option value="">Any region</option>
                {regions.map((held) => (
                  <option key={held} value={held}>
                    {held}
                  </option>
                ))}
              </select>
            )}
          </>
        )}
      </div>

      {/* The blast radius. Non-negotiable, and shown before anything is sent. */}
      <div className={`ds-reach${reached.length > 0 ? " is-live" : ""}`}>
        <Megaphone size={16} />
        <div>
          <strong>
            {nothingChosen && !everyone
              ? "Nothing chosen — this would reach every screen"
              : reached.length === 1
                ? "This will interrupt 1 screen"
                : `This will interrupt ${reached.length} screens`}
          </strong>
          <span>
            {reached.length === 0
              ? "No screen matches."
              : reached
                  .slice(0, 8)
                  .map((screen) => screen.name)
                  .join(", ") + (reached.length > 8 ? `, and ${reached.length - 8} more` : "")}
          </span>
          <small>At least these — a reactive term is not counted from here.</small>
        </div>
      </div>

      <label className="ds-field">
        <span>Priority</span>
        <input
          className="ds-input"
          type="number"
          min={0}
          max={100}
          value={priority}
          onChange={(event) => setPriority(Number(event.target.value))}
        />
        <small className="ds-hint">
          Higher wins when two broadcasts reach one screen. An emergency belongs
          near the top.
        </small>
      </label>

      <footer>
        <button type="button" className="ds-btn ds-btn-quiet" onClick={onClose}>
          Cancel
        </button>
        <button
          type="button"
          className="ds-btn ds-btn-solid"
          disabled={reached.length === 0}
          onClick={() => {
            const audience = draftAudience(name.trim() || "Audience", rule);
            void onSend(audience, {
              id: mintBodyId(),
              name: name.trim() || "Broadcast",
              audience: audience.id,
              action,
              timing: { timing: "when", of: { match: "all" }, priority },
              supersedes: [],
              cancelled_at_unix_ms: null,
            });
          }}
        >
          Send to {reached.length === 1 ? "1 screen" : `${reached.length} screens`}
        </button>
      </footer>
    </section>
  );
}
