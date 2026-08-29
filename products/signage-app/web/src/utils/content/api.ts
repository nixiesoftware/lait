/**
 * The media library. Uploading is two steps with one owner each: the bytes
 * stream to the content plane (`POST /api/spaces/{orbit}/content`), then
 * `media_put` records the library entry naming the sealed content id.
 * Dimensions and duration are probed client-side, as Medusa already did —
 * they are caller-asserted facts on the record.
 */

import { LaitError, rpc, space } from '../api/client';
import { mintBodyId } from '../lait/ids';
import { normalizeMedia } from '../lait/normalize';
import type {
  LibraryReply,
  MediaReply,
  MediaSavedReply,
  SignageMedia,
  UsedByReply,
} from '../lait/types';
import { getFileDimensions } from '../uploads/getFileDimensions';

interface ContentWritten {
  kind: 'content';
  content: string;
  size: number;
}

async function uploadBytes(file: File): Promise<ContentWritten> {
  const orbit = await space();
  const r = await fetch(
    `/api/spaces/${encodeURIComponent(orbit)}/content?len=${file.size}`,
    {
      method: 'POST',
      credentials: 'same-origin',
      body: file,
    },
  );
  let body: Record<string, unknown> = {};
  try {
    body = (await r.json()) as Record<string, unknown>;
  } catch {
    body = {};
  }
  if (!r.ok || body.kind !== 'content' || typeof body.content !== 'string') {
    throw new LaitError(
      String(body.message ?? `upload refused (HTTP ${r.status})`),
      r.status,
      typeof body.error_kind === 'string' ? body.error_kind : null,
    );
  }
  return body as unknown as ContentWritten;
}

/** A file the library refuses, with its reason — never a silent skip. */
export interface RefusedUpload {
  file: File;
  reason: string;
}

export interface UploadOutcome {
  uploaded: SignageMedia[];
  refused: RefusedUpload[];
}

export async function uploadContentAll(files: File[]): Promise<UploadOutcome> {
  const uploaded: SignageMedia[] = [];
  const refused: RefusedUpload[] = [];
  for (const file of files) {
    if (!file.type.startsWith('image') && !file.type.startsWith('video')) {
      refused.push({
        file,
        reason: `"${file.name}" is ${file.type || 'an unknown type'}, and the library takes images and videos`,
      });
      continue;
    }
    try {
      uploaded.push(await uploadOne(file));
    } catch (error) {
      refused.push({
        file,
        reason: error instanceof Error ? error.message : String(error),
      });
    }
  }
  return { uploaded, refused };
}

async function uploadOne(file: File): Promise<SignageMedia> {
  const written = await uploadBytes(file);
  let width: number | null = null;
  let height: number | null = null;
  let duration_ms: number | null = null;
  try {
    const probed = await getFileDimensions(file);
    width = probed?.width ?? null;
    height = probed?.height ?? null;
    duration_ms = probed?.duration_ms ?? null;
  } catch {
    width = null;
    height = null;
    duration_ms = null;
  }
  const media: SignageMedia = {
    id: mintBodyId(),
    name: file.name.replace(/\.[^/.]+$/, ''),
    source: 'stored',
    content: written.content,
    size: written.size,
    mime: file.type,
    duration_ms,
    width,
    height,
    catalog: null,
  };
  await rpc<MediaSavedReply>({ cmd: 'media_put', media });
  return media;
}

export async function uploadContent(files: File[]): Promise<SignageMedia[]> {
  const outcome = await uploadContentAll(files);
  return outcome.uploaded;
}

export async function fetchLibrary(): Promise<SignageMedia[]> {
  const reply = await rpc<LibraryReply>({ cmd: 'media_list' });
  return reply.media.map(normalizeMedia);
}

export async function fetchMedia(id: string): Promise<SignageMedia | null> {
  const reply = await rpc<MediaReply>({ cmd: 'media_get', media: id });
  return reply.media ? normalizeMedia(reply.media) : null;
}

export async function saveMedia(media: SignageMedia): Promise<void> {
  await rpc<MediaSavedReply>({ cmd: 'media_put', media });
}

/** Which programs play this entry — asked before a deletion is offered. */
export async function fetchMediaUsedBy(id: string): Promise<string[]> {
  const reply = await rpc<UsedByReply>({ cmd: 'media_used_by', media: id });
  return reply.programs;
}

export async function deleteMedia(id: string): Promise<void> {
  await rpc({ cmd: 'media_delete', media: id }, { confirm: true });
}
