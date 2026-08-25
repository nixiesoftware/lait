import { useEffect, useRef, useState } from "react";
import { Moon } from "lucide-react";
import { Confirm, Inspector } from "@/ds";
import { CityPicker, type CitySelection } from "@/components/integrations/CityPicker";
import { deleteConfig, putConfig } from "@/utils/apps/api";
import type { SignageConfig } from "@/utils/lait/types";
import { THEMES, type Theme } from "@/program-editor/athan";
import { AthanPreview } from "@/program-editor/AthanPreview";

const METHODS = [
  { id: "mwl", label: "Muslim World League" },
  { id: "isna", label: "ISNA" },
  { id: "egypt", label: "Egyptian" },
  { id: "makkah", label: "Umm al-Qura" },
  { id: "karachi", label: "Karachi" },
  { id: "tehran", label: "Tehran" },
  { id: "jafari", label: "Jafari" },
] as const;

const PRAYERS = [
  ["fajr", "Fajr"],
  ["dhuhr", "Dhuhr"],
  ["asr", "Asr"],
  ["maghrib", "Maghrib"],
  ["isha", "Isha"],
] as const;

const VOICES = [
  { id: "off", label: "Off" },
  { id: "makkah", label: "Makkah" },
  { id: "madinah", label: "Madinah" },
  { id: "alafasy", label: "Mishary Alafasy" },
] as const;

type PrayerKey = (typeof PRAYERS)[number][0];

type Form = {
  name: string;
  latitude: string;
  longitude: string;
  timezone: string;
  method: string;
  asrSchool: "shafi" | "hanafi";
  tune: Record<PrayerKey, string>;
  iqamah: Record<PrayerKey, string>;
  jumuahKhutbah: string;
  jumuahIqamah: string;
  hijriOffset: string;
  theme: Theme;
  clock24h: boolean;
  showSunrise: boolean;
  showHijri: boolean;
  countdown: string;
  silence: string;
  audioVoice: string;
  audioMuteFajr: boolean;
  audioVolume: string;
};

function emptyTune(): Record<PrayerKey, string> {
  return { fajr: "", dhuhr: "", asr: "", maghrib: "", isha: "" };
}

function seed(config: SignageConfig | null): Form {
  const s = config?.settings ?? {};
  const tune = emptyTune();
  const iqamah = emptyTune();
  for (const [key] of PRAYERS) {
    tune[key] = s[`tune_${key}`] ?? "";
    iqamah[key] = s[`iqamah_${key}`] ?? "";
  }
  const theme = (s.theme ?? "ink") as Theme;
  return {
    name: config?.name ?? "Athan",
    latitude: s.latitude ?? "",
    longitude: s.longitude ?? "",
    timezone: s.timezone ?? "",
    method: (s.method || "mwl").toLowerCase(),
    asrSchool: s.asr_school === "hanafi" ? "hanafi" : "shafi",
    tune,
    iqamah,
    jumuahKhutbah: s.jumuah_khutbah ?? "",
    jumuahIqamah: s.jumuah_iqamah ?? "",
    hijriOffset: s.hijri_offset ?? "0",
    theme: theme in THEMES ? theme : "ink",
    clock24h: s.clock_24h !== "0",
    showSunrise: s.show_sunrise !== "0",
    showHijri: s.show_hijri !== "0",
    countdown: s.countdown_s ?? "60",
    silence: s.silence_s ?? "0",
    audioVoice: s.audio_voice ?? "off",
    audioMuteFajr: s.audio_mute_fajr !== "0",
    audioVolume: s.audio_volume ?? "80",
  };
}

