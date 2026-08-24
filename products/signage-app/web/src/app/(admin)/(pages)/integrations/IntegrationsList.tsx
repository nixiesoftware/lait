import { useCallback, useEffect, useState, type HTMLInputTypeAttribute } from "react";
import { AppWindow, Globe, Moon, Youtube } from "lucide-react";
import {
  Confirm,
  Inspector,
  Page,
  PageHeader,
  PageStatus,
  haptic,
  useToast,
} from "@/ds";
import { CityPicker, type CitySelection } from "@/components/integrations/CityPicker";
import {
  KINDS,
  deleteConfig,
  fetchConfigs,
  putConfig,
  type KindDefinition,
  type KindField,
} from "@/utils/apps/api";
import type { SignageConfig } from "@/utils/lait/types";

type FormValues = Record<string, string>;

const MARK: Record<
  string,
  { tone: "athan" | "youtube" | "web"; Icon: typeof Moon }
> = {
  athan: { tone: "athan", Icon: Moon },
  youtube: { tone: "youtube", Icon: Youtube },
  html_widget: { tone: "web", Icon: Globe },
};

function AppMark({ kind, size = 22 }: { kind: string; size?: number }) {
  const spec = MARK[kind] ?? { tone: "web" as const, Icon: AppWindow };
  return (
    <span className={`ds-app-mark is-${spec.tone}`}>
      <spec.Icon size={size} strokeWidth={1.8} />
    </span>
  );
}

function seed(kind: KindDefinition, config: SignageConfig | null): FormValues {
  const out: FormValues = {};
  for (const f of kind.fields) {
    out[f.name] = config?.settings[f.name] ?? "";
  }
  return out;
}

function inputTypeFor(k: KindField["kind"]): HTMLInputTypeAttribute {
  switch (k) {
    case "number":
      return "number";
    case "secret":
      return "password";
    default:
      return "text";
  }
}

function summary(kind: KindDefinition, config: SignageConfig | null): string {
  if (!config) return kind.description;
  if (kind.kind === "youtube") return config.settings.video_id || config.name;
  if (kind.kind === "html_widget") return config.settings.url || config.name;
  const timezone = config.settings.timezone;
  if (timezone) return timezone;
  const lat = config.settings.latitude;
  const lng = config.settings.longitude;
  if (lat && lng) {
    return `${Number(lat).toFixed(2)}°, ${Number(lng).toFixed(2)}°`;
  }
  return config.name;
}

