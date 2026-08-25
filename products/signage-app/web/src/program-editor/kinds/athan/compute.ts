/**
 * Prayer times for the athan kind. Same formulas as the display renderer
 * (`products/signage-app/src/athan.rs`). The screen is source of truth.
 *
 * London ISNA 2024-01-01 12:00 UTC (locked by the Rust test):
 * Fajr precedes sunrise, sunrise ≥ 07:30, maghrib ≤ 17:00, Isha after Maghrib.
 */

export type Clock = { hour: number; minute: number };
export type Prayer = { name: string; adhan: Clock; iqamah: Clock | null };
export type Theme = "ink" | "paper" | "emerald" | "night";

export type DayTimes = {
  prayers: Prayer[];
  next: number;
  nextIsIqamah: boolean;
  zone: string;
  nowLabel: string;
  hijriLabel: string;
  theme: Theme;
  clock24h: boolean;
  showIqamah: boolean;
  showHijri: boolean;
};

export const THEMES: Record<Theme, { bg: string; muted: string; accent: string }> = {
  ink: { bg: "#120e0a", muted: "#b4a082", accent: "#f3c07a" },
  paper: { bg: "#f6f1e8", muted: "#5a4e40", accent: "#241c14" },
  emerald: { bg: "#0c241c", muted: "#96b4a0", accent: "#dce8d2" },
  night: { bg: "#080a10", muted: "#8c96aa", accent: "#d2dceb" },
};

const METHODS: Record<string, { fajr: number; isha: number; maghribMin?: number; ishaMin?: number }> = {
  isna: { fajr: 15, isha: 15 },
  "2": { fajr: 15, isha: 15 },
  egypt: { fajr: 19.5, isha: 17.5 },
  egyptian: { fajr: 19.5, isha: 17.5 },
  "5": { fajr: 19.5, isha: 17.5 },
  makkah: { fajr: 18.5, isha: 0, ishaMin: 90 },
  mecca: { fajr: 18.5, isha: 0, ishaMin: 90 },
  "4": { fajr: 18.5, isha: 0, ishaMin: 90 },
  karachi: { fajr: 18, isha: 18 },
  "1": { fajr: 18, isha: 18 },
  tehran: { fajr: 17.7, isha: 14 },
  "7": { fajr: 17.7, isha: 14 },
  jafari: { fajr: 16, isha: 14, maghribMin: 4 },
  shia: { fajr: 16, isha: 14, maghribMin: 4 },
  "0": { fajr: 16, isha: 14, maghribMin: 4 },
  mwl: { fajr: 18, isha: 17 },
  "3": { fajr: 18, isha: 17 },
};

const HIJRI_MONTHS = [
  "Muharram",
  "Safar",
  "Rabi I",
  "Rabi II",
  "Jumada I",
  "Jumada II",
  "Rajab",
  "Shaban",
  "Ramadan",
  "Shawwal",
  "Dhul-Qadah",
  "Dhul-Hijjah",
];

/** Space config wins over a Kind row snapshot. */
export function overlay(
  space: Record<string, string> | null | undefined,
  entry: Record<string, string>,
): Record<string, string> {
  return space ? { ...entry, ...space } : { ...entry };
}

export function formatClock(clock: Clock, clock24h: boolean): string {
  if (clock24h) {
    return `${pad(clock.hour)}:${pad(clock.minute)}`;
  }
  const period = clock.hour >= 12 ? "PM" : "AM";
  const hour12 = clock.hour % 12 === 0 ? 12 : clock.hour % 12;
  return `${hour12}:${pad(clock.minute)} ${period}`;
}