function pack(form: Form): Record<string, string> {
  const settings: Record<string, string> = {
    latitude: form.latitude.trim(),
    longitude: form.longitude.trim(),
    method: form.method,
    timezone: form.timezone.trim() || "UTC",
    asr_school: form.asrSchool,
    theme: form.theme,
    clock_24h: form.clock24h ? "1" : "0",
    show_sunrise: form.showSunrise ? "1" : "0",
    show_hijri: form.showHijri ? "1" : "0",
    countdown_s: form.countdown.trim() || "60",
    silence_s: form.silence.trim() || "0",
    audio_voice: form.audioVoice,
    audio_mute_fajr: form.audioMuteFajr ? "1" : "0",
    audio_volume: form.audioVolume.trim() || "80",
    hijri_offset: form.hijriOffset.trim() || "0",
  };
  for (const [key] of PRAYERS) {
    const tune = form.tune[key].trim();
    if (tune) settings[`tune_${key}`] = tune;
    const iqamah = form.iqamah[key].trim();
    if (iqamah) settings[`iqamah_${key}`] = iqamah;
  }
  if (form.jumuahKhutbah.trim()) settings.jumuah_khutbah = form.jumuahKhutbah.trim();
  if (form.jumuahIqamah.trim()) settings.jumuah_iqamah = form.jumuahIqamah.trim();
  return settings;
}

