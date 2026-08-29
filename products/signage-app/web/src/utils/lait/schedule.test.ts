import { describe, expect, it } from 'vitest';

import { broadcastStatus, clockOfDay, nextQuarterHour, scheduledWindow } from './schedule';

describe('a broadcast scheduled from the composer', () => {
  const now = new Date(2026, 7, 28, 21, 7, 30); // 28 Aug 2026, 21:07:30 local

  it('is one window from a time for some minutes, today when the time is still ahead', () => {
    const window = scheduledWindow('21:30', 20, 80, now);
    expect(window).toMatchObject({
      start_local: '2026-08-28T21:30:00',
      duration_ms: 20 * 60_000,
      recurrence: 'none',
      until_unix_ms: null,
      priority: 80,
      enabled: true,
    });
    expect(window.timezone).toBe(Intl.DateTimeFormat().resolvedOptions().timeZone);
  });

  it('lands on tomorrow when the time has already passed today — nobody schedules the past', () => {
    expect(scheduledWindow('09:00', 5, 50, now).start_local).toBe('2026-08-29T09:00:00');
    // The very minute that just passed counts as passed.
    expect(scheduledWindow('21:07', 5, 50, now).start_local).toBe('2026-08-29T21:07:00');
  });

  it('is scheduled until its window opens, on air inside it, and ended after', () => {
    const span = { start: 22 * 3_600_000 + 15 * 60_000, end: 22 * 3_600_000 + 18 * 60_000 };
    expect(broadcastStatus(span, 22 * 3_600_000 + 9 * 60_000)).toEqual({ kind: 'scheduled', label: 'Scheduled 22:15' });
    expect(broadcastStatus(span, span.start)).toEqual({ kind: 'on_air' });
    expect(broadcastStatus(span, span.end)).toEqual({ kind: 'ended', label: 'Ended 22:18' });
    // No window at all — sent now — is on air until stopped; a window that
    // does not open today is neither.
    expect(broadcastStatus(undefined, null)).toEqual({ kind: 'on_air' });
    expect(broadcastStatus(null, 0)).toEqual({ kind: 'not_today' });
    expect(clockOfDay(0)).toBe('00:00');
  });

  it('suggests the next quarter hour as a start', () => {
    expect(nextQuarterHour(now)).toBe('21:15');
    expect(nextQuarterHour(new Date(2026, 7, 28, 23, 50))).toBe('00:00');
  });
});
