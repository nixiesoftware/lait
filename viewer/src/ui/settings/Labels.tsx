import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowDown, ArrowUp, Trash2 } from "lucide-react";
import { Button, IconButton, Popover, TextInput } from "@astryxdesign/core";

import { useProjectViewerStore } from "../../projectStore";
import type { LabelDto } from "../../types";
import { EmptyState } from "../AppState";
import { catalogColor } from "../colors";
import { ColorPicker } from "../ColorPicker";
import * as ask from "../dialogs";
import { cn } from "../primitives";
import { SettingsPageHeader } from "../settingsLayout";

/**
 * Labels — the registry as a table, edited in place.
 *
 * Linear's shape, kept because it is the right one for a registry: the table
 * *is* the editor. A new label is a row at the top of the table with its name
 * field focused, not a card above it; renaming is clicking the name; recolouring
 * is clicking the swatch. There is no form that replaces the row, because a
 * form that replaces a row costs the person the row they were looking at.
 *
 * Columns are the facts a `LabelDto` carries — name, colour, identifier. Issue
 * counts would need every issue in the space in memory here, which nothing
 * else on this page needs, so they wait for a per-label query the engine does
 * not offer yet.
 */
export function LabelsPanel({
  spaceId,
  labels,
  readOnly,
  onError,
}: {
  spaceId: string;
  labels: LabelDto[];
  readOnly: boolean;
  onError: (message: string) => void;
}) {
  const projectStore = useProjectViewerStore();
  const [creating, setCreating] = useState(false);
  const [query, setQuery] = useState("");
  const [descending, setDescending] = useState(false);

  const shown = useMemo(() => {
    const needle = query.trim().toLowerCase();
    const matched = needle
      ? labels.filter((label) => label.name.toLowerCase().includes(needle))
      : labels;
    const sorted = [...matched].sort((a, b) => a.name.localeCompare(b.name));
    return descending ? sorted.reverse() : sorted;
  }, [labels, query, descending]);

  const send = async (fn: () => Promise<unknown>) => {
    try {
      await fn();
    } catch (e) {
      onError(e instanceof Error ? e.message : String(e));
    }
  };

  const grid =
    "grid grid-cols-[1.25rem_minmax(0,1fr)_7rem_minmax(8rem,12rem)_4.5rem] items-center gap-3";

  return (
    <>
      <SettingsPageHeader
        title="Labels"
        description="Shared across every project. Renaming re-points every issue that uses one."
        actions={
          !readOnly ? (
            <Button
              label="New label"
              variant="primary"
              size="sm"
              isDisabled={creating}
              onClick={() => setCreating(true)}
            />
          ) : undefined
        }
      />

      {(labels.length > 0 || creating) && (
        <div className="mb-4 flex items-center gap-3">
          <div className="w-full max-w-xs">
            <TextInput
              label="Filter labels"
              isLabelHidden
              value={query}
              onChange={setQuery}
              placeholder="Filter by name…"
              size="sm"
              width="100%"
            />
          </div>
          <span className="text-mute ml-auto text-xs tabular-nums">
            {labels.length} {labels.length === 1 ? "label" : "labels"}
          </span>
        </div>
      )}

      {labels.length === 0 && !creating ? (
        <EmptyState
          art="filtered"
          title="No labels yet"
          body="A label is a shared word every project can put on an issue."
          action={
            !readOnly ? (
              <Button
                label="New label"
                variant="primary"
                size="sm"
                onClick={() => setCreating(true)}
              />
            ) : undefined
          }
        />
      ) : (
        <div className="border-line overflow-hidden rounded-surface border">
          <div className={cn(grid, "text-mute border-line border-b px-3 py-2 text-2xs")}>
            <span aria-hidden />
            <button
              type="button"
              onClick={() => setDescending((d) => !d)}
              className="hover:text-fg flex w-fit items-center gap-1 text-left"
              aria-label={`Sort by name, ${descending ? "descending" : "ascending"}`}
            >
              Name
              {descending ? (
                <ArrowDown className="size-icon-xs" />
              ) : (
                <ArrowUp className="size-icon-xs" />
              )}
            </button>
            <span>Colour</span>
            <span>Identifier</span>
            <span aria-hidden />
          </div>

          <ul className="divide-line divide-y">
            {creating && !readOnly && (
              <NewLabelRow
                grid={grid}
                onCancel={() => setCreating(false)}
                onCreate={(name, color) => {
                  setCreating(false);
                  void send(() => projectStore.createLabel(spaceId, name, color));
                }}
              />
            )}
            {shown.length === 0 && !creating && (
              <li className="text-mute px-3 py-6 text-center text-sm">
                Nothing matches “{query}”.
              </li>
            )}
            {shown.map((label) => (
              <LabelRow
                key={label.id}
                grid={grid}
                label={label}
                readOnly={readOnly}
                onRename={(name) =>
                  void send(() => projectStore.editLabel(spaceId, label.id, name, label.color))
                }
                onRecolor={(color) =>
                  void send(() => projectStore.editLabel(spaceId, label.id, label.name, color))
                }
                onDelete={() =>
                  void ask
                    .confirm({
                      title: `Delete label “${label.name}”?`,
                      body: "Issues keep the reference until it's re-created; it just leaves the registry.",
                      confirmText: "Delete",
                      danger: true,
                    })
                    .then((ok) => {
                      if (ok) void send(() => projectStore.deleteLabel(spaceId, label.id));
                    })
                }
              />
            ))}
          </ul>
        </div>
      )}
    </>
  );
}

