/**
 * One field declaration, drawn for whichever surface asked.
 *
 * `surface` changes density and control choice, never which fields exist or
 * what they write. A phone gets full-width rows and native pickers; a desktop
 * gets a label-and-value grid. Both write the same settings keys, which is the
 * only reason two shells can share one panel declaration.
 */

import type { Field } from "../kinds/types";
import { PlaceField } from "./PlaceField";

export type Surface = "desktop" | "mobile";

export type FieldProps = {
  field: Field;
  draft: Record<string, string>;
  onPatch: (patch: Record<string, string>) => void;
  surface: Surface;
  errorFor: (key: string) => string | null;
};

export function FieldControl({ field, draft, onPatch, surface, errorFor }: FieldProps) {
  switch (field.control) {
    case "place":
      return (
        <PlaceField
          field={field}
          draft={draft}
          onPatch={onPatch}
          surface={surface}
          errorFor={errorFor}
        />
      );

    case "text":
      return (
        <Row label={field.label} hint={field.hint} error={errorFor(field.key)} surface={surface}>
          <input
            className="ds-input"
            value={draft[field.key] ?? ""}
            placeholder={field.placeholder}
            onChange={(event) => onPatch({ [field.key]: event.target.value })}
          />
        </Row>
      );

    case "select":
      return (
        <Row label={field.label} hint={field.hint} error={errorFor(field.key)} surface={surface}>
          <select
            className="ds-input"
            value={draft[field.key] ?? field.options[0]?.value ?? ""}
            onChange={(event) => onPatch({ [field.key]: event.target.value })}
          >
            {field.options.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </Row>
      );

    case "toggle": {
      const on = draft[field.key] !== "0";
      return (
        <label className={`pe-field is-toggle is-${surface}`}>
          <span className="pe-field-label">
            {field.label}
            {field.hint ? <small>{field.hint}</small> : null}
          </span>
          <button
            type="button"
            role="switch"
            aria-checked={on}
            className={`pe-switch${on ? " is-on" : ""}`}
            onClick={() => onPatch({ [field.key]: on ? "0" : "1" })}
          >
            <span />
          </button>
        </label>
      );
    }

    case "int":
      return (
        <Row label={field.label} hint={field.hint} error={errorFor(field.key)} surface={surface}>
          <div className="pe-stepper">
            <input
              className="ds-input"
              type="number"
              inputMode="numeric"
              min={field.min}
              max={field.max}
              placeholder={field.placeholder}
              value={draft[field.key] ?? ""}
              onChange={(event) => onPatch({ [field.key]: event.target.value })}
            />
            {field.unit ? <span className="pe-unit">{field.unit}</span> : null}
          </div>
        </Row>
      );

    case "time":
      return (
        <Row label={field.label} hint={field.hint} error={errorFor(field.key)} surface={surface}>
          <input
            className="ds-input"
            type="time"
            value={draft[field.key] ?? ""}
            onChange={(event) => onPatch({ [field.key]: event.target.value })}
          />
        </Row>
      );

    case "swatch":
      return (
        <div className={`pe-field is-block is-${surface}`}>
          <span className="pe-field-label">{field.label}</span>
          <div className="pe-swatches" role="radiogroup" aria-label={field.label}>
            {field.options.map((option) => {
              const on = (draft[field.key] ?? field.options[0]?.value) === option.value;
              return (
                <button
                  type="button"
                  key={option.value}
                  role="radio"
                  aria-checked={on}
                  className={`pe-swatch${on ? " is-on" : ""}`}
                  style={{ background: option.bg, color: option.fg }}
                  onClick={() => onPatch({ [field.key]: option.value })}
                >
                  {option.label}
                </button>
              );
            })}
          </div>
        </div>
      );

    case "matrix":
      return (
        <div className={`pe-field is-block is-${surface}`}>
          <span className="pe-field-label">{field.label}</span>
          <div
            className="pe-matrix"
            style={{ gridTemplateColumns: `minmax(4.5rem, 1fr) repeat(${field.columns.length}, 1fr)` }}
          >
            <span />
            {field.columns.map((column) => (
              <span key={column.id} className="pe-matrix-head">
                {column.label}
              </span>
            ))}
            {field.rows.map((row) => (
              <MatrixRow
                key={row.value}
                label={row.label}
                cells={field.columns.map((column) => ({
                  key: column.keyFor(row.value),
                  min: column.min,
                  max: column.max,
                  placeholder: column.placeholder,
                }))}
                draft={draft}
                onPatch={onPatch}
              />
            ))}
          </div>
        </div>
      );
  }
}

function MatrixRow({
  label,
  cells,
  draft,
  onPatch,
}: {
  label: string;
  cells: { key: string; min: number; max: number; placeholder?: string }[];
  draft: Record<string, string>;
  onPatch: (patch: Record<string, string>) => void;
}) {
  return (
    <>
      <span className="pe-matrix-row">{label}</span>
      {cells.map((cell) => (
        <input
          key={cell.key}
          className="ds-input"
          type="number"
          inputMode="numeric"
          min={cell.min}
          max={cell.max}
          placeholder={cell.placeholder}
          aria-label={`${label} ${cell.key}`}
          value={draft[cell.key] ?? ""}
          onChange={(event) => onPatch({ [cell.key]: event.target.value })}
        />
      ))}
    </>
  );
}

export function Row({
  label,
  hint,
  error,
  surface,
  children,
}: {
  label: string;
  hint?: string;
  error?: string | null;
  surface: Surface;
  children: React.ReactNode;
}) {
  return (
    <label className={`pe-field is-${surface}${error ? " is-bad" : ""}`}>
      <span className="pe-field-label">
        {label}
        {hint ? <small>{hint}</small> : null}
      </span>
      {children}
      {error ? <span className="pe-field-error">{error}</span> : null}
    </label>
  );
}
