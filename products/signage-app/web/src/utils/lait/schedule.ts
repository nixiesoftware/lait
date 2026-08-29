/**
 * Windows a person schedules from the composer: once, in this browser's
 * zone, from a time for some minutes. Kept apart from the page so the one
 * rule that can go wrong — "today, or tomorrow if that time has passed" —
 * is a function a test can hold to a clock.
 */

import type { ScheduleWindow } from './types';

const pad = (n: number) => String(n).padStart(2, '0');

/** Milliseconds since local midnight, as `HH:MM`. */
export function clockOfDay(ms: number): string {
  return `${pad(Math.floor(ms / 3_600_000) % 24)}:${pad(Math.floor(ms / 60_000) % 60)}`;
}

export type BroadcastStatus =
  | { kind: 'on_air' }
  | { kind: 'scheduled'; label: string }
  | { kind: 'ended'; label: string }
  | { kind: 'not_today' };

/**
 * What a broadcast is doing right now, by its window and the clock: a window
 * still ahead is scheduled, not on air — the hub said "On air" of a broadcast
 * six minutes before it opened. `span` is the window laid over today, or
 * null when it does not open today; `nowOfDay` is the clock in the window's
 * own zone. A broadcast with no window is on air until it is stopped.
 */
export function broadcastStatus(
  span: { start: number; end: number } | null | undefined,
  nowOfDay: number | null,
): BroadcastStatus {
  if (span === undefined || nowOfDay === null) return { kind: 'on_air' };
  if (span === null) return { kind: 'not_today' };
  if (nowOfDay < span.start) return { kind: 'scheduled', label: `Scheduled ${clockOfDay(span.start)}` };
  if (nowOfDay >= span.end) return { kind: 'ended', label: `Ended ${clockOfDay(span.end)}` };
  return { kind: 'on_air' };
}

/** The next quarter hour, as `HH:MM` — a start a person can read and adjust. */
export function nextQuarterHour(now = new Date()): string {
  const at = new Date(now.getTime() + 15 * 60_000);
  at.setMinutes(Math.floor(at.getMinutes() / 15) * 15, 0, 0);
  return `${pad(at.getHours())}:${pad(at.getMinutes())}`;
}

/**
 * One window, once, in this browser's zone: today at the time given, or
 * tomorrow if that time has already passed — nobody schedules the past.
 */
export function scheduledWindow(
  startAt: string,
  minutes: number,
  priority: number,
  now = new Date(),
): ScheduleWindow {
  const [hours, mins] = startAt.split(':').map(Number);
  const start = new Date(now);
  start.setHours(hours, mins, 0, 0);
  if (start.getTime() <= now.getTime()) start.setDate(start.getDate() + 1);
  return {
    start_local: `${start.getFullYear()}-${pad(start.getMonth() + 1)}-${pad(start.getDate())}T${pad(hours)}:${pad(mins)}:00`,
    duration_ms: minutes * 60_000,
    recurrence: 'none',
    until_unix_ms: null,
    priority,
    enabled: true,
    timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
  };
}
