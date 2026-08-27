/**
 * The right rail: what is selected, and what can be changed about it.
 *
 * Three scopes stack here, and only one of them is dangerous. Clip settings
 * touch this item. Program settings touch this program. A kind config touches
 * every clip of that kind on every screen in the Space — so it says so, with a
 * count, every time it is open. Every editor this borrows from configures
 * element-local properties; ours are shared, and a panel that does not admit
 * that is how one screen's nudge moves all of them.
 */

import { useState } from "react";
import { ChevronDown, Clock, Trash2 } from "lucide-react";
import { CommitMark, Confirm } from "@/ds";
import { FieldControl } from "../fields/FieldControl";
import type { KindPanel } from "../kinds/types";
import { useEditorSession } from "../state/EditorContext";
import { useKindDraft } from "../state/useKindDraft";
import { formatDuration } from "../model";

export function Inspector() {
  const { editor, panel } = useEditorSession();
  const { selected, selectedPanel } = editor;

  // Never blank. With nothing selected the program itself is the subject,
  // which is also where program settings live now that there is no Apps page.
  if (!selected) {
    return (
      <aside className="pe-inspector">
        <ProgramSection />
      </aside>
    );
  }

  const kindPanel =
    panel.sort === "kind" ? panel.panel : selectedPanel ?? null;

  return (
    <aside className="pe-inspector">
      <ClipSection />
      {kindPanel ? (
        <KindSection
          panel={kindPanel}
          presetId={
            selected.media?.source === "kind" ? (selected.media.preset ?? null) : null
          }
        />
      ) : null}
      <ProgramSection collapsed />
    </aside>
  );
}

function Section({
  title,
  kicker,
  children,
  collapsed = false,
}: {
  title: string;
  kicker?: React.ReactNode;
  children: React.ReactNode;
  collapsed?: boolean;
}) {
  const [open, setOpen] = useState(!collapsed);
  return (
    <section className={`pe-insp-section${open ? " is-open" : ""}`}>
      <button
        type="button"
        className="pe-insp-head"
        aria-expanded={open}
        onClick={() => setOpen((was) => !was)}
      >
        <span>{title}</span>
        {kicker}
        <ChevronDown size={16} className="pe-insp-chevron" />
      </button>
      {open ? <div className="pe-insp-body">{children}</div> : null}
    </section>
  );
}

function ClipSection() {
  const { editor } = useEditorSession();
  const { selected } = editor;
  if (!selected) return null;
  const seconds = Math.max(1, Math.round(selected.durationMs / 1000));

  return (
    <Section title="Clip">
      <p className="pe-insp-subject">{selected.media?.name ?? selected.item.media}</p>
      <label className="pe-field is-desktop">
        <span className="pe-field-label">Duration</span>
        <div className="pe-stepper">
          <Clock size={15} />
          <input
            className="ds-input"
            type="number"
            min={1}
            value={seconds}
            onChange={(event) => {
              const next = Number.parseInt(event.target.value, 10);
              if (Number.isFinite(next) && next > 0) {
                editor.setDuration(selected.item.id, next * 1000);
              }
            }}
          />
          <span className="pe-unit">s</span>
        </div>
      </label>
      <p className="ds-hint">
        Starts at {formatDuration(selected.startMs)} · position{" "}
        {selected.index + 1} of {editor.clips.length}
      </p>
      <div className="pe-insp-actions">
        <button
          type="button"
          className="ds-btn ds-btn-quiet"
          onClick={() => editor.duplicate(selected.item.id)}
        >
          Duplicate
        </button>
        <button
          type="button"
          className="ds-btn ds-btn-quiet is-danger"
          onClick={() => editor.remove(selected.item.id)}
        >
          <Trash2 size={15} />
          Remove
        </button>
      </div>
    </Section>
  );
}

function KindSection({
  panel,
  presetId,
}: {
  panel: KindPanel;
  presetId: string | null;
}) {
  const { usageOf, closePanel } = useEditorSession();
  const draft = useKindDraft(panel, presetId);
  const [removeOpen, setRemoveOpen] = useState(false);
  const used = usageOf(panel.kind);

  return (
    <Section
      title={panel.label}
      kicker={
        <span className={`ds-badge${draft.configured ? " is-on" : ""}`}>
          {draft.configured ? "Configured" : "Not configured"}
        </span>
      }
    >
      {panel.scope === "preset" ? (
        <p className="pe-scope">
          <strong>Preset</strong>
          {used === 1 ? "1 clip" : `${used} clips`} here point at this preset.
          Where each one plays supplies the rest.
        </p>
      ) : null}

      <div className="pe-insp-preview">
        <panel.Preview settings={draft.packed} density="panel" />
      </div>

      {panel.groups.map((group) => (
        <div className="pe-insp-group" key={group.id}>
          <h4>{group.label}</h4>
          {group.note ? <p className="ds-hint">{group.note}</p> : null}
          {group.fields.map((field, index) => (
            <FieldControl
              key={`${group.id}-${index}`}
              field={field}
              draft={draft.draft}
              onPatch={draft.patch}
              surface="desktop"
              errorFor={draft.errorFor}
            />
          ))}
        </div>
      ))}

      

      {/* No Save. The panel writes itself; this only reports. */}
      <div className="pe-insp-actions">
        <CommitMark state={draft.state} error={draft.failure} onRetry={draft.retry} />
        {draft.configured && (
          <button
            type="button"
            className="ds-btn ds-btn-quiet is-danger"
            onClick={() => setRemoveOpen(true)}
          >
            Remove preset
          </button>
        )}
      </div>

      <Confirm
        open={removeOpen}
        onOpenChange={setRemoveOpen}
        title={`Remove ${panel.label}?`}
        description={
          used > 0
            ? `${used === 1 ? "1 clip" : `${used} clips`} in this program point at it. They fall back to whatever each screen supplies.`
            : "No clip in this program points at it."
        }
        confirmLabel="Remove"
        danger
        onConfirm={() => {
          void draft.remove().then((ok) => {
            if (ok) closePanel();
          });
        }}
      />
    </Section>
  );
}

function ProgramSection({ collapsed = false }: { collapsed?: boolean }) {
  const { editor } = useEditorSession();
  return (
    <Section title="Program" collapsed={collapsed}>
      <label className="pe-field is-desktop">
        <span className="pe-field-label">Name</span>
        <input
          className="ds-input"
          value={editor.program.name}
          onChange={(event) => editor.rename(event.target.value)}
        />
      </label>
      <label className="pe-field is-desktop">
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
      <p className="ds-hint">
        {editor.clips.length === 1 ? "1 clip" : `${editor.clips.length} clips`} ·{" "}
        {formatDuration(editor.durationMs)}
      </p>
    </Section>
  );
}
