/**
 * The operational bar's rules — persistent operational truth, chosen, not
 * stacked. A refusal must remain visible, but three copies of the same
 * successful open are not three useful rows of UI. The core remains the
 * authority; these functions only choose the most important sentence from
 * the immutable view it sent.
 */
import { actionKey, type ClientView, type Notice, type WorldPerson } from "./client";

export interface IdentityStatus {
  label: string;
  tone: "neutral" | "error" | "warn" | "ok";
}

export function identityStatus(view: ClientView): IdentityStatus {
  if (view.loading) return { label: "Connecting to local identity", tone: "neutral" };
  if (view.failures.length > 0) return { label: "Needs attention", tone: "error" };
  if (view.stale !== null || view.devices.some((device) => device.degraded !== null)) {
    return { label: "Local identity degraded", tone: "warn" };
  }
  if (view.host === null) return { label: "Local identity unavailable", tone: "warn" };
  return { label: "Local identity online", tone: "ok" };
}

export function activityLine(view: ClientView): string {
  if (view.inFlight.length > 0) return describeAction(view.inFlight[0]);
  if (view.failures.length > 0) {
    const failure = view.failures[0];
    return `${failure.what}: ${failure.error}`;
  }
  // A record may be repeated by older cores. The bar is a current summary, so
  // identical sentences collapse here instead of taking over the window.
  const notices = [...new Set(view.notices.map(noticeSummary))];
  if (notices.length > 0) return notices[0];
  return "All local systems current";
}

/**
 * Launch tickets are single-use credentials. The core records the launch so a
 * person can tell that their click worked, but chrome only needs the safe
 * destination — never its query or fragment.
 */
export function noticeSummary(notice: Notice): string {
  const launched = notice.launched;
  if (launched === null) return notice.said;
  let uri: URL;
  try {
    uri = new URL(launched);
  } catch {
    return "Opened World in browser";
  }
  if (uri.host === "") return "Opened World in browser";
  return `Opened ${uri.protocol}//${uri.host}${uri.pathname}`;
}

function describeAction(key: string): string {
  if (key === actionKey.refresh) return "Reading local state…";
  if (key.startsWith("open:")) return "Starting World…";
  if (key.startsWith("head.")) return "Updating head…";
  if (key.startsWith("device.")) return "Updating device…";
  if (key.startsWith("space.")) return "Reading Space…";
  return "Working…";
}

/**
 * The people glance's two tiers of liveness: full rows for people IN the
 * World right now — a launched World is the nearest presence there is — and
 * bare faces for everyone else the book addresses here, ordered by reach
 * with the measured absence last.
 */
export function glanceTiers(people: WorldPerson[]): { here: WorldPerson[]; holding: WorldPerson[] } {
  const absent = people.filter((person) => !person.here);
  return {
    here: people.filter((person) => person.here),
    holding: [
      ...absent.filter((person) => person.presence === "online"),
      ...absent.filter((person) => person.presence === "away"),
      ...absent.filter((person) => person.presence === null),
      ...absent.filter((person) => person.presence === "offline"),
    ],
  };
}
