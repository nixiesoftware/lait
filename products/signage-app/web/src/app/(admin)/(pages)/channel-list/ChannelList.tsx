/**
 * Channels — what screens are tuned to.
 *
 * A channel is a standing stream with its own dayparts. Everything on a card
 * is the value it changes: the program it carries is a row of covers and you
 * press the one you mean; a daypart's hours are the controls that set them;
 * the name is the title. Nothing is saved, because nothing is a form.
 *
 * Making one is a press: the card appears under a name the page hands it,
 * with that name selected, so the first keystroke is the rename.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { Plus, Radio, X } from "lucide-react";
import {
  Bezel,
  Cover,
  DayTrack,
  Empty,
  Page,
  PageHeader,
  PageStatus,
  channelDay,
  civilDateIn,
  haptic,
  litProps,
  useCommit,
  useFocus,
  useHoldable,
  useLive,
  useOrbit,
  useToast,
  useUndo,
} from "@/ds";
import { putChannel, removeChannel, useFleet, type Fleet } from "@/utils/screens/fleet";
import { Thumb } from "@/program-editor/Thumb";
import { mintBodyId } from "@/utils/lait/ids";
import type {
  ChannelWindowOnWire,
  SignageChannel,
  SignageMedia,
  SignageProgram,
} from "@/utils/lait/types";

export default function ChannelList() {
  const toast = useToast();
  const undo = useUndo();
  const fleet = useFleet();
  const { channels, programs, loading, error } = fleet;
  const [justMade, setJustMade] = useState<string | null>(null);

  const refused = (what: string) => (err: unknown) => {
    haptic("error");
    toast.show(what, err instanceof Error ? err.message : String(err));
  };

  const create = () => {
    const channel: SignageChannel = {
      id: mintBodyId(),
      name: "New channel",
      base: programs[0]?.id ?? null,
      schedule: [],
    };
    setJustMade(channel.id);
    haptic("save");
    void putChannel(channel).catch(refused("Could not make a channel"));
  };

  const remove = (channel: SignageChannel) => {
    haptic("delete");
    const tuned = fleet.tunedTo(channel.id).length;
    void removeChannel(channel.id)
      .then((was) => {
        if (!was) return;
        undo.offer(
          tuned > 0
            ? `Removed ${was.name} — ${tuned} ${tuned === 1 ? "screen is" : "screens are"} untuned`
            : `Removed ${was.name}`,
          () => putChannel(was),
        );
      })
      .catch(refused("Could not remove"));
  };

  const sorted = useMemo(
    () => [...channels].sort((a, b) => a.name.localeCompare(b.name)),
    [channels],
  );

  return (
    <Page>
      <PageHeader title="Channels" icon={<Radio size={20} />}>
        <button type="button" className="ds-btn ds-btn-solid" onClick={create}>
          <Plus size={16} />
          New channel
        </button>
      </PageHeader>

      <PageStatus loading={loading} error={error ?? ""} />

      {!loading && channels.length === 0 ? (
        <Empty title="No channels yet">
          <p className="ds-hint">
            A channel is what a screen is tuned to when nothing is being broadcast at
            it. Make one, point it at a program, and tune your panels to it.
          </p>
          <button type="button" className="ds-btn ds-btn-solid" onClick={create}>
            <Plus size={16} />
            New channel
          </button>
        </Empty>
      ) : (
        <div className="ds-stack">
          {sorted.map((channel) => (
            <ChannelCard
              key={channel.id}
              channel={channel}
              programs={programs}
              media={fleet.media}
              fleet={fleet}
              fresh={channel.id === justMade}
              onRemove={() => remove(channel)}
              onError={(message) => toast.show("Could not save", message)}
            />
          ))}
        </div>
      )}
    </Page>
  );
}

function ChannelCard({
  channel,
  programs,
  media,
  fleet,
  fresh,
  onRemove,
  onError,
}: {
  channel: SignageChannel;
  programs: SignageProgram[];
  media: SignageMedia[];
  fleet: Fleet;
  fresh: boolean;
  onRemove: () => void;
  onError: (message: string) => void;
}) {
  const navigate = useNavigate();
  const orbit = useOrbit();
  const { now } = useLive();
  const { held } = useFocus();
  const hold = useHoldable("channel", channel.id);
  const tuned = fleet.tunedTo(channel.id);
  const day = useMemo(
    () =>
      channelDay(
        channel,
        now,
        (program) =>
          void navigate({ to: "/broadcast-list/broadcast/$id", params: { id: program } }),
        (program) => programs.find((entry) => entry.id === program)?.name,
      ),
    [channel, now, navigate, programs],
  );
  const put = useCallback(
    async (next: SignageChannel) => {
      try {
        await putChannel(next);
      } catch (err) {
        onError(err instanceof Error ? err.message : String(err));
        throw err;
      }
    },
    [onError],
  );

  const mediaMap = useMemo(() => new Map(media.map((entry) => [entry.id, entry])), [media]);
  const coverOf = (program: SignageProgram) =>
    program.items.slice(0, 4).map((item) => (
      <Thumb key={item.id} media={mediaMap.get(item.media)} orbit={orbit} />
    ));

  return (
    <section className={`ds-panel${fresh ? " ds-arrive" : ""}`} {...hold.bind}>
      <div className="ds-row-between">
        <ChannelName channel={channel} put={put} select={fresh} />
        <button type="button" className="ds-btn ds-btn-quiet is-danger" onClick={onRemove}>
          Remove
        </button>
      </div>

      {/* The channel's day, as it is authored: its ground and its dayparts. An
          empty track says "carries nothing" without a sentence. */}
      <DayTrack segments={day} now={now} />

      {/* The screens tuned to it, drawn as themselves — attached, not counted.
          Holding the channel lights them. */}
      {tuned.length > 0 && (
        <div
          className="ds-attached"
          {...litProps(held, held?.kind === "channel" && held.id === channel.id)}
        >
          {tuned.map((screen) => (
            <button
              type="button"
              key={screen.id}
              className="ds-attached-hit"
              title={screen.name}
              aria-label={screen.name}
              onClick={() => void navigate({ to: "/screen-list/$id", params: { id: screen.id } })}
            >
              <Bezel
                size="xs"
                screen={screen}
                playback={fleet.playbackFor(screen, now)}
                programs={fleet.programs}
                media={fleet.media}
                presets={fleet.presets}
                orbit={orbit}
                now={now}
              />
            </button>
          ))}
        </div>
      )}

      {/* What it carries: press the cover. The press is the commit. */}
      <div className="ds-stack" style={{ gap: 8 }}>
        <span className="ds-field-label">Carries, when no daypart is open</span>
        <div className="ds-carry" role="radiogroup" aria-label="Carries">
          {programs.map((program) => {
            const on = channel.base === program.id;
            return (
              <button
                type="button"
                key={program.id}
                role="radio"
                aria-checked={on}
                className={`ds-carry-pick${on ? " is-on" : ""}`}
                onClick={() => {
                  if (on) return;
                  haptic("select");
                  void put({ ...channel, base: program.id }).catch(() => undefined);
                }}
              >
                <Cover>{coverOf(program)}</Cover>
                {program.name}
              </button>
            );
          })}
          <button
            type="button"
            role="radio"
            aria-checked={!channel.base}
            className={`ds-carry-pick is-none${!channel.base ? " is-on" : ""}`}
            onClick={() => {
              if (!channel.base) return;
              void put({ ...channel, base: null }).catch(() => undefined);
            }}
          >
            Nothing
          </button>
          {programs.length === 0 && (
            <span className="ds-hint">No programs yet — make one under Programs.</span>
          )}
        </div>
      </div>

      <Dayparts channel={channel} programs={programs} put={put} />
    </section>
  );
}

