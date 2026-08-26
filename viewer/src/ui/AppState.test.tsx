import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";

import {
  ApplicationState,
  classifyFailure,
  InlineError,
  LoadingState,
  recoveryDiagnostics,
  recoveryForError,
  SkeletonRows,
  trustSummary,
} from "./AppState";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

describe("application state vocabulary", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  it("announces loading as busy status without treating it as an error", () => {
    render(<LoadingState title="Loading issues" body="Reading local data." />);
    const state = host!.querySelector('[data-application-state="loading"]');
    expect(state?.getAttribute("role")).toBe("status");
    expect(state?.getAttribute("aria-busy")).toBe("true");
    expect(state?.textContent).toContain("Loading issues");
  });

  it("distinguishes filtered empty from ordinary empty", () => {
    render(<ApplicationState kind="filtered-empty" title="No matching issues" />);
    expect(host!.querySelector('[data-application-state="filtered-empty"]')).toBeTruthy();
  });

  it("uses an alert only for an error state", () => {
    render(<ApplicationState kind="error" title="Could not load" />);
    expect(host!.querySelector('[role="alert"]')?.textContent).toContain("Could not load");
  });

  it("gives retry states an explicit recoverable identity", () => {
    render(<ApplicationState kind="retry" title="Connection interrupted" action={<button>Try again</button>} />);
    expect(host!.querySelector('[data-application-state="retry"] button')?.textContent).toBe("Try again");
  });

  it("offers contextual recovery, diagnostics, and dismissal", () => {
    let retried = 0;
    let copied = 0;
    let dismissed = 0;
    render(
      <InlineError
        title="Local service unavailable"
        message="Network request failed"
        retryLabel="Reconnect"
        onRetry={() => retried++}
        onCopy={() => copied++}
        onDismiss={() => dismissed++}
      />,
    );

    const buttons = [...host!.querySelectorAll("button")];
    act(() => buttons.find((button) => button.textContent?.includes("Reconnect"))?.click());
    act(() => buttons.find((button) => button.textContent?.includes("Copy details"))?.click());
    act(() => buttons.find((button) => button.getAttribute("aria-label") === "Dismiss error")?.click());
    expect([retried, copied, dismissed]).toEqual([1, 1, 1]);
  });

  it("classifies connection and authorization recovery into useful actions", () => {
    expect(recoveryForError("Failed to fetch daemon status")).toEqual({
      title: "Local service unavailable",
      retryLabel: "Reconnect",
    });
    expect(recoveryForError("Unauthorized: space is read-only")).toEqual({
      title: "Read-only space",
      retryLabel: "Refresh",
    });
    expect(recoveryForError("Unexpected projection failure")).toEqual({
      title: "Something didn’t finish",
      retryLabel: "Retry",
    });
  });

  it("classifies the complete viewer failure vocabulary", () => {
    expect(classifyFailure("network offline")).toBe("offline");
    expect(classifyFailure("schema version incompatible")).toBe("incompatible");
    expect(classifyFailure("unauthorized")).toBe("authorization");
    expect(classifyFailure("agent is read-only")).toBe("read-only");
    expect(classifyFailure("unknown issue reference")).toBe("invalid-reference");
    expect(classifyFailure("stale expected revision")).toBe("stale");
    expect(classifyFailure("ambiguous: multiple matches")).toBe("ambiguity");
    expect(classifyFailure("concurrent conflict")).toBe("conflict");
    expect(classifyFailure("provisional body still arriving")).toBe("provisional");
    expect(classifyFailure("corrupt undecodable record")).toBe("corrupt");
    expect(classifyFailure("validation rejected")).toBe("rejected");
    expect(classifyFailure("queued pending synchronization")).toBe("pending-sync");
  });

  // The engine's denial messages, verbatim — every one must classify as
  // authorization (a title of "Change not allowed", never the generic
  // "Something didn’t finish" + Retry that made a standing denial look like a
  // transient fault). These are the regex fallback; a message that arrived
  // through the API layer is classified by its `error_kind` tag first.
  it("classifies the engine's denial wording as authorization", () => {
    expect(classifyFailure(
      "this device isn't recognized as a member of this space at its current "
      + "local view — if you were just invited or promoted, that change may not "
      + "have synced to this node yet (run sync and retry); otherwise ask an "
      + "admin to admit or re-admit you; nothing was changed",
    )).toBe("authorization");
    expect(classifyFailure(
      "you don't hold the capability this change demands — a view-only member "
      + "needs an admin to grant write access, a sponsored agent needs its human "
      + "sponsor to grant it, and a scoped member may be writing outside the "
      + "projects their grant covers; nothing was changed",
    )).toBe("authorization");
    expect(classifyFailure(
      "you can't read this — your grants don't cover this query's scope; an "
      + "admin can widen them",
    )).toBe("authorization");
    // The old collapsed message, still emitted by not-yet-upgraded daemons.
    expect(classifyFailure(
      "you lack write standing in this space — a sponsored agent needs a human "
      + "member to grant it write access, and a view-only member needs an admin "
      + "to grant it; nothing was changed",
    )).toBe("authorization");
    expect(recoveryForError("you lack write standing in this space")).toEqual({
      title: "Change not allowed",
      retryLabel: "Refresh",
    });
  });

  it("keeps a ledger evaluation failure out of the permissions vocabulary", () => {
    const message =
      "this node could not evaluate authority state (MissingHistory) — a local "
      + "ledger problem, not a permissions problem; run sync (or doctor) and "
      + "retry; nothing was changed";
    expect(classifyFailure(message)).toBe("authority-unavailable");
    expect(recoveryForError(message).title).toBe("Authority state unavailable");
  });

  it("lets the engine's error_kind tag outrank every message regex", async () => {
    const { LaitError } = await import("../api");
    // Wording that matches no denial regex still classifies as authorization
    // once a LaitError has carried its `error_kind` through the API layer.
    const worded = "completely novel refusal wording with zero keyword overlap";
    void new LaitError(worded, 403, "denied");
    expect(classifyFailure(worded)).toBe("authorization");
  });

  function render(node: React.ReactNode) {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    act(() => root?.render(node));
  }
});

