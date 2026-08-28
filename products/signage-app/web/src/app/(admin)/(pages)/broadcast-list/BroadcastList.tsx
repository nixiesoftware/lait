/**
 * Programs — what a channel carries and a broadcast plays.
 *
 * A row is the program's cover, its length, and its relations drawn rather
 * than counted: the channels that carry it, each with where in their day it
 * sits, and the screens showing it right now, drawn as themselves. Both come
 * from the same held copy every other page reads, resolved by the same ladder
 * the World uses — so there is no second request per row and no moment where
 * this page disagrees with the one beside it.
 *
 * Deleting is a press and an offer to put it back. Nothing asks.
 */

import React, { useMemo, useState } from "react";
import { useNavigate, useSearch } from "@tanstack/react-router";
import { Clapperboard, Copy, Plus, Trash2, Tv } from "lucide-react";
import {
  Bezel,
  CommitText,
  Cover,
  DayTrack,
  Empty,
  Inspector,
  Page,
  PageHeader,
  PageSearch,
  PageStatus,
  PlaylistRow,
  PlaylistTile,
  SelectionBar,
  ViewToggle,
  channelDay,
  haptic,
  litProps,
  useFocus,
  useHoldable,
  useLive,
  useOrbit,
  useToast,
  useUndo,
  useWide,
  type MenuItem,
} from "@/ds";
import { putProgram, removeProgram, useFleet } from "@/utils/screens/fleet";
import { Thumb } from "@/program-editor/Thumb";
import { formatDuration, itemDurationMs } from "@/program-editor/model";
import { mintBodyId } from "@/utils/lait/ids";
import type { SignageProgram } from "@/utils/lait/types";
import { useCreateBroadcast } from "@/utils/navigation/hooks";

