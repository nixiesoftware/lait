/**
 * Groups — what Medusa called networks. A group's intent is what its screens
 * play when they choose nothing themselves: the bottom rung of the ladder.
 */

import { actor, rpc } from '../api/client';
import { mintBodyId } from '../lait/ids';
import type {
  GroupReply,
  GroupSavedReply,
  GroupsReply,
  SignageGroup,
} from '../lait/types';

export async function fetchGroups(): Promise<SignageGroup[]> {
  const reply = await rpc<GroupsReply>({ cmd: 'group_list' });
  return reply.groups;
}

export async function fetchGroup(id: string): Promise<SignageGroup | null> {
  const reply = await rpc<GroupReply>({ cmd: 'group_get', group: id });
  return reply.group;
}

export async function saveGroup(group: SignageGroup): Promise<string> {
  const reply = await rpc<GroupSavedReply>({ cmd: 'group_put', group });
  return reply.group;
}

export async function createGroup(name: string): Promise<SignageGroup> {
  const group: SignageGroup = {
    id: mintBodyId(),
    name,
    intent: {},
    screens: [],
  };
  await saveGroup(group);
  return group;
}

export async function deleteGroup(id: string): Promise<void> {
  await rpc({ cmd: 'group_delete', group: id }, { confirm: true });
}

/** Set what this group's screens play by default. */
export async function assignProgramToGroup(
  groupId: string,
  programId: string,
): Promise<void> {
  const group = await fetchGroup(groupId);
  if (!group) throw new Error(`no group matches "${groupId}"`);
  group.intent = {
    ...group.intent,
    base: {
      member: programId,
      chosen_unix_ms: Date.now(),
      chooser: await actor(),
    },
  };
  await saveGroup(group);
}

export async function removeProgramFromGroup(groupId: string): Promise<void> {
  const group = await fetchGroup(groupId);
  if (!group) throw new Error(`no group matches "${groupId}"`);
  const { base: _cleared, ...rest } = group.intent;
  group.intent = rest;
  await saveGroup(group);
}
