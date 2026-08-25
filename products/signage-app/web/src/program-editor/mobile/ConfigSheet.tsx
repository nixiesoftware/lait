/**
 * The mobile settings sheet: tabbed, not scrolled.
 *
 * Athan carries around thirty settings — an order of magnitude more than the
 * three-to-six a phone editor usually configures, so the one-long-scroll every
 * reference uses would bury Place under Audio. The declared groups become tabs
 * (Depop, Linktree, Universe), and the stage stays visible above the sheet so
 * the card can be watched changing while it is typed.
 */

import { useState } from "react";
import { X } from "lucide-react";
import { Confirm } from "@/ds";
import { FieldControl } from "../fields/FieldControl";
import type { KindPanel } from "../kinds/types";
import { useEditorSession } from "../state/EditorContext";
import { useKindDraft } from "../state/useKindDraft";

export function KindSheet({
  panel,
  presetId,
}: {
  panel: KindPanel;
  presetId: string | null;
}) {
  const { closePanel, usageOf } = useEditorSession();
  const draft = useKindDraft(panel, presetId);
  const [tab, setTab] = useState(panel.groups[0]?.id ?? "");
  const [removeOpen, setRemoveOpen] = useState(false);
  const group = panel.groups.find((entry) => entry.id === tab) ?? panel.groups[0];
  const used = usageOf(panel.kind);

  return (
    <section className="pe-sheet" aria-label={panel.label}>
      <header className="pe-sheet-head">
        <span className="pe-sheet-grip" aria-hidden />
        <strong>{panel.label}</strong>
        <button type="button" className="ds-icon" onClick={closePanel} aria-label="Close">
          <X size={18} />
        </button>
      </header>

      {panel.scope === "preset" ? (
        <p className="pe-scope is-compact">
          Preset — shared by every clip pointing at it.
          {used > 0 ? ` ${used === 1 ? "1 clip" : `${used} clips`} here.` : ""}
        </p>
      ) : null}

      <nav className="pe-sheet-tabs" aria-label={`${panel.label} settings`}>
        {panel.groups.map((entry) => (
          <button
            type="button"
            key={entry.id}
            className={`pe-sheet-tab${entry.id === tab ? " is-on" : ""}`}
            aria-pressed={entry.id === tab}
            onClick={() => setTab(entry.id)}
          >
            {entry.label}
          </button>
        ))}
      </nav>

      <div className="pe-sheet-body">
        {group?.note ? <p className="ds-hint">{group.note}</p> : null}
        {group?.fields.map((field, index) => (
          <FieldControl
            key={`${group.id}-${index}`}
            field={field}
            draft={draft.draft}
            onPatch={draft.patch}
            surface="mobile"
            errorFor={draft.errorFor}
          />
        ))}
        {draft.failure ? <p className="ds-danger-text">{draft.failure}</p> : null}
      </div>

      <footer className="pe-sheet-foot">
        {draft.configured ? (
          <button
            type="button"
            className="ds-btn ds-btn-quiet is-danger"
            disabled={draft.saving}
            onClick={() => setRemoveOpen(true)}
          >
            Remove
          </button>
        ) : null}
        <button
          type="button"
          className="ds-btn ds-btn-solid"
          disabled={draft.saving || !draft.dirty}
          onClick={() => {
            void draft.commit().then((ok) => {
              if (ok) closePanel();
            });
          }}
        >
          {draft.saving ? "Saving…" : draft.configured ? "Save preset" : "Create preset"}
        </button>
      </footer>

      <Confirm
        open={removeOpen}
        onOpenChange={setRemoveOpen}
        title={`Remove ${panel.label}?`}
        description={
          used > 0
            ? `${used === 1 ? "1 clip" : `${used} clips`} here point at it. They fall back to whatever each screen supplies.`
            : "No clip here points at it."
        }
        confirmLabel="Remove"
        danger
        onConfirm={() => {
          void draft.remove().then((ok) => {
            if (ok) closePanel();
          });
        }}
      />
    </section>
  );
}

export function ProgramSheet() {
  const { editor, closePanel } = useEditorSession();
  return (
    <section className="pe-sheet" aria-label="Program">
      <header className="pe-sheet-head">
        <span className="pe-sheet-grip" aria-hidden />
        <strong>Program</strong>
        <button type="button" className="ds-icon" onClick={closePanel} aria-label="Close">
          <X size={18} />
        </button>
      </header>
      <div className="pe-sheet-body">
        <label className="pe-field is-mobile">
          <span className="pe-field-label">Name</span>
          <input
            className="ds-input"
            value={editor.program.name}
            onChange={(event) => editor.rename(event.target.value)}
          />
        </label>
        <label className="pe-field is-mobile">
          <span className="pe-field-label">When it ends</span>
          <select
            className="ds-input"
            value={editor.program.cycle}
            onChange={(event) =>
              editor.apply({
                ...editor.program,
                cycle: event.target.value as typeof editor.program.cycle,
              })
            }
          >
            <option value="loop">Loop from the start</option>
            <option value="hold_last">Hold the last clip</option>
            <option value="poll_at_end">Ask for the next program</option>
            <option value="blank_at_end">Go blank</option>
          </select>
        </label>
      </div>
    </section>
  );
}