export function athanTimes(
  settings: Record<string, string>,
  now = new Date(),
): DayTimes | null {
  const lat = Number(settings.latitude);
  const lng = Number(settings.longitude);
  if (!Number.isFinite(lat) || !Number.isFinite(lng)) return null;
  if (lat < -90 || lat > 90 || lng < -180 || lng > 180) return null;
  const method =
    METHODS[(settings.method ?? "").trim().toLowerCase()] ?? METHODS.mwl;
  const asrFactor = (settings.asr_school ?? "").trim().toLowerCase() === "hanafi" ? 2 : 1;
  const zone = settings.timezone?.trim() || "UTC";
  let local: Date;
  try {
    local = new Date(now.toLocaleString("en-US", { timeZone: zone }));
  } catch {
    local = now;
  }
  const offsetHours =
    (Date.UTC(
      local.getFullYear(),
      local.getMonth(),
      local.getDate(),
      local.getHours(),
      local.getMinutes(),
    ) -
      Date.UTC(
        now.getUTCFullYear(),
        now.getUTCMonth(),
        now.getUTCDate(),
        now.getUTCHours(),
        now.getUTCMinutes(),
      )) /
    3_600_000;
  const hours = compute(
    lat,
    lng,
    offsetHours,
    local.getFullYear(),
    local.getMonth() + 1,
    local.getDate(),
    method,
    asrFactor,
  );
  if (!hours?.fajr || hours.maghrib == null) return null;
  const fajr = prayerRow("Fajr", hours.fajr, settings, "tune_fajr", "iqamah_fajr");
  const maghrib = prayerRow("Maghrib", hours.maghrib, settings, "tune_maghrib", "iqamah_maghrib");
  if (!fajr || !maghrib) return null;
  const prayers: Prayer[] = [fajr];
  if (flag(settings, "show_sunrise", true) && hours.sunrise != null) {
    const sunrise = prayerRow("Sunrise", hours.sunrise, settings, "tune_sunrise", "");
    if (sunrise) prayers.push(sunrise);
  }
  if (hours.dhuhr != null) {
    const dhuhr = prayerRow("Dhuhr", hours.dhuhr, settings, "tune_dhuhr", "iqamah_dhuhr");
    if (dhuhr) prayers.push(dhuhr);
  }
  if (hours.asr != null) {
    const asr = prayerRow("Asr", hours.asr, settings, "tune_asr", "iqamah_asr");
    if (asr) prayers.push(asr);
  }
  prayers.push(maghrib);
  if (hours.isha != null) {
    const isha = prayerRow("Isha", hours.isha, settings, "tune_isha", "iqamah_isha");
    if (isha) prayers.push(isha);
  }
  if (local.getDay() === 5) applyJumuah(prayers, settings);
  const current = local.getHours() * 60 + local.getMinutes();
  let next = 0;
  let nextIsIqamah = false;
  for (let i = 0; i < prayers.length; i += 1) {
    const adhan = minutesOf(prayers[i].adhan);
    const iqamahClock = prayers[i].iqamah;
    const iqamah = iqamahClock ? minutesOf(iqamahClock) : null;
    if (adhan > current) {
      next = i;
      nextIsIqamah = false;
      break;
    }
    if (iqamah != null && iqamah > current) {
      next = i;
      nextIsIqamah = true;
      break;
    }
  }
  const clock24h = flag(settings, "clock_24h", true);
  return {
    prayers,
    next,
    nextIsIqamah,
    zone,
    nowLabel: formatClock(
      { hour: local.getHours(), minute: local.getMinutes() },
      clock24h,
    ),
    hijriLabel: hijriLabel(
      local.getFullYear(),
      local.getMonth() + 1,
      local.getDate(),
      intIn(settings, "hijri_offset", 0, -2, 2),
    ),
    theme: parseTheme(settings.theme),
    clock24h,
    showIqamah: flag(settings, "show_iqamah", true) && prayers.some((row) => row.iqamah),
    showHijri: flag(settings, "show_hijri", true),
  };
}

function prayerRow(
  name: string,
  hours: number,
  settings: Record<string, string>,
  tuneKey: string,
  iqamahKey: string,
): Prayer | null {
  const adhan = hoursToClock(hours + intIn(settings, tuneKey, 0, -30, 30) / 60);
  if (!adhan) return null;
  let iqamah: Clock | null = null;
  if (iqamahKey) {
    const raw = (settings[iqamahKey] ?? "").trim();
    if (raw !== "") {
      const offset = Number(raw);
      if (Number.isFinite(offset)) {
        iqamah = addMinutes(adhan, clamp(offset, 0, 180));
      }
    }
  }
  return { name, adhan, iqamah };
}

