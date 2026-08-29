import type { MediaSource, SignageMedia } from '@/utils/lait/types';

/** A library entry as the content surfaces render it, plus upload-transient state. */
export type ContentItemProps = SignageMedia & {
  isUploading?: boolean;
  tempId?: string;
};

export type SourceCategory = 'image' | 'video' | 'stored' | 'card' | 'kind' | 'live';

/** The client-side filter bucket: stored entries split image/video by mime. */
export function sourceCategory(source: MediaSource): SourceCategory {
  if (source.source === 'stored') {
    if (source.mime.startsWith('image')) return 'image';
    if (source.mime.startsWith('video')) return 'video';
    return 'stored';
  }
  return source.source;
}

/** Short chip text: the mime subtype for stored bytes, the tag otherwise. */
export function sourceLabel(source: MediaSource): string {
  switch (source.source) {
    case 'stored':
      return (source.mime.split('/')[1] || source.mime).toUpperCase();
    case 'card':
      return 'CARD';
    case 'kind':
      return source.kind.toUpperCase();
    case 'live':
      return 'LIVE';
  }
}