function ChannelName({
  channel,
  put,
  select,
}: {
  channel: SignageChannel;
  put: (next: SignageChannel) => Promise<void>;
  select: boolean;
}) {
  const ref = useRef<HTMLInputElement>(null);
  const name = useCommit<string>({
    committed: channel.name,
    write: (next) => put({ ...channel, name: next.trim() || channel.name }),
  });
  useEffect(() => {
    if (select) ref.current?.select();
  }, [select]);
  return (
    <span style={{ flex: 1, display: "flex", alignItems: "center", gap: 10 }}>
      <input
        ref={ref}
        className="ds-title-input"
        style={{ fontSize: "var(--ds-fs-heading)" }}
        value={name.value}
        aria-label="Channel name"
        onChange={(event) => name.set(event.target.value)}
        onBlur={() => {
          if (name.state === "pending") name.setNow(name.value);
        }}
        onKeyDown={(event) => {
          if (event.key === "Enter") (event.target as HTMLInputElement).blur();
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

const HOUR_MS = 60 * 60 * 1000;

/**
 * A channel's own hours — "breakfast until eleven, then lunch".
 *
 * Each daypart is drawn as the sentence it is, and every word of the sentence
 * is the control that changes it. Adding one is a press; the hours it lands
 * with are a guess the person corrects, which is quicker than a blank.
 */
function Dayparts({
  channel,
  programs,
  put,
}: {
  channel: SignageChannel;
  programs: SignageProgram[];
  put: (next: SignageChannel) => Promise<void>;
}) {
  const windows = channel.schedule ?? [];
  const zone = Intl.DateTimeFormat().resolvedOptions().timeZone;

  const write = (schedule: ChannelWindowOnWire[]) =>
    void put({ ...channel, schedule }).catch(() => undefined);

  const patch = (index: number, change: Partial<ChannelWindowOnWire>) =>
    write(windows.map((window, at) => (at === index ? { ...window, ...change } : window)));

  const add = () => {
    haptic("select");
    // The next free hour after the last daypart, or nine, so two presses
    // make two dayparts that do not sit on top of each other.
    const last = windows[windows.length - 1];
    const startHour = last
      ? Math.min(20, Number(last.start_local.slice(11, 13)) + Math.round(last.duration_ms / HOUR_MS))
      : 9;
    write([
      ...windows,
      {
        id: mintBodyId().slice(0, 12),
        program: programs.find((program) => program.id !== channel.base)?.id ?? programs[0]?.id ?? "",
        // The civil date in the daypart's own zone. `toISOString()` gave the
        // UTC date, which after 19:00 Central is already tomorrow — a "once"
        // daypart authored in the evening never opened that evening.
        start_local: `${civilDateIn(Date.now(), zone)}T${String(startHour).padStart(2, "0")}:00:00`,
        duration_ms: 3 * HOUR_MS,
        recurrence: "daily",
        until_unix_ms: null,
        priority: 0,
        enabled: true,
        timezone: zone,
      },
    ]);
  };

  return (
    <div className="ds-stack" style={{ gap: 8 }}>
      <div className="ds-row-between">
        <span className="ds-field-label">Dayparts</span>
        <button
          type="button"
          className="ds-btn ds-btn-quiet"
          onClick={add}
          disabled={programs.length === 0}
        >
          <Plus size={14} />
          Add a daypart
        </button>
      </div>
      {windows.length === 0 ? (
        <p className="ds-hint">None. The channel carries the program above at all hours.</p>
      ) : (
        windows.map((window, index) => (
          <div className="ds-daypart" key={window.id}>
            <div className="ds-daypart-when">
              <select
                className="ds-input"
                value={window.program}
                aria-label="Program"
                onChange={(event) => patch(index, { program: event.target.value })}
              >
                {programs.map((program) => (
                  <option key={program.id} value={program.id}>
                    {program.name}
                  </option>
                ))}
              </select>
              <select
                className="ds-input"
                value={window.recurrence}
                aria-label="Repeats"
                onChange={(event) =>
                  patch(index, { recurrence: event.target.value as ChannelWindowOnWire["recurrence"] })
                }
              >
                <option value="daily">daily</option>
                <option value="weekly">weekly</option>
                <option value="monthly">monthly</option>
                <option value="none">once</option>
              </select>
              <span>from</span>
              <input
                className="ds-input"
                type="time"
                value={window.start_local.slice(11, 16)}
                aria-label="From"
                onChange={(event) => {
                  const time = event.target.value;
                  if (!time) return;
                  patch(index, { start_local: `${window.start_local.slice(0, 10)}T${time}:00` });
                }}
              />
              <span>for</span>
              <input
                className="ds-input"
                type="number"
                min={1}
                max={24}
                step={1}
                value={Math.max(1, Math.round(window.duration_ms / HOUR_MS))}
                aria-label="Hours"
                onChange={(event) => {
                  const hours = Number(event.target.value);
                  if (!Number.isFinite(hours) || hours < 1) return;
                  patch(index, { duration_ms: Math.min(24, hours) * HOUR_MS });
                }}
              />
              <span>{Math.round(window.duration_ms / HOUR_MS) === 1 ? "hour" : "hours"} · {window.timezone}</span>
            </div>
            <button
              type="button"
              className="ds-icon"
              aria-label="Remove this daypart"
              onClick={() => {
                haptic("delete");
                write(windows.filter((_, at) => at !== index));
              }}
            >
              <X size={16} />
            </button>
          </div>
        ))
      )}
    </div>
  );
}
