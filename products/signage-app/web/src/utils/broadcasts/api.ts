/**
 * Programs — what Medusa called broadcasts/playlists.
 *
 * The item-level verbs (create/update/delete/reorder item) do not exist on
 * this surface and never will: one `program_put` carries the whole ordered
 * `items[]`, so there is nothing to leave half-applied. The editor edits a
 * document and saves the document.
 */

import { rpc } from '../api/client';
import { mintBodyId } from '../lait/ids';
import type {
  DeletedReply,
  ProgramReply,
  ProgramsReply,
  SavedReply,
  ShowingReply,
  SignageMedia,
  SignageProgram,
} from '../lait/types';

export interface ProgramWithLibrary {
  program: SignageProgram;
  /** The library entries the items name, in item order, deduplicated. */
  media: SignageMedia[];
}

export async function fetchPrograms(): Promise<SignageProgram[]> {
  const reply = await rpc<ProgramsReply>({ cmd: 'program_list' });
  return reply.programs;
}

export async function fetchProgram(id: string): Promise<ProgramWithLibrary | null> {
  const reply = await rpc<ProgramReply>({ cmd: 'program_get', program: id });
  if (!reply.program) return null;
  return { program: reply.program, media: reply.media };
}

export async function saveProgram(program: SignageProgram): Promise<string> {
  const reply = await rpc<SavedReply>({ cmd: 'program_put', program });
  return reply.program;
}

export async function createProgram(name: string): Promise<SignageProgram> {
  const program: SignageProgram = {
    id: mintBodyId(),
    name,
    cycle: 'loop',
    items: [],
    windows: [],
  };
  await saveProgram(program);
  return program;
}

export async function deleteProgram(id: string): Promise<void> {
  await rpc<DeletedReply>({ cmd: 'program_delete', program: id }, { confirm: true });
}

/** Which screens intend this program — the World's own index, not an N+1. */
export async function fetchProgramScreens(id: string): Promise<string[]> {
  const reply = await rpc<ShowingReply>({ cmd: 'screen_showing', program: id });
  return reply.screens;
}
