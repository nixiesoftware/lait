/**
 * The Athan panel, declared.
 *
 * Every control the screen actually reads is listed here, which is how
 * `show_iqamah` got one: the renderer has honoured that key since the kind
 * landed and no form ever offered it. A key the renderer reads and the panel
 * omits is a setting nobody can reach.
 */

import { Moon } from "lucide-react";
import type { SignageConfig } from "@/utils/lait/types";
import {
  mergeSettings,
  seedFrom,
  type Draft,
  type FieldError,
  type Group,
  type KindPanel,
  type Settings,
} from "../types";
import { THEMES } from "./compute";
import { AthanPreview } from "./Preview";

const METHODS = [
  { value: "mwl", label: "Muslim World League" },
  { value: "isna", label: "ISNA" },
  { value: "egypt", label: "Egyptian" },
  { value: "makkah", label: "Umm al-Qura" },
  { value: "karachi", label: "Karachi" },
  { value: "tehran", label: "Tehran" },
  { value: "jafari", label: "Jafari" },
];

const PRAYERS = [
  { value: "fajr", label: "Fajr" },
  { value: "dhuhr", label: "Dhuhr" },
  { value: "asr", label: "Asr" },
  { value: "maghrib", label: "Maghrib" },
  { value: "isha", label: "Isha" },
];

const VOICES = [
  { value: "off", label: "Off" },
  { value: "makkah", label: "Makkah" },
  { value: "madinah", label: "Madinah" },
  { value: "alafasy", label: "Mishary Alafasy" },
];

const GROUPS: Group[] = [
  {
    id: "place",
    label: "Place",
    note: "Times are computed on the screen from this location. Nothing is fetched.",
    fields: [
      {
        control: "place",
        label: "Location",
        keys: { latitude: "latitude", longitude: "longitude", timezone: "timezone" },
      },
      {
        control: "select",
        key: "method",
        label: "Calculation method",
        options: METHODS,
      },
    ],
  },
  {
    id: "times",
    label: "Times",
    fields: [
      {
        control: "select",
        key: "asr_school",
        label: "Asr school",
        options: [
          { value: "shafi", label: "Shafi (standard)" },
          { value: "hanafi", label: "Hanafi" },
        ],
      },
      {
        control: "matrix",
        label: "Per prayer",
        rows: PRAYERS,
        columns: [
          {
            id: "tune",
            label: "Tune (min)",
            keyFor: (row) => `tune_${row}`,
            min: -30,
            max: 30,
            placeholder: "0",
          },
          {
            id: "iqamah",
            label: "Iqamah (min)",
            keyFor: (row) => `iqamah_${row}`,
            min: 0,
            max: 180,
            placeholder: "—",
          },
        ],
      },
      { control: "time", key: "jumuah_khutbah", label: "Jumu’ah khutbah" },
      { control: "time", key: "jumuah_iqamah", label: "Jumu’ah iqamah" },
      {
        control: "int",
        key: "hijri_offset",
        label: "Hijri offset",
        min: -2,
        max: 2,
        unit: "days",
      },
    ],
  },
  {
    id: "look",
    label: "Look",
    fields: [
      {
        control: "swatch",
        key: "theme",
        label: "Theme",
        options: (Object.keys(THEMES) as (keyof typeof THEMES)[]).map((theme) => ({
          value: theme,
          label: theme,
          bg: THEMES[theme].bg,
          fg: THEMES[theme].accent,
        })),
      },
      { control: "toggle", key: "clock_24h", label: "24-hour clock" },
      { control: "toggle", key: "show_sunrise", label: "Show sunrise" },
      {
        control: "toggle",
        key: "show_iqamah",
        label: "Show iqamah column",
        hint: "Hidden anyway when no prayer has an iqamah offset.",
      },
      { control: "toggle", key: "show_hijri", label: "Show Hijri date" },
    ],
  },
  {
    id: "sequence",
    label: "Sequence",
    fields: [
      {
        control: "int",
        key: "countdown_s",
        label: "Countdown before adhan",
        min: 0,
        max: 600,
        unit: "s",
      },
      {
        control: "int",
        key: "silence_s",
        label: "Silence after iqamah",
        min: 0,
        max: 3600,
        unit: "s",
      },
    ],
  },
  {
    id: "audio",
    label: "Audio",
    note: "Saved for the player. This head does not play the adhan yet.",
    fields: [
      { control: "select", key: "audio_voice", label: "Voice", options: VOICES },
      { control: "toggle", key: "audio_mute_fajr", label: "Mute Fajr" },
      {
        control: "int",
        key: "audio_volume",
        label: "Volume",
        min: 0,
        max: 100,
      },
    ],
  },
];

const DEFAULTS: Settings = {
  method: "mwl",
  timezone: "",
  asr_school: "shafi",
  theme: "ink",
  clock_24h: "1",
  show_sunrise: "1",
  show_iqamah: "1",
  show_hijri: "1",
  countdown_s: "60",
  silence_s: "0",
  hijri_offset: "0",
  audio_voice: "off",
  audio_mute_fajr: "1",
  audio_volume: "80",
};

function validate(draft: Draft): FieldError[] {
  const errors: FieldError[] = [];
  const lat = Number(draft.latitude);
  const lng = Number(draft.longitude);
  if (!draft.latitude?.trim() || !draft.longitude?.trim()) {
    errors.push({ key: "latitude", message: "A location is required" });
  } else if (!Number.isFinite(lat) || !Number.isFinite(lng)) {
    errors.push({ key: "latitude", message: "Latitude and longitude must be numbers" });
  } else if (lat < -90 || lat > 90) {
    errors.push({ key: "latitude", message: "Latitude runs from -90 to 90" });
  } else if (lng < -180 || lng > 180) {
    errors.push({ key: "longitude", message: "Longitude runs from -180 to 180" });
  }
  // The renderer falls back to UTC, which computes a plausible timetable for
  // the wrong place rather than failing. Refuse it here instead.
  if (!draft.timezone?.trim()) {
    errors.push({ key: "timezone", message: "A time zone is required" });
  }
  for (const field of ["jumuah_khutbah", "jumuah_iqamah"]) {
    const raw = draft[field]?.trim();
    if (raw && !/^([01]\d|2[0-3]):[0-5]\d$/.test(raw)) {
      errors.push({ key: field, message: "Use HH:MM" });
    }
  }
  return errors;
}

function summarize(config: SignageConfig | null): string {
  if (!config) return "Prayer times for a location, rendered as a schedule card.";
  const zone = config.settings.timezone;
  const lat = config.settings.latitude;
  const lng = config.settings.longitude;
  if (zone && lat && lng) {
    return `${zone} · ${Number(lat).toFixed(2)}°, ${Number(lng).toFixed(2)}°`;
  }
  return zone || config.name;
}

export const athanPanel: KindPanel = {
  kind: "athan",
  label: "Athan",
  description: "Prayer times for a location, rendered as a schedule card.",
  Icon: Moon,
  tone: "athan",
  scope: "space",
  groups: GROUPS,
  defaults: DEFAULTS,
  defaultDurationMs: 60_000,
  seed: (config) => seedFrom(athanPanel, config),
  pack: (draft, existing) => mergeSettings(draft, existing),
  validate,
  summarize,
  Preview: AthanPreview,
};
