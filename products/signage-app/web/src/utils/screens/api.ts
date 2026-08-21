/**
 * Screens — names, groups, and intent. Device lifecycle (pairing, grants,
 * revocation) is Astrolabe's and has no surface here.
 *
 * A screen is a document: reads return it whole, writes put it whole. The
 * intent Slot carries the standing choice and any override; writing one
 * names this actor as the chooser, the tie-breaker two replicas agree on.
 */

import { actor, rpc } from '../api/client';
import { mintBodyId } from '../lait/ids';
import type {
  PlaysReply,
  ScreenReply,
  ScreenSavedReply,
  ScreensReply,
  SignageGroup,
  SignageScreen,
} from '../lait/types';

export async function fetchScreens(): Promise<SignageScreen[]> {
  const reply = await rpc<ScreensReply>({ cmd: 'screen_list' });
  return reply.screens;
}

export async function fetchScreen(id: string): Promise<SignageScreen | null> {
  const reply = await rpc<ScreenReply>({ cmd: 'screen_get', screen: id });
  return reply.screen;
}

export async function saveScreen(screen: SignageScreen): Promise<string> {
  const reply = await rpc<ScreenSavedReply>({ cmd: 'screen_put', screen });
  return reply.screen;
}

export async function createScreen(name: string): Promise<SignageScreen> {
  const screen: SignageScreen = {
    id: mintBodyId(),
    name,
    group: null,
    intent: {},
    schedule: [],
  };
  await saveScreen(screen);
  return screen;
}

export async function deleteScreen(id: string): Promise<void> {
  await rpc({ cmd: 'screen_delete', screen: id }, { confirm: true });
}

export async function deleteScreens(ids: string[]): Promise<void> {
  for (const id of ids) {
    await deleteScreen(id);
  }
}

/** The ladder's inputs: the screen and the group it inherits from, paired. */
export async function fetchScreenPlays(
  id: string,
): Promise<{ screen: SignageScreen | null; group: SignageGroup | null }> {
  const reply = await rpc<PlaysReply>({ cmd: 'screen_plays', screen: id });
  return { screen: reply.screen, group: reply.group ?? null };
}

/** Set this screen's own standing choice — the ladder's Direct rung. */
export async function assignProgramToScreen(
  screenId: string,
  programId: string,
): Promise<void> {
  const screen = await fetchScreen(screenId);
  if (!screen) throw new Error(`no screen matches "${screenId}"`);
  screen.intent = {
    ...screen.intent,
    base: {
      member: programId,
      chosen_unix_ms: Date.now(),
      chooser: await actor(),
    },
  };
  await saveScreen(screen);
}

/** Clear the direct choice; the screen falls through to its group. */
export async function removeProgramFromScreen(screenId: string): Promise<void> {
  const screen = await fetchScreen(screenId);
  if (!screen) throw new Error(`no screen matches "${screenId}"`);
  const { base: _cleared, ...rest } = screen.intent;
  screen.intent = rest;
  await saveScreen(screen);
}

/** Put this screen in a group, or `null` for none. */
export async function setScreenGroup(
  screenId: string,
  groupId: string | null,
): Promise<void> {
  const screen = await fetchScreen(screenId);
  if (!screen) throw new Error(`no screen matches "${screenId}"`);
  screen.group = groupId;
  await saveScreen(screen);
}
