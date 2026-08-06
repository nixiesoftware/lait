import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import { BulkBar } from "./BulkBar";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

describe("BulkBar", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  it("reports partial failure and targets retry without hiding successful work", () => {
    const retry = vi.fn();
    render({
      done: 3,
      total: 3,
      pending: false,
      successes: ["VIEW-1", "VIEW-2"],
      failures: [{ reff: "VIEW-3", label: "VIEW-3", message: "Read-only" }],
    }, retry);

    expect(host!.textContent).toContain("2 succeeded · 1 failed");
    // Not `[role="status"]` any more: Astryx's buttons publish their own live
    // regions, so the bar now has three and document order is theirs to change.
    const status = host!.querySelector("[data-bulk-progress]");
    expect(status?.getAttribute("title")).toContain("VIEW-3: Read-only");
    act(() => [...host!.querySelectorAll("button")]
      .find((button) => button.textContent?.includes("Retry failed"))?.click());
    expect(retry).toHaveBeenCalledOnce();
  });

  it("disables mutation controls while work is pending", () => {
    render({
      done: 1,
      total: 3,
      pending: true,
      successes: [],
      failures: [],
    }, vi.fn());

    expect(host!.textContent).toContain("1/3 complete");
    // `aria-disabled`, not `disabled`. Astryx uses the ARIA form whenever the
    // control carries a tooltip, so the button stays focusable and can still
    // explain why it is unavailable — the opposite of what a bare `disabled`
    // gives a keyboard user.
    const del = host!.querySelector('button[aria-label="Delete selected"]');
    expect(del?.getAttribute("aria-disabled")).toBe("true");
  });

  function render(progress: React.ComponentProps<typeof BulkBar>["progress"], retry: () => void) {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    act(() =>
      root?.render(
          <BulkBar
            count={3}
            progress={progress}
            states={[]}
            labels={[]}
            members={[]}
            onStatus={() => undefined}
            onPriority={() => undefined}
            onLabel={() => undefined}
            onAssign={() => undefined}
            onDue={() => undefined}
            onDelete={() => undefined}
            onRetryFailures={retry}
            onClear={() => undefined}
          />
      ),
    );
  }
});
