import { Bookmark, Plus, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";

import type { DisplayState } from "../core/display";
import type { FilterState } from "../core/filter";
import type { WorkView } from "../core/registry";
import { loadSavedViews, removeView, saveView, type SavedView } from "../core/savedViews";
import { Button, IconButton, Popover, TextInput } from "@astryxdesign/core";
import { navigationItem } from "./primitives";

export function SavedViews({ space, project, view, filter, display, onApply, onChange }: { space: string; project: string; view: WorkView; filter: FilterState; display: DisplayState; onApply: (view: SavedView) => void; onChange?: () => void }) {
  const [views, setViews] = useState(() => loadSavedViews(space, project));
  const [name, setName] = useState("");

  useEffect(() => setViews(loadSavedViews(space, project)), [space, project]);

  const create = () => {
    const title = name.trim();
    if (!title) return;
    const id = `${Date.now().toString(36)}-${title.toLowerCase().replace(/[^a-z0-9]+/g, "-")}`;
    setViews(saveView(space, project, { id, name: title, filter, display, view }));
    onChange?.();
    setName("");
  };

  return (
    <Popover
      alignment="end"
      // Stated here, not on the content — see the note in `Picker.tsx`.
      width={288}
      content={
        <div className="p-2">
          <div className="mb-2 px-1">
            <p className="font-semibold">Saved views</p>
            <p className="text-mute text-xs">Private to this browser and local space.</p>
          </div>
          {views.length === 0 ? (
            <p className="text-mute px-2 py-3 text-center text-sm">No saved views yet.</p>
          ) : (
            <div className="mb-2 flex max-h-overlay-sm flex-col gap-px overflow-y-auto">
              {views.map((view) => (
                <div key={view.id} className="group/view relative">
                  <button onClick={() => onApply(view)} className={`${navigationItem()} pr-8`}>{view.name}</button>
                  <IconButton
                    label={`Delete ${view.name}`}
                    className="absolute top-0.5 right-0.5 opacity-0 group-hover/view:opacity-100 focus-visible:opacity-100"
                    onClick={() => { setViews(removeView(space, project, view.id)); onChange?.(); }}
                    variant="ghost"
                    size="sm"
                    tooltip={`Delete ${view.name}`}
                    icon={<Trash2 className="size-icon-xs" />}
                  />
                </div>
              ))}
            </div>
          )}
          <div className="border-line flex items-center gap-1 border-t pt-2">
            <TextInput
              label="Name this view"
              isLabelHidden
              size="sm"
              value={name}
              onChange={setName}
              onKeyDown={(event) => event.key === "Enter" && create()}
              placeholder="Name this view…"
              className="min-w-0 flex-1"
              width="100%"
            />
            <Button
              isDisabled={!name.trim()}
              onClick={create}
              icon={<Plus className="size-icon-xs" />}
              label="Save"
              variant="secondary"
              size="sm"
            />
          </div>
        </div>
      }
    >
      <IconButton
        label="Local saved views"
        variant="ghost"
        size="sm"
        tooltip="Local saved views"
        icon={<Bookmark className="size-icon-md" />}
      />
    </Popover>
  );
}
