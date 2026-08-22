/**
 * The wire omits what is empty (`skip_serializing_if`); the app's model
 * promises these fields exist. The promise is kept here, once, at the
 * boundary — never with `?? []` scattered through components.
 */

import type { SignageMedia, SignageScreen } from './types';

export function normalizeScreen(screen: SignageScreen): SignageScreen {
  return {
    ...screen,
    group: screen.group ?? null,
    intent: screen.intent ?? {},
    schedule: screen.schedule ?? [],
  };
}

export function normalizeMedia(media: SignageMedia): SignageMedia {
  return {
    ...media,
    duration_ms: media.duration_ms ?? null,
    width: media.width ?? null,
    height: media.height ?? null,
  };
}
