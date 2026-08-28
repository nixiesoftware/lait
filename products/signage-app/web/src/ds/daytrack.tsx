/**
 * One day, drawn once.
 *
 * Time is horizontal everywhere in this product — the filmstrip, a channel's
 * dayparts, a broadcast's window, a screen's day — and it is the same axis
 * with the same playhead, so a person who has learned one has learned them
 * all. A segment is a thing on the track; a band is a thing laid over it. The
 * playhead is the action colour, because scrubbing is the act, and it moves
 * on the shared tick.
 *
 * Positions are authored, not resolved: the browser has no twin of the
 * schedule evaluator, so a segment is drawn where its window says it opens.
 */

import { useMemo, type ReactNode } from "react";
import type { SignageChannel, ScheduleWindow } from "@/utils/lait/types";

export const DAY_MS = 24 * 60 * 60 * 1000;

export type Segment = {
  id: string;
  /** Milliseconds from local midnight. */
  start: number;
  end: number;
  /** `ground` is what plays when nothing else is on; `part` is a daypart;
   *  `band` is a broadcast laid over the day. */
  tone: "ground" | "part" | "band";
  /** Stacking for bands: higher wins, and sits higher. */
  height?: number;
  onOpen?: () => void;
  title?: string;
  children?: ReactNode;
};

/** Civil time in a zone, as milliseconds since that zone's midnight. */
export function timeOfDayIn(now: number, timezone: string | null | undefined): number {
  try {
    const parts = new Intl.DateTimeFormat("en-GB", {
      timeZone: timezone || undefined,
      hour: "numeric",
      minute: "numeric",
      second: "numeric",
      hourCycle: "h23",
    }).formatToParts(new Date(now));
    const get = (type: string) => Number(parts.find((part) => part.type === type)?.value ?? 0);
    return ((get("hour") * 60 + get("minute")) * 60 + get("second")) * 1000;
  } catch {
    const date = new Date(now);
    return ((date.getHours() * 60 + date.getMinutes()) * 60 + date.getSeconds()) * 1000;
  }
}

function civilIn(now: number, timezone: string | null | undefined) {
  try {
    const parts = new Intl.DateTimeFormat("en-CA", {
      timeZone: timezone || undefined,
      year: "numeric",
      month: "2-digit",
      day: "2-digit",
      weekday: "short",
    }).formatToParts(new Date(now));
    const get = (type: string) => parts.find((part) => part.type === type)?.value ?? "";
    return { date: `${get("year")}-${get("month")}-${get("day")}`, weekday: get("weekday") };
  } catch {
    const date = new Date(now);
    return {
      date: date.toISOString().slice(0, 10),
      weekday: ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][date.getDay()],
    };
  }
}

/** Whether an authored window opens today, and from when. */
export function windowToday(
  window: ScheduleWindow,
  now: number,
): { start: number; end: number } | null {
  if (!window.enabled) return null;
  if (window.until_unix_ms != null && now > window.until_unix_ms) return null;
  const [datePart, timePart = "00:00:00"] = window.start_local.split("T");
  const [h, m, s = "0"] = timePart.split(":").map((v) => v);
  const start = ((Number(h) * 60 + Number(m)) * 60 + Number(s)) * 1000;
  if (!Number.isFinite(start)) return null;
  const today = civilIn(now, window.timezone);
  const opens = (() => {
    switch (window.recurrence) {
      case "none":
        return datePart === today.date;
      case "daily":
        return datePart <= today.date;
      case "weekly": {
        const authored = new Date(`${datePart}T12:00:00Z`);
        const weekday = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][authored.getUTCDay()];
        return datePart <= today.date && weekday === today.weekday;
      }
      case "monthly":
        return datePart <= today.date && datePart.slice(8, 10) === today.date.slice(8, 10);
    }
    return false;
  })();
  if (!opens) return null;
  return { start, end: Math.min(DAY_MS, start + window.duration_ms) };
}

/** A channel's day: its base as the ground, its dayparts as parts. */
export function channelDay(
  channel: SignageChannel,
  now: number,
  open?: (program: string) => void,
): Segment[] {
  const segments: Segment[] = [];
  if (channel.base) {
    segments.push({
      id: `base:${channel.base}`,
      start: 0,
      end: DAY_MS,
      tone: "ground",
      onOpen: open ? () => open(channel.base as string) : undefined,
    });
  }
  for (const part of channel.schedule ?? []) {
    const span = windowToday(part, now);
    if (!span) continue;
    segments.push({
      id: part.id,
      start: span.start,
      end: span.end,
      tone: "part",
      height: part.priority,
      onOpen: open ? () => open(part.program) : undefined,
    });
  }
  return segments;
}

const HOURS = [0, 6, 12, 18];

export function DayTrack({
  segments,
  now,
  timezone,
  size = "md",
  className,
}: {
  segments: Segment[];
  now: number;
  timezone?: string | null;
  size?: "sm" | "md";
  className?: string;
}) {
  const at = timeOfDayIn(now, timezone);
  const bands = useMemo(
    () => segments.filter((segment) => segment.tone === "band").sort((a, b) => (a.height ?? 0) - (b.height ?? 0)),
    [segments],
  );
  const parts = useMemo(() => segments.filter((segment) => segment.tone !== "band"), [segments]);
  const pct = (ms: number) => `${(Math.max(0, Math.min(DAY_MS, ms)) / DAY_MS) * 100}%`;

  return (
    <div className={`ds-day is-${size}${className ? ` ${className}` : ""}`} role="img" aria-label="Today">
      {bands.length > 0 && (
        <div className="ds-day-bands">
          {bands.map((band) => (
            <button
              type="button"
              key={band.id}
              className="ds-day-band"
              style={{ left: pct(band.start), width: pct(band.end - band.start) }}
              title={band.title}
              onClick={band.onOpen}
              disabled={!band.onOpen}
            >
              {band.children}
            </button>
          ))}
        </div>
      )}
      <div className="ds-day-track">
        {HOURS.map((hour) => (
          <i key={hour} className="ds-day-tick" style={{ left: pct(hour * 3_600_000) }} />
        ))}
        {parts.map((segment) => (
          <button
            type="button"
            key={segment.id}
            className={`ds-day-seg is-${segment.tone}`}
            style={{ left: pct(segment.start), width: pct(segment.end - segment.start) }}
            title={segment.title}
            onClick={segment.onOpen}
            disabled={!segment.onOpen}
          >
            {segment.children}
          </button>
        ))}
        <i className="ds-day-now" style={{ left: pct(at) }} />
      </div>
    </div>
  );
}
