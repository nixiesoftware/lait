/**
 * The contextual action bar: mobile's inspector, indexed.
 *
 * CapCut, TikTok and Edits all land on the same anatomy — preview, compact
 * timeline, and a horizontally scrolling row of actions that changes with the
 * selection. There is no room for a properties panel on a phone, so the bar is
 * the index into one: each entry either acts, or raises the sheet at the group
 * it names.
 */

import { Copy, Clock, Plus, Settings2, Trash2, type LucideIcon } from "lucide-react";
import { useEditorSession } from "../state/EditorContext";

export type BarAction = {
  id: string;
  label: string;
  Icon: LucideIcon;
  danger?: boolean;
  onPick: () => void;
};

export function ActionBar({
  onAdd,
  onDuration,
}: {
  onAdd: () => void;
  onDuration: () => void;
}) {
  const { editor, openKindPanel, openProgramPanel } = useEditorSession();
  const { selected, selectedPanel } = editor;

  const actions: BarAction[] = [];

  if (selected && selectedPanel) {
    actions.push({
      id: "configure",
      label: selectedPanel.label,
      Icon: selectedPanel.Icon,
      onPick: () => openKindPanel(selectedPanel, selected.item.id),
    });
  }

  actions.push({ id: "add", label: "Add", Icon: Plus, onPick: onAdd });

  if (selected) {
    actions.push(
      { id: "length", label: "Length", Icon: Clock, onPick: onDuration },
      {
        id: "duplicate",
        label: "Duplicate",
        Icon: Copy,
        onPick: () => editor.duplicate(selected.item.id),
      },
      {
        id: "remove",
        label: "Remove",
        Icon: Trash2,
        danger: true,
        onPick: () => editor.remove(selected.item.id),
      },
    );
  }

  actions.push({
    id: "program",
    label: "Program",
    Icon: Settings2,
    onPick: openProgramPanel,
  });

  return (
    <nav className="pe-actionbar" aria-label="Clip actions">
      {actions.map((action) => (
        <button
          type="button"
          key={action.id}
          className={`pe-action${action.danger ? " is-danger" : ""}`}
          onClick={action.onPick}
        >
          <action.Icon size={20} strokeWidth={1.75} />
          {action.label}
        </button>
      ))}
    </nav>
  );
}
