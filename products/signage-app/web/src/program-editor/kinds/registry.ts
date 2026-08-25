/**
 * The kinds this build can put on a program.
 *
 * Membership here is the gate: a kind with no panel cannot be offered, which
 * is the fix for a clip that could be added and then neither configured nor
 * drawn. `youtube` and `html_widget` were exactly that — listed for adding,
 * blanked by the renderer, and configurable only from a page the nav no longer
 * reaches. They come back when they have a panel and the renderer draws them.
 */

import { athanPanel } from "./athan/schema";
import type { KindPanel, Settings } from "./types";

export const KIND_PANELS: KindPanel[] = [athanPanel];

export function panelFor(kind: string | undefined): KindPanel | null {
  if (!kind) return null;
  return KIND_PANELS.find((panel) => panel.kind === kind) ?? null;
}

export function isDrawable(kind: string | undefined): boolean {
  return panelFor(kind) != null;
}

/** Space config wins over whatever the library row was saved with. */
export function overlaySettings(
  space: Settings | null | undefined,
  entry: Settings,
): Settings {
  return space ? { ...entry, ...space } : { ...entry };
}

export type { Draft, Field, Group, KindPanel, Settings } from "./types";
