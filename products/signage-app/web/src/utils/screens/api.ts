/**
 * Screens — names, where they are, what is true of them, and what they are
 * tuned to. Device lifecycle (pairing, grants, revocation) is Astrolabe's and
 * has no surface here.
 *
 * A screen is a document: reads return it whole, writes put it whole. What it
 * *plays* is no longer written onto it — a screen tunes to a channel, and
 * broadcasts interrupt. Assigning a program per panel is what made addressing
 * a fleet expensive, so the verb for it is gone rather than reimplemented.
 */

import { rpc } from '../api/client';
import { normalizeScreen } from '../lait/normalize';
import { mintBodyId } from '../lait/ids';
import type {
  Playback,
  PlaysReply,
  ScreenReply,
  ScreenSavedReply,
  ScreensReply,
  ShowingReply,
  SignageScreen,
} from '../lait/types';
import { resolvePlayback, type ResolutionInputs } from '../lait/resolve';

export async function fetchScreens(): Promise<SignageScreen[]> {
  const reply = await rpc<ScreensReply>({ cmd: 'screen_list' });
  return reply.screens.map(normalizeScreen);
}

export async function fetchScreen(id: string): Promise<SignageScreen | null> {
  const reply = await rpc<ScreenReply>({ cmd: 'screen_get', screen: id });
  return reply.screen ? normalizeScreen(reply.screen) : null;
}

export async function saveScreen(screen: SignageScreen): Promise<string> {
  const reply = await rpc<ScreenSavedReply>({ cmd: 'screen_put', screen });
  return reply.screen;
}

export async function createScreen(name: string): Promise<SignageScreen> {
  const screen: SignageScreen = {
    id: mintBodyId(),
    name,
    place: null,
    facts: {},
    sync: null,
    labels: [],
    tuned: null,
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

/** Point a screen at a channel, or at nothing. */
export async function tuneScreen(
  screenId: string,
  channelId: string | null,
): Promise<void> {
  const screen = await fetchScreen(screenId);
  if (!screen) throw new Error('that screen is not here');
  await saveScreen({ ...screen, tuned: channelId });
}

/** Labels are the operator's own vocabulary — overlapping and arbitrary. */
export async function setScreenLabels(
  screenId: string,
  labels: string[],
): Promise<void> {
  const screen = await fetchScreen(screenId);
  if (!screen) throw new Error('that screen is not here');
  // Sorted and deduplicated: the contract wants them strictly ascending, and
  // a screen labelled twice is a screen whose audience count is wrong.
  const tidy = [...new Set(labels.map((label) => label.trim()).filter(Boolean))].sort();
  await saveScreen({ ...screen, labels: tidy });
}

/**
 * Everything resolution takes for one screen, and the answer at this clock.
 *
 * The World holds no clock, so it returns the inputs and the caller resolves.
 * That is not a limitation worked around: a receiver deciding what to show
 * from a coordinator's clock would keep showing yesterday's broadcast through
 * a partition, which is the case the whole ladder exists to survive.
 */
export async function fetchScreenPlays(
  id: string,
): Promise<{ inputs: ResolutionInputs; playback: Playback | null }> {
  const reply = await rpc<PlaysReply>({ cmd: 'screen_plays', screen: id });
  const inputs: ResolutionInputs = {
    screen: reply.screen ? normalizeScreen(reply.screen) : null,
    channels: reply.channels ?? [],
    broadcasts: reply.broadcasts ?? [],
    audiences: reply.audiences ?? [],
    programs: reply.programs ?? [],
    media: reply.media ?? [],
    presets: reply.presets ?? [],
  };
  return {
    inputs,
    playback: inputs.screen ? resolvePlayback(inputs, Date.now()) : null,
  };
}

/** Which screens a program could reach — asked before a rename or a delete. */
export async function fetchScreensShowing(program: string): Promise<string[]> {
  const reply = await rpc<ShowingReply>({ cmd: 'screen_showing', program });
  return reply.screens;
}