export const BroadcastListPage: React.FC = () => {
  const navigate = useNavigate();
  const { q: searchQuery } = useSearch({ strict: false }) as { q?: string };
  const toast = useToast();
  const undo = useUndo();
  const orbit = useOrbit();
  const wide = useWide();
  const fleet = useFleet();
  const { now } = useLive();
  const { held } = useFocus();

  const [query, setQuery] = useState(searchQuery || "");
  const [viewMode, setViewMode] = useState<"grid" | "list">("list");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [inspect, setInspect] = useState<string | null>(null);

  const { handleCreate, isCreating } = useCreateBroadcast();

  const mediaMap = useMemo(
    () => new Map(fleet.media.map((media) => [media.id, media])),
    [fleet.media],
  );

  const refused = (what: string) => (err: unknown) => {
    haptic("error");
    toast.show(what, err instanceof Error ? err.message : String(err));
  };

  const openEditor = (id: string) =>
    void navigate({ to: "/broadcast-list/broadcast/$id", params: { id } });

  const makeCopy = (program: SignageProgram) => {
    const itemIds = new Map(program.items.map((item) => [item.id, mintBodyId()]));
    const copy: SignageProgram = {
      ...program,
      id: mintBodyId(),
      name: `${program.name} (copy)`,
      items: program.items.map((item) => ({ ...item, id: itemIds.get(item.id)! })),
      windows: (program.windows ?? []).map((window) => ({
        ...window,
        id: mintBodyId(),
        items: window.items.flatMap((id) => {
          const mapped = itemIds.get(id);
          return mapped ? [mapped] : [];
        }),
      })),
    };
    haptic("save");
    void putProgram(copy).catch(refused("Could not copy"));
  };

  const remove = (ids: string[]) => {
    haptic("delete");
    setSelected(new Set());
    if (inspect && ids.includes(inspect)) setInspect(null);
    void Promise.all(ids.map((id) => removeProgram(id)))
      .then((gone) => {
        const kept = gone.filter((row): row is SignageProgram => row != null);
        if (kept.length === 0) return;
        undo.offer(
          kept.length === 1 ? `Deleted ${kept[0].name}` : `Deleted ${kept.length} programs`,
          () => Promise.all(kept.map((row) => putProgram(row))),
        );
      })
      .catch(refused("Could not delete"));
  };

  const toggle = (id: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const rows = useMemo(() => {
    const q = query.trim().toLowerCase();
    return fleet.programs
      .filter((row) => !q || row.name.toLowerCase().includes(q))
      .sort((a, b) => a.name.localeCompare(b.name));
  }, [fleet.programs, query]);

  const menuFor = (row: SignageProgram): MenuItem[] => [
    { label: "Open", onPick: () => openEditor(row.id) },
    { label: "Details", onPick: () => setInspect(row.id) },
    { label: "Make a copy", onPick: () => makeCopy(row) },
    { label: "Delete", danger: true, onPick: () => remove([row.id]) },
  ];

  /** The channels that carry this program, and where in their day it sits. */
  const carriedBy = (row: SignageProgram) =>
    fleet.channels.filter(
      (channel) =>
        channel.base === row.id ||
        (channel.schedule ?? []).some((part) => part.program === row.id),
    );

  /** The screens showing it right now, by the same ladder the World uses. */
  const showingOn = (row: SignageProgram) =>
    fleet.screens.filter((screen) => {
      const showing = fleet.playbackFor(screen, now).showing;
      return showing.showing === "program" && showing.program === row.id;
    });

  const relations = (row: SignageProgram) => {
    const channels = carriedBy(row);
    const screens = showingOn(row);
    if (channels.length === 0 && screens.length === 0) return null;
    return (
      <span className="ds-constellation" onClick={(event) => event.stopPropagation()}>
        {channels.map((channel) => (
          <span
            key={channel.id}
            className="ds-carried"
            title={`Carried by ${channel.name}`}
            {...litProps(held, held?.kind === "channel" && held.id === channel.id)}
          >
            <Tv size={12} />
            {channel.name}
            <DayTrack
              size="sm"
              now={now}
              segments={channelDay(channel, now).map((segment) => ({
                ...segment,
                onOpen: undefined,
                tone:
                  segment.id === `base:${row.id}` ||
                  (channel.schedule ?? []).some(
                    (part) => part.id === segment.id && part.program === row.id,
                  )
                    ? segment.tone
                    : "ground",
              }))}
            />
          </span>
        ))}
        {screens.length > 0 && (
          <span className="ds-attached">
            {screens.map((screen) => (
              <button
                type="button"
                key={screen.id}
                className="ds-attached-hit"
                title={`Showing on ${screen.name}`}
                aria-label={`Showing on ${screen.name}`}
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
          </span>
        )}
      </span>
    );
  };

  const coverCells = (row: SignageProgram) =>
    row.items.slice(0, 4).map((item) => (
      <Thumb key={item.id} media={mediaMap.get(item.media)} orbit={orbit} />
    ));

  const meta = (row: SignageProgram) => {
    const ms = row.items.reduce(
      (sum, item) => sum + itemDurationMs(item, mediaMap.get(item.media)),
      0,
    );
    const clips = `${row.items.length} ${row.items.length === 1 ? "clip" : "clips"}`;
    return `${clips} · ${formatDuration(ms)}`;
  };

  const showList = !wide || viewMode === "list";
  const inspected = inspect ? fleet.programs.find((row) => row.id === inspect) ?? null : null;

  return (
    <Page>
      <PageHeader title="Programs" icon={<Clapperboard size={20} />}>
        <button
          type="button"
          className="ds-btn ds-btn-solid"
          disabled={isCreating}
          onClick={() => void handleCreate()}
        >
          <Plus size={16} />
          New program
        </button>
      </PageHeader>

      <PageStatus loading={fleet.loading} error={fleet.error ?? ""} />

      {selected.size > 0 ? (
        <SelectionBar count={selected.size} onClear={() => setSelected(new Set())}>
          <button
            type="button"
            className="ds-btn ds-btn-danger"
            onClick={() => remove(Array.from(selected))}
          >
            <Trash2 size={16} />
            Delete
          </button>
        </SelectionBar>
      ) : (
        fleet.programs.length > 0 && (
          <div className="ds-toolbar">
            <PageSearch value={query} onChange={setQuery} placeholder="Filter programs…" />
            <ViewToggle value={viewMode} onChange={setViewMode} />
          </div>
        )
      )}

      {fleet.programs.length === 0 && !fleet.loading ? (
        <Empty title="No programs yet">
          <p className="ds-hint">
            A program is an ordered set of clips. Channels carry one; broadcasts play one.
          </p>
          <button
            type="button"
            className="ds-btn ds-btn-solid"
            disabled={isCreating}
            onClick={() => void handleCreate()}
          >
            <Plus size={16} />
            New program
          </button>
        </Empty>
      ) : showList ? (
        <div className="ds-pl-list">
          {rows.map((row) => (
            <ProgramRow key={row.id} id={row.id}>
              <PlaylistRow
                name={row.name}
                meta={meta(row)}
                selected={selected.has(row.id)}
                onSelect={() => toggle(row.id)}
                onOpen={() => openEditor(row.id)}
                menu={menuFor(row)}
                more={menuFor(row)}
              >
                <Cover>{coverCells(row)}</Cover>
              </PlaylistRow>
              {relations(row)}
            </ProgramRow>
          ))}
        </div>
      ) : (
        <div className="ds-pl-grid">
          {rows.map((row) => (
            <ProgramRow key={row.id} id={row.id} tile>
              <PlaylistTile
                name={row.name}
                meta={meta(row)}
                selected={selected.has(row.id)}
                onSelect={() => toggle(row.id)}
                onOpen={() => openEditor(row.id)}
                menu={menuFor(row)}
                more={menuFor(row)}
              >
                <Cover>{coverCells(row)}</Cover>
              </PlaylistTile>
              {relations(row)}
            </ProgramRow>
          ))}
        </div>
      )}

      <Inspector
        open={inspected != null}
        onOpenChange={(open) => {
          if (!open) setInspect(null);
        }}
        title={inspected?.name ?? "Program"}
        actions={
          inspected && (
            <>
              <button
                type="button"
                className="ds-btn ds-btn-solid"
                onClick={() => openEditor(inspected.id)}
              >
                Open
              </button>
              <button
                type="button"
                className="ds-btn ds-btn-ghost"
                onClick={() => makeCopy(inspected)}
              >
                <Copy size={16} />
                Copy
              </button>
              <button
                type="button"
                className="ds-btn ds-btn-danger"
                onClick={() => remove([inspected.id])}
              >
                Delete
              </button>
            </>
          )
        }
      >
        {inspected && (
          <>
            <div className="ds-pl-tile" style={{ marginBottom: 12 }}>
              <Cover>{coverCells(inspected)}</Cover>
            </div>
            <CommitText
              label="Name"
              value={inspected.name}
              onWrite={(next) =>
                next.trim() && next.trim() !== inspected.name
                  ? putProgram({ ...inspected, name: next.trim() })
                  : Promise.resolve()
              }
            />
            <p className="ds-hint">{meta(inspected)}</p>
            {relations(inspected)}
          </>
        )}
      </Inspector>
    </Page>
  );
};

/** A program's row or tile, holdable: what carries it and shows it lights. */
function ProgramRow({
  id,
  tile,
  children,
}: {
  id: string;
  tile?: boolean;
  children: React.ReactNode;
}) {
  const hold = useHoldable("program", id);
  return (
    <div className={`ds-program${tile ? " is-tile" : ""}`} {...hold.bind}>
      {children}
    </div>
  );
}
