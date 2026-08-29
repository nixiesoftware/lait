import { act, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import { MyIssues } from "./MyIssues";

const rpcMock = vi.hoisted(() => vi.fn());
vi.mock("../api", () => ({ rpc: rpcMock, spaceRpc: vi.fn() }));

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

/**
 * A read that failed is not a read still going.
 *
 * These surfaces hold their own `useState<T | null>(null)`, where null means
 * "not loaded". A failed read left it null and returned, so the surface drew
 * its loading state forever — a spinner promising progress that was never
 * coming — while the reason went to a toast that had already faded. It is the
 * same defect as the board's, wearing the other mask: there, a first run was
 * called a failure; here, a failure is called a first run.
 */
describe("a surface whose first read failed", () => {
  const hosts: HTMLElement[] = [];
  afterEach(() => {
    for (const host of hosts.splice(0)) host.remove();
    rpcMock.mockReset();
  });

  it("says so, and offers the retry, instead of loading forever", async () => {
    rpcMock.mockRejectedValue(new Error("the daemon went away"));
    const host = document.createElement("div");
    document.body.appendChild(host);
    hosts.push(host);

    await act(async () => {
      createRoot(host).render(
        <StrictMode>
          <MyIssues spaceId="orb_x" revision={0} onOpen={() => {}} onError={() => {}} />
        </StrictMode>,
      );
    });
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });

    const state = host.querySelector("[data-application-state]");
    expect(state?.getAttribute("data-application-state")).toBe("retry");
    expect(host.textContent).toContain("the daemon went away");
    expect(host.textContent).toContain("Retry");
    expect(host.textContent).not.toContain("Loading your issues");
  });
});
