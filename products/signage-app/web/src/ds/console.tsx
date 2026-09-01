/**
 * Console: the screen and its chin, one body.
 *
 * A monitor is one object: the glass, and a chin along the bottom holding
 * its controls. The body's padding leaves room for the rim a never-heard
 * or on-air panel wears; pressing a chin key blooms its panel upward over
 * the glass, Dynamic-Island fashion, from the chin that owns it.
 *
 * Containment is composition: the console draws what it is made of, so the
 * caller places the glass (a `Bezel`) and the chin (`ScreenChin`) inside it
 * rather than passing them as configuration.
 */

import type { ReactNode } from "react";

export function Console({ children }: { children: ReactNode }) {
  return <div className="ds-console">{children}</div>;
}
