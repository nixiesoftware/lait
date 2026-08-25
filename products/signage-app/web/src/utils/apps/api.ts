/**
 * Kind presentation, addressing, and the standing streams screens tune to.
 *
 * Settings stay an untyped string map on purpose — what a kind's settings
 * *mean* is this application's knowledge, and it lives in
 * `program-editor/kinds`, where one declaration renders the panel, the preview
 * and the summary. This module is the transport and nothing else.
 *
 * There is no `putConfig` any more, and no read-before-write behind it. A kind
 * may have as many presets as somebody finds useful; the old uniqueness rule
 * existed only to make a lookup *by kind* unambiguous, and entries name their
 * preset by id now.
 */

import { rpc } from '../api/client';
import { mintBodyId } from '../lait/ids';
import type {
  AudienceSavedReply,
  AudiencesReply,
  BroadcastSavedReply,
  BroadcastsReply,
  ChannelSavedReply,
  ChannelsReply,
  Match,
  PresetSavedReply,
  PresetsReply,
  ReachesReply,
  SignageAudience,
  SignageBroadcast,
  SignageChannel,
  SignagePreset,
} from '../lait/types';

// ── presets ─────────────────────────────────────────────────────────────────

export async function fetchPresets(): Promise<SignagePreset[]> {
  const reply = await rpc<PresetsReply>({ cmd: 'preset_list' });
  return reply.presets;
}

export async function savePreset(preset: SignagePreset): Promise<string> {
  const reply = await rpc<PresetSavedReply>({ cmd: 'preset_put', preset });
  return reply.preset;
}

export async function createPreset(
  kind: string,
  name: string,
  settings: Record<string, string>,
): Promise<SignagePreset> {
  const preset: SignagePreset = { id: mintBodyId(), kind, name, settings };
  await savePreset(preset);
  return preset;
}

export async function deletePreset(id: string): Promise<void> {
  await rpc({ cmd: 'preset_delete', preset: id }, { confirm: true });
}

// ── channels ────────────────────────────────────────────────────────────────

export async function fetchChannels(): Promise<SignageChannel[]> {
  const reply = await rpc<ChannelsReply>({ cmd: 'channel_list' });
  return reply.channels;
}

export async function saveChannel(channel: SignageChannel): Promise<string> {
  const reply = await rpc<ChannelSavedReply>({ cmd: 'channel_put', channel });
  return reply.channel;
}

export async function deleteChannel(id: string): Promise<void> {
  await rpc({ cmd: 'channel_delete', channel: id }, { confirm: true });
}

// ── audiences ───────────────────────────────────────────────────────────────

export async function fetchAudiences(): Promise<SignageAudience[]> {
  const reply = await rpc<AudiencesReply>({ cmd: 'audience_list' });
  return reply.audiences;
}

export async function saveAudience(audience: SignageAudience): Promise<string> {
  const reply = await rpc<AudienceSavedReply>({ cmd: 'audience_put', audience });
  return reply.audience;
}

export async function deleteAudience(id: string): Promise<void> {
  await rpc({ cmd: 'audience_delete', audience: id }, { confirm: true });
}

/**
 * Which screens an audience reaches, right now.
 *
 * Asked before anything is sent, and answered by the same evaluator that will
 * decide — an expressive audience whose blast radius you learn about
 * afterwards is the dangerous kind, and the emergency case is precisely the
 * one that reaches everything.
 *
 * It is a lower bound: the World holds no clock and no observations, so a
 * reactive term reaches nobody from here. Present it as "at least these".
 */
export async function audienceReaches(id: string): Promise<string[]> {
  const reply = await rpc<ReachesReply>({ cmd: 'audience_reaches', audience: id });
  return reply.screens;
}

/** A rule with no name yet — saved so it can be previewed and reused. */
export function draftAudience(name: string, rule: Match): SignageAudience {
  return { id: mintBodyId(), name, rule };
}

// ── broadcasts ──────────────────────────────────────────────────────────────

export async function fetchBroadcasts(): Promise<SignageBroadcast[]> {
  const reply = await rpc<BroadcastsReply>({ cmd: 'broadcast_list' });
  return reply.broadcasts;
}

export async function saveBroadcast(broadcast: SignageBroadcast): Promise<string> {
  const reply = await rpc<BroadcastSavedReply>({ cmd: 'broadcast_put', broadcast });
  return reply.broadcast;
}

export async function deleteBroadcast(id: string): Promise<void> {
  await rpc({ cmd: 'broadcast_delete', broadcast: id }, { confirm: true });
}

/**
 * Stop a broadcast now rather than waiting for its window to close.
 *
 * A cancellation, not a deletion: the record stays, so "what interrupted the
 * menus at 14:30 and who stopped it" is still answerable. Lifted from CAP,
 * where an all-clear has to travel faster than an expiry.
 */
export async function cancelBroadcast(
  broadcast: SignageBroadcast,
  atUnixMs = Date.now(),
): Promise<void> {
  await saveBroadcast({ ...broadcast, cancelled_at_unix_ms: atUnixMs });
}