export default function Integrations() {
  const toast = useToast();
  const [configs, setConfigs] = useState<SignageConfig[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<KindDefinition | null>(null);

  const reload = useCallback(async () => {
    try {
      setError(null);
      setConfigs(await fetchConfigs());
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to load apps");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const configFor = (kind: string): SignageConfig | null =>
    configs.find((c) => c.kind === kind) ?? null;

  return (
    <Page>
      <PageHeader title="Apps" />
      <PageStatus loading={loading} error={error ?? ""} />
      <div className="ds-app-list">
        {KINDS.map((kind) => {
          const config = configFor(kind.kind);
          const configured = config != null;
          return (
            <button
              type="button"
              key={kind.kind}
              className="ds-app"
              onClick={() => setEditing(kind)}
            >
              <AppMark kind={kind.kind} />
              <span className="ds-app-copy">
                <h3>{kind.label}</h3>
                <p>{summary(kind, config)}</p>
              </span>
              <span className="ds-app-go">
                {configured ? "Edit" : "Configure"}
              </span>
            </button>
          );
        })}
      </div>
      <ConfigSheet
        kind={editing}
        config={editing ? configFor(editing.kind) : null}
        onClose={() => setEditing(null)}
        onSaved={() => {
          haptic("save");
          void reload();
        }}
        onRemoved={() => {
          haptic("delete");
          void reload();
        }}
        onError={(message) => {
          toast.show("Save failed", message);
          haptic("error");
        }}
      />
    </Page>
  );
}

function ConfigSheet({
  kind,
  config,
  onClose,
  onSaved,
  onRemoved,
  onError,
}: {
  kind: KindDefinition | null;
  config: SignageConfig | null;
  onClose: () => void;
  onSaved: () => void;
  onRemoved: () => void;
  onError: (message: string) => void;
}) {
  const [values, setValues] = useState<FormValues>(() =>
    kind ? seed(kind, config) : {},
  );
  const [name, setName] = useState(() => config?.name ?? kind?.label ?? "");
  const [submitting, setSubmitting] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [removeOpen, setRemoveOpen] = useState(false);

  useEffect(() => {
    setFormError(null);
    setRemoveOpen(false);
    if (!kind) {
      setValues({});
      setName("");
      return;
    }
    setValues(seed(kind, config));
    setName(config?.name ?? kind.label);
  }, [kind?.kind, config, kind]);

  const fields = kind?.fields ?? [];
  const fieldNames = new Set(fields.map((f) => f.name));
  const useCityPicker = fieldNames.has("latitude") && fieldNames.has("longitude");

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
  }, [kind?.kind, config, useCityPicker]);

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
    setFormError(null);

    const settings: Record<string, string> = {};
    for (const f of fields) {
      const raw = (values[f.name] ?? "").trim();
      if (raw === "") {
        if (f.required) {
          setFormError(`${f.label} is required`);
          setSubmitting(false);
          return;
        }
        continue;
      }
      if (f.kind === "number" && !Number.isFinite(Number(raw))) {
        setFormError(`${f.label} must be a number`);
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
      const message = e instanceof Error ? e.message : "Save failed";
      setFormError(message);
      onError(message);
    } finally {
      setSubmitting(false);
    }
  };

  const handleRemove = async () => {
    if (!config) return;
    setSubmitting(true);
    try {
      await deleteConfig(config.id);
      onRemoved();
      onClose();
    } catch (e) {
      const message = e instanceof Error ? e.message : "Could not remove this app";
      setFormError(message);
      onError(message);
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <>
      <Inspector
        open={kind != null}
        onOpenChange={(open) => {
          if (!open) onClose();
        }}
        className="ds-app-setup"
        title={kind?.label ?? "App"}
        mark={kind ? <AppMark kind={kind.kind} /> : null}
        kicker={
          kind ? (
            <span className={`ds-badge${config ? " is-on" : ""}`}>
              {config ? "Configured" : "Not configured"}
            </span>
          ) : null
        }
        actions={
          kind && (
            <>
              {config && (
                <button
                  type="button"
                  className="ds-btn ds-btn-quiet"
                  disabled={submitting}
                  onClick={() => setRemoveOpen(true)}
                >
                  Remove
                </button>
              )}
              <button
                type="button"
                className="ds-btn ds-btn-solid"
                disabled={submitting}
                onClick={() => void handleSave()}
              >
                {submitting ? "Saving…" : config ? "Save" : "Configure"}
              </button>
            </>
          )
        }
      >
        {kind && (
          <>
            <p className="ds-hint" style={{ margin: 0 }}>
              {kind.description}
            </p>
            <label className="ds-field">
              <span>Name</span>
              <input
                className="ds-input"
                value={name}
                onChange={(event) => setName(event.target.value)}
              />
            </label>
            {useCityPicker && (
              <CityPicker
                currentLatitude={values.latitude ? Number(values.latitude) : null}
                currentLongitude={values.longitude ? Number(values.longitude) : null}
                initialLabel={null}
                onSelect={handleCityPicked}
              />
            )}
            {fields.map((f) => {
              if (useCityPicker && (f.name === "latitude" || f.name === "longitude")) {
                return null;
              }
              return (
                <label className="ds-field" key={f.name}>
                  <span>
                    {f.label}
                    {f.required ? " *" : ""}
                  </span>
                  <input
                    className="ds-input"
                    type={inputTypeFor(f.kind)}
                    value={values[f.name] ?? ""}
                    step={f.kind === "number" ? "any" : undefined}
                    onChange={(event) =>
                      setValues((prev) => ({ ...prev, [f.name]: event.target.value }))
                    }
                  />
                </label>
              );
            })}
            {formError && <p className="ds-danger-text">{formError}</p>}
          </>
        )}
      </Inspector>
      <Confirm
        open={removeOpen}
        onOpenChange={setRemoveOpen}
        title={`Remove ${kind?.label ?? "this app"}?`}
        description="Programs using it will need another source."
        confirmLabel="Remove"
        danger
        onConfirm={handleRemove}
      />
    </>
  );
}