describe("local trust summary", () => {
  it("makes locally safe offline data explicit", () => {
    expect(trustSummary("retrying", true, 0, false)).toBe("Offline · local data safe");
  });

  it("prioritizes degraded recovery over connectivity", () => {
    expect(trustSummary("live", true, 2, true)).toBe("Recovery needs attention");
  });

  it("reports reachability without claiming convergence", () => {
    expect(trustSummary("live", true, 2, false)).toBe("2 peers");
  });

  it("produces copyable recovery detail without inventing repair success", () => {
    expect(recoveryDiagnostics({
      id: "local",
      nick: "me",
      name: "Viewer",
      online_peers: 0,
      space: "ws_viewer",
      items: 89,
      scopes: 13,
      membership: "admin",
      recovery: {
        authority: null,
        configuration: Array(32).fill(0),
        generation: 1,
        custody: { state: "unreadable", detail: { kind: "wrong_protector" } },
        backing: { holders: [], satisfies_configuration: false },
        availability: { state: "unknown" },
      },
      degraded_recovery: [{
        transcript: "transcript-1",
        reason: { kind: "wrong_protector" },
        is_current_authority: true,
      }],
    })).toContain("Failure: wrong_protector");
  });
});

describe("the list that is coming", () => {
  /**
   * Eight grey bars are for the eye. A screen reader gets one honest sentence
   * instead — announcing each placeholder individually is worse than silence,
   * and silence would be worse than saying something is on its way.
   */
  it("stands in for rows visually and says it once for a reader", () => {
    const host = document.createElement("div");
    document.body.appendChild(host);
    const root = createRoot(host);
    act(() => {
      root.render(<SkeletonRows rows={5} label="Loading issues" />);
    });

    const status = host.querySelector('[role="status"]');
    expect(status?.textContent).toBe("Loading issues");
    const hidden = host.querySelector("[aria-hidden]");
    expect(hidden?.children.length).toBe(5);
    expect(host.querySelector("[data-application-state]")?.getAttribute("aria-busy")).toBe("true");
    root.unmount();
    host.remove();
  });
});
