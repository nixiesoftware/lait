/**
 * Where the window's own controls sit over this page — and nowhere else in the
 * app is allowed to guess.
 *
 * A World's page is normally a page: it starts below whatever chrome the
 * window put above it, and it never has to think about the frame. Astrolabe
 * can open this World with that chrome made transparent instead, so the app's
 * own surface runs to the top edge of the window and the close/minimise/zoom
 * buttons sit *in* it rather than on a strip above it. That is the whole point
 * — one surface, not an application bolted under a skeleton — but it costs the
 * page the one thing the frame used to guarantee: that its top-left corner was
 * its own.
 *
 * The document cannot find out on its own. There is no media query for
 * "the window controls overlap you", `navigator.windowControlsOverlay` is a
 * PWA API this page is not, and sniffing the platform answers a different
 * question — macOS in a browser tab has no controls over the page at all. So
 * the host states it, before the document parses, as
 *
 *     window.__LAIT_WINDOW_CONTROLS__ = { top: 28, leading: 78 }
 *
 * and its absence is the ordinary case: a browser tab, a Linux window, any
 * World the host did not hand its rail to.
 *
 * The numbers are raw CSS pixels and deliberately outside `--scale`. They
 * describe buttons the operating system drew at a size of its own choosing;
 * scaling them with the app's density would move the app's content off the
 * controls it is supposed to be clearing.
 *
 * It can stop being true. Full screen takes the controls off the page, and the
 * host says so by writing the same global again and dispatching
 * `lait:window-controls` — the fact restated, not a second channel with its own
 * shape. Nothing here ever answers back; the host talks to this page and the
 * page has no way to reach the host at all.
 */
export type WindowControls = {
  /** Height of the band the controls sit in, from the top of the page. */
  top: number;
  /**
   * Width to keep clear at the leading edge of that band.
   *
   * Part of the host's declaration and validated with the rest of it, but this
   * shell spends no CSS on it: it clears the whole band with `top`, so nothing
   * it draws is ever beside the controls. A surface that *does* draw inside the
   * band — a World putting tabs or a title up there — needs this, and reaches
   * it through [`declaredControls`]. Publishing it as a custom property nobody
   * reads would be a claim the stylesheet does not keep.
   */
  leading: number;
};

/** The largest inset we will take on a host's word, in CSS pixels. */
const CEILING = 200;

/**
 * Read the host's declaration, or `null` when there is none to read.
 *
 * Everything here is a guard against a malformed declaration rather than a
 * hostile one: this value arrives from the host that opened the window, not
 * from the page. A bad number is worth refusing anyway, because the failure it
 * produces — a shell inset by `NaN`, or by a thousand pixels — reads as a
 * broken layout and names nothing about where it came from.
 */
export function declaredControls(scope: unknown = globalThis): WindowControls | null {
  const declared = (scope as { __LAIT_WINDOW_CONTROLS__?: unknown } | null | undefined)
    ?.__LAIT_WINDOW_CONTROLS__;
  if (typeof declared !== "object" || declared === null) return null;
  const { top, leading } = declared as { top?: unknown; leading?: unknown };
  const inset = (value: unknown) =>
    typeof value === "number" && Number.isFinite(value) && value >= 0 && value <= CEILING
      ? value
      : null;
  const t = inset(top);
  const l = inset(leading);
  if (t === null || l === null) return null;
  // A declaration of zero is a host saying its controls are not over the page,
  // which is what no declaration already means. Keep one representation of it.
  if (t === 0 && l === 0) return null;
  return { top: t, leading: l };
}

/** The host's signal that it has rewritten the global. */
export const RESTATED = "lait:window-controls";

/**
 * Apply the declaration now, and again whenever the host restates it.
 *
 * Returns the way to stop listening. The app never does — the document outlives
 * every surface in it — but a test that installs this on a fake window must be
 * able to leave the fake window clean.
 */
export function trackWindowControls(
  root: { style: { setProperty(name: string, value: string): void } },
  scope: {
    addEventListener(type: string, listener: () => void): void;
    removeEventListener(type: string, listener: () => void): void;
  } = window,
): () => void {
  const apply = () => applyWindowControls(root, declaredControls(scope));
  apply();
  scope.addEventListener(RESTATED, apply);
  return () => scope.removeEventListener(RESTATED, apply);
}

/**
 * Publish the band's height as a custom property, so the shell can spend it in
 * CSS and every surface that does not care keeps reading a `0px` it never has
 * to branch on.
 *
 * Only `top`. The shell clears the entire band, so nothing it draws is beside
 * the controls and `leading` has nothing to inset — and a custom property no
 * stylesheet reads is a value that drifts out of true without anything failing.
 * It stays available on the declaration itself for a surface that draws in the
 * band and therefore has a use for it.
 */
export function applyWindowControls(
  root: { style: { setProperty(name: string, value: string): void } },
  controls: WindowControls | null,
): void {
  root.style.setProperty("--window-controls-top", `${controls?.top ?? 0}px`);
}
