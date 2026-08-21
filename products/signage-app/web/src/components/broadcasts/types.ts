import type { SignageItem, SignageMedia } from '@/utils/lait/types';

export type { SignageItem, SignageMedia };

/** Fallback when neither the item nor its library entry states a duration. */
export const DEFAULT_ITEM_SECONDS = 10;

/**
 * The editor's working row: a program item joined to the library entry it
 * names. The item is the wire shape — order lives in the array, never on
 * the row.
 */
export interface BroadcastRow {
  item: SignageItem;
  media: SignageMedia;
}

/**
 * The seconds the timeline draws for a row: the item's own duration when
 * set, else the library entry's default, else DEFAULT_ITEM_SECONDS.
 */
export function rowDurationSeconds(row: BroadcastRow): number {
  const ms = row.item.duration_ms ?? row.media.duration_ms;
  return (ms ?? DEFAULT_ITEM_SECONDS * 1000) / 1000;
}

/**
 * A display discriminator for the kind helpers: the mime for stored bytes
 * (`image/png`), the kind name for integrations (`athan`), the source tag
 * otherwise.
 */
export function mediaKind(media: SignageMedia): string {
  switch (media.source) {
    case 'stored':
      return media.mime;
    case 'kind':
      return media.kind;
    default:
      return media.source;
  }
}

/** The short label a placeholder tile chips next to the name. */
export function mediaChip(media: SignageMedia): string {
  switch (media.source) {
    case 'stored':
      return media.mime.split('/')[1]?.toUpperCase() ?? media.mime.toUpperCase();
    case 'kind':
      return media.kind.toUpperCase();
    default:
      return media.source.toUpperCase();
  }
}