/** The row a new label is typed into — the table's first row, not a form. */
function NewLabelRow({
  grid,
  onCancel,
  onCreate,
}: {
  grid: string;
  onCancel: () => void;
  onCreate: (name: string, color: string) => void;
}) {
  const [name, setName] = useState("");
  const [color, setColor] = useState("blue");
  const ready = name.trim().length > 0;
  return (
    <li className={cn(grid, "bg-raised px-3 py-1.5")}>
      <Swatch color={color} onChange={setColor} label="Colour of the new label" />
      <input
        autoFocus
        value={name}
        placeholder="Label name"
        aria-label="New label name"
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && ready) onCreate(name.trim(), color);
          if (e.key === "Escape") onCancel();
        }}
        className="control-hover-outline border-line focus:border-line-strong h-ctl-sm min-w-0 rounded-control border bg-transparent px-2 text-sm outline-none"
      />
      <span className="text-dim text-xs capitalize">{color}</span>
      <span className="text-mute text-2xs">assigned on create</span>
      <span className="flex justify-end gap-1">
        <Button label="Cancel" variant="ghost" size="sm" onClick={onCancel} />
        <Button
          label="Create"
          variant="primary"
          size="sm"
          isDisabled={!ready}
          onClick={() => onCreate(name.trim(), color)}
        />
      </span>
    </li>
  );
}

/** One label. The name is a field the moment you click it; the swatch is a
 *  picker the moment you click it. Nothing else on the row moves. */
function LabelRow({
  grid,
  label,
  readOnly,
  onRename,
  onRecolor,
  onDelete,
}: {
  grid: string;
  label: LabelDto;
  readOnly: boolean;
  onRename: (name: string) => void;
  onRecolor: (color: string) => void;
  onDelete: () => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(label.name);
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (!editing) setDraft(label.name);
  }, [editing, label.name]);
  useEffect(() => {
    if (editing) inputRef.current?.select();
  }, [editing]);

  const commit = () => {
    const next = draft.trim();
    setEditing(false);
    if (next && next !== label.name) onRename(next);
  };

  return (
    <li className={cn(grid, "group/label hover:bg-hover min-h-ctl-lg px-3 py-1")}>
      <Swatch
        color={label.color}
        onChange={onRecolor}
        label={`Colour of ${label.name}`}
        disabled={readOnly}
      />
      {editing ? (
        <input
          ref={inputRef}
          autoFocus
          value={draft}
          aria-label={`Rename ${label.name}`}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === "Enter") commit();
            if (e.key === "Escape") setEditing(false);
          }}
          className="control-hover-outline border-line focus:border-line-strong h-ctl-sm min-w-0 rounded-control border bg-transparent px-2 text-sm outline-none"
        />
      ) : (
        <button
          type="button"
          disabled={readOnly}
          onClick={() => setEditing(true)}
          title={readOnly ? undefined : "Rename"}
          className="hover:border-line-strong h-ctl-sm min-w-0 truncate rounded-control border border-transparent px-2 text-left text-sm font-medium outline-none disabled:cursor-default"
        >
          {label.name}
        </button>
      )}
      <span className="text-dim text-xs capitalize">{label.color}</span>
      <code className="text-mute truncate font-mono text-2xs" title={label.id}>
        {label.id}
      </code>
      <span className="flex justify-end">
        {!readOnly && (
          <IconButton
            label={`Delete ${label.name}`}
            tooltip="Delete"
            variant="ghost"
            size="sm"
            className="opacity-0 group-hover/label:opacity-100 focus-visible:opacity-100"
            onClick={onDelete}
            icon={<Trash2 className="size-icon-sm" />}
          />
        )}
      </span>
    </li>
  );
}

/** A colour dot that opens the catalog picker. Picking closes it. */
function Swatch({
  color,
  onChange,
  label,
  disabled,
}: {
  color: string;
  onChange: (color: string) => void;
  label: string;
  disabled?: boolean;
}) {
  const [open, setOpen] = useState(false);
  return (
    <Popover
      isOpen={open}
      onOpenChange={setOpen}
      alignment="start"
      width={224}
      content={
        <div className="p-3">
          <ColorPicker
            value={color}
            onChange={(next) => {
              setOpen(false);
              onChange(next);
            }}
          />
        </div>
      }
    >
      <button
        type="button"
        disabled={disabled}
        aria-label={label}
        title={disabled ? undefined : "Change colour"}
        className="hover:ring-line-strong flex size-ctl-sm items-center justify-center rounded-control hover:ring-1 disabled:cursor-default disabled:hover:ring-0"
      >
        <span className="size-mark-lg rounded-full" style={{ background: catalogColor(color) }} />
      </button>
    </Popover>
  );
}
