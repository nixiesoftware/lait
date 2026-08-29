/**
 * Tactile state, as far as the web will allow.
 *
 * Blink (Chrome/Edge/Android WebView) ships `navigator.vibrate`. Firefox
 * removed it. WebKit has never shipped it and objects to the API: it cannot
 * drive a Taptic Engine, and a richer actuator API is a fingerprinting
 * surface. Vibration also requires a user gesture, is silenced when the
 * document is hidden, and is a no-op on a desktop with no motor.
 *
 * Safari 18+ fires a system tick when an `<input type="checkbox" switch>`
 * is toggled through its label. That is the switch control's own haptic,
 * not a vibration API. We use it only for discrete events (lift, delete,
 * save), never for continuous snap-while-dragging.
 *
 * Treat every call as progressive enhancement. Visual state remains the
 * authority.
 */

export type HapticKind =
  | "select"
  | "lift"
  | "snap"
  | "edge"
  | "delete"
  | "save"
  | "error";

/** Vibration timelines: on-ms, off-ms, on-ms. Short = light, paired = notice. */
const PATTERNS: Record<HapticKind, number | number[]> = {
  select: 8,
  lift: 18,
  snap: 10,
  edge: 14,
  delete: [22, 36, 28],
  save: [12, 32, 18],
  error: [28, 40, 28],
};

const SNAP_GAP_MS = 48;
const last: Partial<Record<HapticKind, number>> = {};

function reduced(): boolean {
  return (
    typeof window !== "undefined" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches
  );
}

function hidden(): boolean {
  return typeof document !== "undefined" && document.visibilityState === "hidden";
}

function vibrate(pattern: number | number[]): void {
  if (typeof navigator === "undefined" || typeof navigator.vibrate !== "function") {
    return;
  }
  try {
    navigator.vibrate(pattern);
  } catch {
    /* desktop Chrome, iOS, and Firefox all refuse in different ways */
  }
}

let switchLabel: HTMLLabelElement | null = null;

function iosTick(): void {
  if (typeof document === "undefined") return;
  if (!switchLabel) {
    const host = document.createElement("div");
    host.className = "ds-haptic-host pe-haptic-host";
    host.setAttribute("aria-hidden", "true");
    const input = document.createElement("input");
    input.type = "checkbox";
    input.setAttribute("switch", "");
    input.id = "pe-haptic-switch";
    input.tabIndex = -1;
    const label = document.createElement("label");
    label.htmlFor = "pe-haptic-switch";
    host.append(input, label);
    document.body.append(host);
    switchLabel = label;
  }
  try {
    switchLabel.click();
  } catch {
    /* WebKit only plays this from a user gesture, through the label */
  }
}

/** Discrete events get an iOS switch tick. Dragging snaps stay on vibrate only. */
const IOS_TICK: ReadonlySet<HapticKind> = new Set([
  "select",
  "lift",
  "edge",
  "delete",
  "save",
  "error",
]);

export function haptic(kind: HapticKind): void {
  if (reduced() || hidden()) return;
  const now = performance.now();
  const minGap = kind === "snap" ? SNAP_GAP_MS : 0;
  if (minGap && now - (last[kind] ?? 0) < minGap) return;
  last[kind] = now;
  vibrate(PATTERNS[kind]);
  if (IOS_TICK.has(kind)) iosTick();
}
