/**
 * Scheduling lives on the screen document: `schedule` puts a different
 * program on the screen inside a window, and `intent.over` overrides
 * everything until a stated moment. Both are edits to the document.
 *
 * What plays *now* is a resolution the engine owns (override → schedule →
 * direct → group, with civil-time occurrence expansion). This app renders
 * the inputs and does not run a twin of that resolver: a second
 * implementation disagreeing with the first is the defect, not a feature.
 */

import { actor } from '../api/client';
import { mintBodyId } from '../lait/ids';
import { fetchScreen, saveScreen } from '../screens/api';
import type { ProgramWindow, ScheduleWindow } from '../lait/types';

export async function fetchScreenSchedule(screenId: string): Promise<ProgramWindow[]> {
  const screen = await fetchScreen(screenId);
  return screen?.schedule ?? [];
}

export async function addScheduleWindow(
  screenId: string,
  programId: string,
  window: ScheduleWindow,
): Promise<ProgramWindow> {
  const screen = await fetchScreen(screenId);
  if (!screen) throw new Error(`no screen matches "${screenId}"`);
  const scheduled: ProgramWindow = { id: mintBodyId(), window, program: programId };
  screen.schedule = [...screen.schedule, scheduled];
  await saveScreen(screen);
  return scheduled;
}

export async function updateScheduleWindow(
  screenId: string,
  scheduled: ProgramWindow,
): Promise<void> {
  const screen = await fetchScreen(screenId);
  if (!screen) throw new Error(`no screen matches "${screenId}"`);
  screen.schedule = screen.schedule.map((row) => (row.id === scheduled.id ? scheduled : row));
  await saveScreen(screen);
}

export async function deleteScheduleWindow(screenId: string, windowId: string): Promise<void> {
  const screen = await fetchScreen(screenId);
  if (!screen) throw new Error(`no screen matches "${screenId}"`);
  screen.schedule = screen.schedule.filter((row) => row.id !== windowId);
  await saveScreen(screen);
}

/** Override everything on this screen until `untilUnixMs`. */
export async function setScreenOverride(
  screenId: string,
  programId: string,
  untilUnixMs: number,
): Promise<void> {
  const screen = await fetchScreen(screenId);
  if (!screen) throw new Error(`no screen matches "${screenId}"`);
  screen.intent = {
    ...screen.intent,
    over: {
      choice: {
        member: programId,
        chosen_unix_ms: Date.now(),
        chooser: await actor(),
      },
      until_unix_ms: untilUnixMs,
    },
  };
  await saveScreen(screen);
}

export async function clearScreenOverride(screenId: string): Promise<void> {
  const screen = await fetchScreen(screenId);
  if (!screen) throw new Error(`no screen matches "${screenId}"`);
  const { over: _cleared, ...rest } = screen.intent;
  screen.intent = rest;
  await saveScreen(screen);
}
