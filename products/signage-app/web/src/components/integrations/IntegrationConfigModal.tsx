import React, { useEffect, useMemo, useState } from "react";
import { BaseDetailsModal } from "@/components/ui/BaseDetailsModal";
import Label from "@/components/form/Label";
import Button from "@/components/ui/button/Button";
import { KindDefinition, KindField, putConfig } from "@/utils/apps/api";
import type { SignageConfig } from "@/utils/lait/types";
import { CityPicker, CitySelection } from "@/components/integrations/CityPicker";

interface IntegrationConfigModalProps {
  // The kind to configure. Null hides the modal. The parent owns
  // open/closed state via this prop rather than a separate boolean so
  // that the form re-seeds whenever a different kind is selected.
  kind: KindDefinition | null;
  // The existing config for this kind, when one exists.
  config: SignageConfig | null;
  onClose: () => void;
  // Called after a successful save. The parent should re-fetch the
  // configs so the configured indicator updates everywhere.
  onSaved: () => void;
}

// Settings are strings on the wire; number fields parse at save time
// and the inputs hold the string form throughout.
type FormValues = Record<string, string>;

// Seeded synchronously so the FIRST render has the saved values — the
// city picker captures its initial label at mount and would otherwise
// lock onto an empty form. See CityPicker for the mount-once caveat.
function seed(kind: KindDefinition, config: SignageConfig | null): FormValues {
  const out: FormValues = {};
  for (const f of kind.fields) {
    out[f.name] = config?.settings[f.name] ?? "";
  }
  return out;
}

function inputTypeFor(k: KindField["kind"]): React.HTMLInputTypeAttribute {
  switch (k) {
    case "number":
      return "number";
    case "secret":
      return "password";
    default:
      return "text";
  }
}

export const IntegrationConfigModal: React.FC<IntegrationConfigModalProps> = ({
  kind,
  config,
  onClose,
  onSaved,
}) => {
  const [values, setValues] = useState<FormValues>(() =>
    kind ? seed(kind, config) : {},
  );
  const [name, setName] = useState(() => config?.name ?? kind?.label ?? "");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Re-seed when the user clicks a different kind's card (modal stays
  // mounted; only the props swap).
  useEffect(() => {
    setError(null);
    if (!kind) {
      setValues({});
      setName("");
      return;
    }
    setValues(seed(kind, config));
    setName(config?.name ?? kind.label);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kind?.kind]);

  const fields = useMemo(() => kind?.fields ?? [], [kind]);
  const fieldNames = useMemo(() => new Set(fields.map((f) => f.name)), [fields]);
  const useCityPicker = fieldNames.has("latitude") && fieldNames.has("longitude");

  // First-time setup pre-fills coordinates from the browser. Skipped
  // when editing an existing config — its saved coords are the truth.
  useEffect(() => {
    if (!kind || config || !useCityPicker) return;
    if (typeof navigator === "undefined" || !("geolocation" in navigator)) return;
    let cancelled = false;
    navigator.geolocation.getCurrentPosition(
      (position) => {
        if (cancelled) return;
        setValues((prev) => ({
          ...prev,
          latitude: prev.latitude || String(position.coords.latitude),
          longitude: prev.longitude || String(position.coords.longitude),
        }));
      },
      () => {},
      { timeout: 5000 },
    );
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [kind?.kind, useCityPicker]);

  const handleCityPicked = (s: CitySelection) => {
    setValues((prev) => {
      const next: FormValues = {
        ...prev,
        latitude: String(s.latitude),
        longitude: String(s.longitude),
      };
      if (fieldNames.has("timezone")) next.timezone = s.timezone;
      return next;
    });
  };

  const handleSave = async () => {
    if (!kind) return;
    setSubmitting(true);
    setError(null);

    const settings: Record<string, string> = {};
    for (const f of fields) {
      const raw = (values[f.name] ?? "").trim();
      if (raw === "") {
        if (f.required) {
          setError(`${f.label} is required`);
          setSubmitting(false);
          return;
        }
        continue;
      }
      if (f.kind === "number" && !Number.isFinite(Number(raw))) {
        setError(`${f.label} must be a number`);
        setSubmitting(false);
        return;
      }
      settings[f.name] = raw;
    }

    try {
      await putConfig(kind.kind, name.trim() || kind.label, settings);
      onSaved();
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Save failed");
    } finally {
      setSubmitting(false);
    }
  };

  if (!kind) return null;

  const detailsSections = (
    <div className="space-y-4 pt-4">
      <div>
        <Label htmlFor={`config-${kind.kind}-name`}>Name</Label>
        <input
          id={`config-${kind.kind}-name`}
          type="text"
          value={name}
          onChange={(e) => setName(e.target.value)}
          className="h-11 w-full rounded-md border border-gray-300 bg-transparent px-4 py-2.5 text-sm shadow-theme-xs placeholder:text-gray-400 focus:border-brand-300 focus:outline-hidden focus:ring-3 focus:ring-brand-500/10 dark:border-gray-700 dark:bg-gray-900 dark:text-white/90 dark:placeholder:text-white/30 dark:focus:border-brand-800"
        />
      </div>
      {useCityPicker && (
        <CityPicker
          currentLatitude={values.latitude ? Number(values.latitude) : null}
          currentLongitude={values.longitude ? Number(values.longitude) : null}
          initialLabel={null}
          onSelect={handleCityPicked}
        />
      )}
      {fields.map((f) => {
        // When the city picker is in play, latitude/longitude are
        // owned by the picker and shouldn't render as separate inputs.
        if (useCityPicker && (f.name === "latitude" || f.name === "longitude")) {
          return null;
        }
        const id = `config-${kind.kind}-${f.name}`;
        return (
          <div key={f.name}>
            <Label htmlFor={id}>
              {f.label}
              {f.required && <span className="text-red-500"> *</span>}
            </Label>
            <input
              id={id}
              type={inputTypeFor(f.kind)}
              value={values[f.name] ?? ""}
              step={f.kind === "number" ? "any" : undefined}
              onChange={(e) =>
                setValues((prev) => ({ ...prev, [f.name]: e.target.value }))
              }
              className="h-11 w-full rounded-md border border-gray-300 bg-transparent px-4 py-2.5 text-sm shadow-theme-xs placeholder:text-gray-400 focus:border-brand-300 focus:outline-hidden focus:ring-3 focus:ring-brand-500/10 dark:border-gray-700 dark:bg-gray-900 dark:text-white/90 dark:placeholder:text-white/30 dark:focus:border-brand-800"
            />
          </div>
        );
      })}
      {error && (
        <div className="rounded-md border border-red-300 bg-red-50 p-3 text-sm text-red-800 dark:border-red-800 dark:bg-red-900/30 dark:text-red-300">
          {error}
        </div>
      )}
    </div>
  );

  const actionButtons = (
    <div className="flex justify-end gap-2">
      <Button variant="outline" onClick={onClose} disabled={submitting}>
        Cancel
      </Button>
      <Button onClick={handleSave} disabled={submitting}>
        {submitting ? "Saving…" : config ? "Save changes" : "Configure"}
      </Button>
    </div>
  );

  return (
    <BaseDetailsModal
      isOpen={true}
      onClose={onClose}
      title={kind.label}
      detailsSections={detailsSections}
      actionButtons={actionButtons}
    />
  );
};