function applyJumuah(prayers: Prayer[], settings: Record<string, string>) {
  const dhuhr = prayers.find((row) => row.name === "Dhuhr");
  if (!dhuhr) return;
  const khutbah = parseHhmm(settings.jumuah_khutbah);
  if (khutbah) dhuhr.adhan = khutbah;
  const iqamah = parseHhmm(settings.jumuah_iqamah);
  if (iqamah) dhuhr.iqamah = iqamah;
}

function parseHhmm(raw: string | undefined): Clock | null {
  if (!raw) return null;
  const [h, m] = raw.split(":");
  const hour = Number(h);
  const minute = Number(m);
  if (!Number.isInteger(hour) || !Number.isInteger(minute)) return null;
  if (hour > 23 || minute > 59 || hour < 0 || minute < 0) return null;
  return { hour, minute };
}

function hoursToClock(hours: number): Clock | null {
  if (!Number.isFinite(hours)) return null;
  const wrapped = fixhour(hours);
  let hour = Math.floor(wrapped);
  let minute = Math.round((wrapped - hour) * 60);
  if (minute === 60) {
    minute = 0;
    hour += 1;
  }
  return { hour: ((hour % 24) + 24) % 24, minute };
}

function addMinutes(clock: Clock, minutes: number): Clock {
  const total = clock.hour * 60 + clock.minute + minutes;
  const wrapped = ((total % (24 * 60)) + 24 * 60) % (24 * 60);
  return { hour: Math.floor(wrapped / 60), minute: wrapped % 60 };
}

function minutesOf(clock: Clock): number {
  return clock.hour * 60 + clock.minute;
}

function parseTheme(raw: string | undefined): Theme {
  switch ((raw ?? "").trim().toLowerCase()) {
    case "paper":
      return "paper";
    case "emerald":
      return "emerald";
    case "night":
      return "night";
    default:
      return "ink";
  }
}

function flag(settings: Record<string, string>, key: string, fallback: boolean): boolean {
  const raw = settings[key];
  if (raw === "0" || raw === "false") return false;
  if (raw === "1" || raw === "true") return true;
  return fallback;
}

function intIn(
  settings: Record<string, string>,
  key: string,
  fallback: number,
  lo: number,
  hi: number,
): number {
  const value = Number(settings[key]);
  if (!Number.isFinite(value)) return fallback;
  return clamp(value, lo, hi);
}

function clamp(value: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, Math.round(value)));
}

function hijriLabel(year: number, month: number, day: number, offsetDays: number): string {
  const shifted = new Date(Date.UTC(year, month - 1, day + offsetDays));
  const [hy, hm, hd] = gregorianToHijri(
    shifted.getUTCFullYear(),
    shifted.getUTCMonth() + 1,
    shifted.getUTCDate(),
  );
  const name = HIJRI_MONTHS[Math.min(11, Math.max(0, hm - 1))] ?? "Muharram";
  return `${hd} ${name} ${hy}`;
}

function gregorianToHijri(year: number, month: number, day: number): [number, number, number] {
  const jd = Math.floor(julian(year, month, day));
  let l = jd - 1_948_440 + 10_632;
  const n = Math.floor((l - 1) / 10_631);
  l = l - 10_631 * n + 354;
  const j =
    Math.floor((10_985 - l) / 5316) * Math.floor((50 * l) / 17_719) +
    Math.floor(l / 5670) * Math.floor((43 * l) / 15_238);
  l =
    l -
    Math.floor((30 - j) / 15) * Math.floor((17_719 * j) / 50) -
    Math.floor(j / 16) * Math.floor((15_238 * j) / 43) +
    29;
  const m = Math.floor((24 * l) / 709);
  const d = l - Math.floor((709 * m) / 24);
  const y = 30 * n + j - 30;
  return [y, m, d];
}

function pad(n: number): string {
  return String(n).padStart(2, "0");
}