export function AthanSheet({
  open,
  config,
  onClose,
  onSaved,
  onRemoved,
  onError,
  onDraft,
  embedded,
}: {
  open: boolean;
  config: SignageConfig | null;
  onClose: () => void;
  onSaved: () => void | Promise<void>;
  onRemoved: () => void | Promise<void>;
  onError: (message: string) => void;
  /** Packed settings as the person types. `null` when the sheet closes. */
  onDraft?: (settings: Record<string, string> | null) => void;
  /** Dock in the program editor instead of a covering inspector. */
  embedded?: boolean;
}) {
  const [form, setForm] = useState<Form>(() => seed(config));
  const [submitting, setSubmitting] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [removeOpen, setRemoveOpen] = useState(false);
  const draftRef = useRef(onDraft);
  draftRef.current = onDraft;

  useEffect(() => {
    setForm(seed(config));
    setFormError(null);
    setRemoveOpen(false);
  }, [config, open]);

  useEffect(() => {
    if (!open) {
      draftRef.current?.(null);
      return;
    }
    draftRef.current?.(pack(form));
  }, [open, form]);

  useEffect(() => {
    if (!open || config) return;
    if (typeof navigator === "undefined" || !("geolocation" in navigator)) return;
    let cancelled = false;
    navigator.geolocation.getCurrentPosition(
      (position) => {
        if (cancelled) return;
        setForm((current) => ({
          ...current,
          latitude: current.latitude || String(position.coords.latitude),
          longitude: current.longitude || String(position.coords.longitude),
        }));
      },
      () => {},
      { timeout: 5000 },
    );
    return () => {
      cancelled = true;
    };
  }, [open, config]);

  const set = <K extends keyof Form>(key: K, value: Form[K]) => {
    setForm((current) => ({ ...current, [key]: value }));
  };

  const handleCity = (city: CitySelection) => {
    setForm((current) => ({
      ...current,
      latitude: String(city.latitude),
      longitude: String(city.longitude),
      timezone: city.timezone,
    }));
  };

  const handleSave = async () => {
    if (!form.latitude.trim() || !form.longitude.trim()) {
      setFormError("A city is required");
      return;
    }
    if (!Number.isFinite(Number(form.latitude)) || !Number.isFinite(Number(form.longitude))) {
      setFormError("Latitude and longitude must be numbers");
      return;
    }
    setSubmitting(true);
    setFormError(null);
    try {
      await putConfig("athan", form.name.trim() || "Athan", pack(form));
      await onSaved();
      onClose();
    } catch (error) {
      const message = error instanceof Error ? error.message : "Save failed";
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
      await onRemoved();
      onClose();
    } catch (error) {
      const message = error instanceof Error ? error.message : "Could not remove this app";
      setFormError(message);
      onError(message);
    } finally {
      setSubmitting(false);
    }
  };

  const packed = pack(form);
  const actions = (
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
  );

  const fields = (
    <>
      <div className="ds-athan-live" aria-hidden={false}>
        <AthanPreview settings={packed} />
      </div>
      <p className="ds-hint" style={{ margin: 0 }}>
        {embedded
          ? "The card on the stage updates as you type. Save writes these times for every screen."
          : "These times apply to every Athan clip in this Space."}
      </p>

        <label className="ds-field">
          <span>Name</span>
          <input
            className="ds-input"
            value={form.name}
            onChange={(event) => set("name", event.target.value)}
          />
        </label>

        <section className="ds-athan-block">
          <h3>Place</h3>
          <CityPicker
            currentLatitude={form.latitude ? Number(form.latitude) : null}
            currentLongitude={form.longitude ? Number(form.longitude) : null}
            initialLabel={null}
            onSelect={handleCity}
          />
          <label className="ds-field">
            <span>Calculation method</span>
            <select
              className="ds-input"
              value={form.method}
              onChange={(event) => set("method", event.target.value)}
            >
              {METHODS.map((method) => (
                <option key={method.id} value={method.id}>
                  {method.label}
                </option>
              ))}
            </select>
          </label>
          <label className="ds-field">
            <span>Time zone</span>
            <input
              className="ds-input"
              value={form.timezone}
              onChange={(event) => set("timezone", event.target.value)}
            />
          </label>
        </section>

        <section className="ds-athan-block">
          <h3>Times</h3>
          <label className="ds-field">
            <span>Asr school</span>
            <select
              className="ds-input"
              value={form.asrSchool}
              onChange={(event) =>
                set("asrSchool", event.target.value === "hanafi" ? "hanafi" : "shafi")
              }
            >
              <option value="shafi">Shafi (standard)</option>
              <option value="hanafi">Hanafi</option>
            </select>
          </label>
          <div className="ds-athan-grid">
            <span />
            <span>Tune (min)</span>
            <span>Iqamah (min)</span>
            {PRAYERS.map(([key, label]) => (
              <FragmentRow
                key={key}
                label={label}
                tune={form.tune[key]}
                iqamah={form.iqamah[key]}
                onTune={(value) =>
                  setForm((current) => ({
                    ...current,
                    tune: { ...current.tune, [key]: value },
                  }))
                }
                onIqamah={(value) =>
                  setForm((current) => ({
                    ...current,
                    iqamah: { ...current.iqamah, [key]: value },
                  }))
                }
              />
            ))}
          </div>
          <div className="ds-athan-pair">
            <label className="ds-field">
              <span>Jumu’ah khutbah</span>
              <input
                className="ds-input"
                placeholder="HH:MM"
                value={form.jumuahKhutbah}
                onChange={(event) => set("jumuahKhutbah", event.target.value)}
              />
            </label>
            <label className="ds-field">
              <span>Jumu’ah iqamah</span>
              <input
                className="ds-input"
                placeholder="HH:MM"
                value={form.jumuahIqamah}
                onChange={(event) => set("jumuahIqamah", event.target.value)}
              />
            </label>
          </div>
          <label className="ds-field">
            <span>Hijri offset (days)</span>
            <input
              className="ds-input"
              type="number"
              min={-2}
              max={2}
              value={form.hijriOffset}
              onChange={(event) => set("hijriOffset", event.target.value)}
            />
          </label>
        </section>

        <section className="ds-athan-block">
          <h3>Look</h3>
          <div className="ds-theme-tiles" role="listbox" aria-label="Theme">
            {(Object.keys(THEMES) as Theme[]).map((theme) => (
              <button
                type="button"
                key={theme}
                className={`ds-theme-tile${form.theme === theme ? " is-on" : ""}`}
                style={{ background: THEMES[theme].bg, color: THEMES[theme].accent }}
                onClick={() => set("theme", theme)}
              >
                {theme}
              </button>
            ))}
          </div>
          <label className="ds-check">
            <input
              type="checkbox"
              checked={form.clock24h}
              onChange={(event) => set("clock24h", event.target.checked)}
            />
            24-hour clock
          </label>
          <label className="ds-check">
            <input
              type="checkbox"
              checked={form.showSunrise}
              onChange={(event) => set("showSunrise", event.target.checked)}
            />
            Show sunrise
          </label>
          <label className="ds-check">
            <input
              type="checkbox"
              checked={form.showHijri}
              onChange={(event) => set("showHijri", event.target.checked)}
            />
            Show Hijri date
          </label>
        </section>

        <section className="ds-athan-block">
          <h3>Sequence</h3>
          <div className="ds-athan-pair">
            <label className="ds-field">
              <span>Countdown (seconds)</span>
              <input
                className="ds-input"
                type="number"
                min={0}
                max={600}
                value={form.countdown}
                onChange={(event) => set("countdown", event.target.value)}
              />
            </label>
            <label className="ds-field">
              <span>Silence after iqamah (seconds)</span>
              <input
                className="ds-input"
                type="number"
                min={0}
                max={3600}
                value={form.silence}
                onChange={(event) => set("silence", event.target.value)}
              />
            </label>
          </div>
        </section>

        <section className="ds-athan-block">
          <h3>Audio</h3>
          <p className="ds-hint">Saved for the player. This head does not play the adhan yet.</p>
          <label className="ds-field">
            <span>Voice</span>
            <select
              className="ds-input"
              value={form.audioVoice}
              onChange={(event) => set("audioVoice", event.target.value)}
            >
              {VOICES.map((voice) => (
                <option key={voice.id} value={voice.id}>
                  {voice.label}
                </option>
              ))}
            </select>
          </label>
          <label className="ds-check">
            <input
              type="checkbox"
              checked={form.audioMuteFajr}
              onChange={(event) => set("audioMuteFajr", event.target.checked)}
            />
            Mute Fajr
          </label>
          <label className="ds-field">
            <span>Volume</span>
            <input
              className="ds-input"
              type="number"
              min={0}
              max={100}
              value={form.audioVolume}
              onChange={(event) => set("audioVolume", event.target.value)}
            />
          </label>
        </section>

        {formError && <p className="ds-danger-text">{formError}</p>}
    </>
  );

  return (
    <>
      {embedded ? (
        open ? (
          <section className="pe-athan-panel" aria-label="Athan">
            <header className="pe-athan-panel-head">
              <div>
                <strong>Athan</strong>
                <span className={`ds-badge${config ? " is-on" : ""}`}>
                  {config ? "Configured" : "Not configured"}
                </span>
              </div>
              <div className="pe-athan-panel-actions">
                <button
                  type="button"
                  className="ds-btn ds-btn-ghost"
                  disabled={submitting}
                  onClick={onClose}
                >
                  Done
                </button>
                {actions}
              </div>
            </header>
            <div className="pe-athan-panel-body">{fields}</div>
          </section>
        ) : null
      ) : (
        <Inspector
          open={open}
          onOpenChange={(next) => {
            if (!next) onClose();
          }}
          className="ds-app-setup ds-athan-setup"
          title="Athan"
          mark={
            <span className="ds-app-mark is-athan">
              <Moon size={22} strokeWidth={1.8} />
            </span>
          }
          kicker={
            <span className={`ds-badge${config ? " is-on" : ""}`}>
              {config ? "Configured" : "Not configured"}
            </span>
          }
          actions={actions}
        >
          {fields}
        </Inspector>
      )}
      <Confirm
        open={removeOpen}
        onOpenChange={setRemoveOpen}
        title="Remove Athan?"
        description="Programs using it will need another source."
        confirmLabel="Remove"
        danger
        onConfirm={handleRemove}
      />
    </>
  );
}

function FragmentRow({
  label,
  tune,
  iqamah,
  onTune,
  onIqamah,
}: {
  label: string;
  tune: string;
  iqamah: string;
  onTune: (value: string) => void;
  onIqamah: (value: string) => void;
}) {
  return (
    <>
      <span>{label}</span>
      <input
        className="ds-input"
        type="number"
        inputMode="numeric"
        value={tune}
        placeholder="0"
        onChange={(event) => onTune(event.target.value)}
      />
      <input
        className="ds-input"
        type="number"
        inputMode="numeric"
        value={iqamah}
        placeholder="—"
        onChange={(event) => onIqamah(event.target.value)}
      />
    </>
  );
}
