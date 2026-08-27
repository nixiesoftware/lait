/**
 * Channels — what screens are tuned to.
 *
 * This surface did not exist, which made the rest of the product look broken:
 * a screen could be tuned, and a broadcast could redirect a tuning, but there
 * was no way to create the thing being tuned to. Starting from an empty Space
 * you could reach nothing.
 *
 * A channel is a standing stream with its own dayparts. Editing one commits;
 * there is nothing to save.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { Plus, Radio, Tv } from "lucide-react";
import {
  Confirm,
  Empty,
  LiveValue,
  Page,
  PageHeader,
  PageStatus,
  Prompt,
  CommitText,
  Field,
  useCommit,
  useRevision,
  useToast,
} from "@/ds";
import {
  deleteChannel,
  fetchChannels,
  saveChannel,
} from "@/utils/apps/api";
import { fetchPrograms } from "@/utils/broadcasts/api";
import { fetchScreens } from "@/utils/screens/api";
import { mintBodyId } from "@/utils/lait/ids";
import type {
  SignageChannel,
  SignageProgram,
  SignageScreen,
} from "@/utils/lait/types";

export default function ChannelList() {
  const toast = useToast();
  const revision = useRevision();
  const [channels, setChannels] = useState<SignageChannel[]>([]);
  const [programs, setPrograms] = useState<SignageProgram[]>([]);
  const [screens, setScreens] = useState<SignageScreen[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<SignageChannel | null>(null);

  const reload = useCallback(async () => {
    try {
      setError(null);
      const [streams, shows, fleet] = await Promise.all([
        fetchChannels(),
        fetchPrograms(),
        fetchScreens(),
      ]);
      setChannels(streams);
      setPrograms(shows);
      setScreens(fleet);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not load channels");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload, revision]);

  /** How many panels are on each channel, live. */
  const tunedCount = useMemo(() => {
    const counts = new Map<string, number>();
    for (const screen of screens) {
      if (!screen.tuned) continue;
      counts.set(screen.tuned, (counts.get(screen.tuned) ?? 0) + 1);
    }
    return counts;
  }, [screens]);

  return (
    <Page>
      <PageHeader title="Channels" icon={<Radio size={20} />}>
        <button
          type="button"
          className="ds-btn ds-btn-ghost"
          onClick={() => setAdding(true)}
        >
          <Plus size={16} />
          New channel
        </button>
      </PageHeader>

      <PageStatus loading={loading} error={error ?? ""} />

      {!loading && channels.length === 0 ? (
        <Empty title="No channels yet">
          <p className="ds-hint">
            A channel is what a screen is tuned to when nothing is being
            broadcast at it. Make one, point it at a program, and tune your
            panels to it.
          </p>
        </Empty>
      ) : (
        <div className="ds-stack">
          {channels.map((channel) => (
            <ChannelCard
              key={channel.id}
              channel={channel}
              programs={programs}
              tuned={tunedCount.get(channel.id) ?? 0}
              onRemove={() => setRemoving(channel)}
              onError={(message) => toast.show("Could not save", message)}
            />
          ))}
        </div>
      )}

      <Prompt
        open={adding}
        onOpenChange={setAdding}
        title="New channel"
        label="Name"
        placeholder="Downtown Menus"
        confirmLabel="Create"
        onSubmit={(name: string) => {
          void saveChannel({
            id: mintBodyId(),
            name: name.trim() || "Channel",
            base: null,
            schedule: [],
          })
            .then(reload)
            .catch((err: unknown) =>
              toast.show(
                "Could not create",
                err instanceof Error ? err.message : String(err),
              ),
            );
        }}
      />

      <Confirm
        open={removing != null}
        onOpenChange={(open) => {
          if (!open) setRemoving(null);
        }}
        title={`Remove ${removing?.name ?? "this channel"}?`}
        description={
          removing && (tunedCount.get(removing.id) ?? 0) > 0
            ? `${tunedCount.get(removing.id)} screen(s) are tuned to it. They will show nothing until they are tuned somewhere else.`
            : "No screen is tuned to it."
        }
        confirmLabel="Remove"
        danger
        onConfirm={() => {
          const channel = removing;
          if (!channel) return;
          void deleteChannel(channel.id)
            .then(reload)
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

function ChannelCard({
  channel,
  programs,
  tuned,
  onRemove,
  onError,
}: {
  channel: SignageChannel;
  programs: SignageProgram[];
  tuned: number;
  onRemove: () => void;
  onError: (message: string) => void;
}) {
  const put = useCallback(
    async (next: SignageChannel) => {
      try {
        await saveChannel(next);
      } catch (err) {
        onError(err instanceof Error ? err.message : String(err));
        throw err;
      }
    },
    [onError],
  );

  const base = useCommit<string>({
    committed: channel.base ?? "",
    write: (next) => put({ ...channel, base: next || null }),
  });

  return (
    <section className="ds-panel">
      <div className="ds-row-between">
        <ChannelName channel={channel} put={put} />
        <button
          type="button"
          className="ds-btn ds-btn-quiet is-danger"
          onClick={onRemove}
        >
          Remove
        </button>
      </div>

      <div className="ds-row-between">
        <span className="ds-tag">
          <Tv size={11} />
          {/* Live because a panel can be retuned from anywhere. */}
          <LiveValue>
            {tuned === 1 ? "1 screen tuned" : `${tuned} screens tuned`}
          </LiveValue>
        </span>
      </div>

      <Field
        label="Carries"
        commit={base}
        hint="What plays when no daypart of its own is open."
      >
        <select
          className="ds-input"
          value={base.value}
          onChange={(event) => base.setNow(event.target.value)}
        >
          <option value="">Nothing</option>
          {programs.map((program) => (
            <option key={program.id} value={program.id}>
              {program.name}
            </option>
          ))}
        </select>
      </Field>

      <Dayparts channel={channel} programs={programs} put={put} />
    </section>
  );
}

function ChannelName({
  channel,
  put,
}: {
  channel: SignageChannel;
  put: (next: SignageChannel) => Promise<void>;
}) {
  const name = useCommit<string>({
    committed: channel.name,
    write: (next) => put({ ...channel, name: next.trim() || channel.name }),
  });
  return (
    <span style={{ flex: 1, display: "flex", alignItems: "center", gap: 10 }}>
      <input
        className="ds-title-input"
        style={{ fontSize: "var(--ds-fs-heading)" }}
        value={name.value}
        aria-label="Channel name"
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
 * A channel's own hours — "breakfast until eleven, then lunch".
 *
 * This is the channel's business rather than an interruption of it, which is
 * why it lives here and not among broadcasts.
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

  const add = () =>
    void put({
      ...channel,
      schedule: [
        ...windows,
        {
          id: mintBodyId().slice(0, 12),
          program: programs[0]?.id ?? "",
          start_local: `${new Date().toISOString().slice(0, 10)}T09:00:00`,
          duration_ms: 4 * 60 * 60 * 1000,
          recurrence: "daily",
          until_unix_ms: null,
          priority: 0,
          enabled: true,
          timezone: zone,
        },
      ],
    });

  return (
    <div className="ds-stack">
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
        <p className="ds-hint">
          None. The channel carries the program above at all hours.
        </p>
      ) : (
        windows.map((window, index) => (
          <div className="ds-unit" key={window.id}>
            <div className="ds-unit-copy">
              <strong>
                {programs.find((program) => program.id === window.program)?.name ??
                  "an unknown program"}
              </strong>
              <span>
                {window.recurrence} from {window.start_local.slice(11, 16)} for{" "}
                {Math.round(window.duration_ms / 3_600_000)}h · {window.timezone}
              </span>
            </div>
            <button
              type="button"
              className="ds-btn ds-btn-quiet is-danger"
              onClick={() =>
                void put({
                  ...channel,
                  schedule: windows.filter((_, at) => at !== index),
                })
              }
            >
              Remove
            </button>
          </div>
        ))
      )}
    </div>
  );
}