function dtr(d: number): number {
  return (d * Math.PI) / 180;
}
function rtd(r: number): number {
  return (r * 180) / Math.PI;
}
function sin(d: number): number {
  return Math.sin(dtr(d));
}
function cos(d: number): number {
  return Math.cos(dtr(d));
}
function tan(d: number): number {
  return Math.tan(dtr(d));
}
function arcsin(x: number): number {
  return rtd(Math.asin(x));
}
function arccos(x: number): number {
  return rtd(Math.acos(x));
}
function arctan2(y: number, x: number): number {
  return rtd(Math.atan2(y, x));
}
function arccot(x: number): number {
  return rtd(Math.atan(1 / x));
}
function fix(value: number, modulus: number): number {
  let wrapped = value % modulus;
  if (wrapped < 0) wrapped += modulus;
  return wrapped;
}
function fixangle(d: number): number {
  return fix(d, 360);
}
function fixhour(h: number): number {
  return fix(h, 24);
}

function julian(year: number, month: number, day: number): number {
  let y = year;
  let m = month;
  if (m <= 2) {
    y -= 1;
    m += 12;
  }
  const a = Math.floor(y / 100);
  const b = 2 - a + Math.floor(a / 4);
  return Math.floor(365.25 * (y + 4716)) + Math.floor(30.6001 * (m + 1)) + day + b - 1524.5;
}

function sun(jd: number): { decl: number; eqt: number } {
  const d = jd - 2451545;
  const g = fixangle(357.529 + 0.98560028 * d);
  const q = fixangle(280.459 + 0.98564736 * d);
  const L = fixangle(q + 1.915 * sin(g) + 0.02 * sin(2 * g));
  const e = 23.439 - 0.00000036 * d;
  const ra = fixhour(arctan2(cos(e) * sin(L), cos(L)) / 15);
  return { decl: arcsin(sin(e) * sin(L)), eqt: q / 15 - ra };
}

function midday(jd: number, portion: number): number {
  return fixhour(12 - sun(jd + portion).eqt);
}

function sunAngle(
  jd: number,
  lat: number,
  angle: number,
  portion: number,
  before: boolean,
): number | null {
  const decl = sun(jd + portion).decl;
  const noon = midday(jd, portion);
  const cosine = (-sin(angle) - sin(decl) * sin(lat)) / (cos(decl) * cos(lat));
  if (cosine < -1 || cosine > 1) return null;
  const t = arccos(cosine) / 15;
  return before ? noon - t : noon + t;
}

function asrTime(jd: number, lat: number, factor: number, portion: number): number | null {
  const decl = sun(jd + portion).decl;
  const angle = -arccot(factor + tan(Math.abs(lat - decl)));
  return sunAngle(jd, lat, angle, portion, false);
}

function compute(
  lat: number,
  lng: number,
  tz: number,
  year: number,
  month: number,
  day: number,
  method: { fajr: number; isha: number; maghribMin?: number; ishaMin?: number },
  asrFactor: number,
): {
  fajr: number | null;
  sunrise: number | null;
  dhuhr: number | null;
  asr: number | null;
  maghrib: number | null;
  isha: number | null;
} {
  const jd = julian(year, month, day) - lng / (15 * 24);
  let fajr: number | null = null;
  let sunrise: number | null = null;
  let dhuhr: number | null = null;
  let asr: number | null = null;
  let sunset: number | null = null;
  let maghrib: number | null = null;
  let isha: number | null = null;
  for (let i = 0; i < 2; i += 1) {
    fajr = sunAngle(jd, lat, method.fajr, (fajr ?? 5) / 24, true);
    sunrise = sunAngle(jd, lat, 0.833, (sunrise ?? 6) / 24, true);
    dhuhr = midday(jd, (dhuhr ?? 12) / 24);
    asr = asrTime(jd, lat, asrFactor, (asr ?? 13) / 24);
    sunset = sunAngle(jd, lat, 0.833, (sunset ?? 18) / 24, false);
    maghrib =
      sunset == null ? null : method.maghribMin != null ? sunset + method.maghribMin / 60 : sunset;
    isha =
      method.ishaMin != null
        ? maghrib == null
          ? null
          : maghrib + method.ishaMin / 60
        : sunAngle(jd, lat, method.isha, (isha ?? 18) / 24, false);
  }
  const shift = tz - lng / 15;
  const wrap = (value: number | null) => (value == null ? null : fixhour(value + shift));
  return {
    fajr: wrap(fajr),
    sunrise: wrap(sunrise),
    dhuhr: wrap(dhuhr),
    asr: wrap(asr),
    maghrib: wrap(maghrib),
    isha: wrap(isha),
  };
}
