/**
 * What a kind *is*, declared once.
 *
 * A kind (`athan`, and whatever follows it) is configured by a Space-wide
 * `SignageConfig`, drawn on the stage by a preview, and summarised in a row.
 * Before this file those three facts lived in three places — `KINDS` in
 * `utils/apps/api.ts` described fields nobody rendered, `AthanSheet` hand-built
 * a form that ignored them, and `Thumb`/`Stage` each re-derived the preview.
 * They drifted, and a kind with no panel could still be added to a program.
 *
 * One declaration now feeds all of it, and the two surfaces render the same
 * groups differently: desktop as dense collapsible sections, mobile as tabs in
 * a sheet. That is the whole point of describing fields instead of drawing them.
 */

import type { ComponentType } from "react";
import type { LucideIcon } from "lucide-react";
import type { SignageConfig } from "@/utils/lait/types";

/** A kind's settings, exactly as the World stores them. */
export type Settings = Record<string, string>;

/**
 * A draft is settings-shaped on purpose.
 *
 * The old form had its own field names and a `pack()` that rebuilt the map from
 * scratch, which silently erased any key the form did not know about. A draft
 * keyed by setting name merges instead.
 */
export type Draft = Settings;

export type Option = { value: string; label: string };

type Base = { label: string; hint?: string };

/**
 * The place field owns three keys at once, because a coordinate without its
 * zone is a prayer time computed in UTC — which is wrong everywhere except
 * Greenwich, and wrong silently.
 */
export type PlaceField = Base & {
  control: "place";
  keys: { latitude: string; longitude: string; timezone: string };
};

export type Field =
  | PlaceField
  | (Base & { control: "text"; key: string; placeholder?: string })
  | (Base & { control: "select"; key: string; options: Option[] })
  | (Base & { control: "toggle"; key: string })
  | (Base & {
      control: "int";
      key: string;
      min: number;
      max: number;
      unit?: string;
      placeholder?: string;
    })
  | (Base & { control: "time"; key: string })
  | (Base & {
      control: "swatch";
      key: string;
      options: (Option & { bg: string; fg: string })[];
    })
  | (Base & {
      control: "matrix";
      rows: Option[];
      columns: {
        id: string;
        label: string;
        keyFor: (row: string) => string;
        min: number;
        max: number;
        placeholder?: string;
      }[];
    });

/** One section of a panel: a desktop disclosure, a mobile tab. */
export type Group = {
  id: string;
  label: string;
  note?: string;
  fields: Field[];
};

export type FieldError = { key: string; message: string };

/**
 * Where an edit lands. `space` is the one that needs saying out loud: the
 * config is shared, so editing it from one clip changes every clip of that
 * kind on every screen.
 */
export type Scope = "clip" | "space" | "program";

export type KindPanel = {
  kind: string;
  label: string;
  description: string;
  Icon: LucideIcon;
  /** Drives `.ds-app-mark.is-<tone>`. */
  tone: string;
  scope: Scope;
  groups: Group[];
  /** Sensible values for a config that does not exist yet. */
  defaults: Settings;
  /** How long a freshly added clip of this kind runs. */
  defaultDurationMs: number;
  seed(config: SignageConfig | null): Draft;
  /** Merged over what is already stored — never a wholesale replacement. */
  pack(draft: Draft, existing: Settings): Settings;
  validate(draft: Draft): FieldError[];
  summarize(config: SignageConfig | null): string;
  Preview: ComponentType<{ settings: Settings; density?: Density }>;
};

/** How much room the preview has. The stage is not a thumbnail. */
export type Density = "stage" | "panel" | "thumb";

/** Seed helper: defaults under whatever the Space already stored. */
export function seedFrom(panel: KindPanel, config: SignageConfig | null): Draft {
  return { ...panel.defaults, ...(config?.settings ?? {}) };
}

/**
 * Merge helper. Empty strings are removals, so clearing an optional field in
 * the form clears it in the World rather than storing `""` for the renderer to
 * parse back into a default.
 */
export function mergeSettings(draft: Draft, existing: Settings): Settings {
  const out: Settings = { ...existing };
  for (const [key, value] of Object.entries(draft)) {
    const trimmed = value.trim();
    if (trimmed === "") delete out[key];
    else out[key] = trimmed;
  }
  return out;
}

/** Every settings key a group's fields own — used to scope a reset. */
export function keysOf(field: Field): string[] {
  switch (field.control) {
    case "place":
      return [field.keys.latitude, field.keys.longitude, field.keys.timezone];
    case "matrix":
      return field.rows.flatMap((row) =>
        field.columns.map((column) => column.keyFor(row.value)),
      );
    default:
      return [field.key];
  }
}
