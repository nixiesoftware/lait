import { act, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";

import { useWorldResource, WorldViewStoreProvider } from "./worldViewReact";
import { WorldViewStore } from "./worldViewStore";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

/**
 * A resource that failed once is asked again; one that keeps failing is not
 * asked forever.
 *
 * `ensure` leaves a rejected entry stale with no promise in flight — ready to
 * be retried, and nothing retried it. The loading effect depends on the key,
 * the loader and the store, none of which change because a request failed, so a
 * resource that lost one race at startup stayed lost for as long as the view
 * did. That is what drew "the local projection could not be loaded" over a
 * daemon that had come up a moment later.
 */
describe("a resource that failed", () => {
  const hosts: HTMLElement[] = [];

  afterEach(() => {
    for (const host of hosts.splice(0)) host.remove();
  });

  /** Mount a probe that reports every snapshot the hook produced. */
  async function observe(store: WorldViewStore, loader: () => Promise<string>) {
    const seen: string[] = [];
    function Probe() {
      const snapshot = useWorldResource<string>("board", loader);
      seen.push(snapshot.state);
      return null;
    }
    const host = document.createElement("div");
    document.body.appendChild(host);
    hosts.push(host);
    const root = createRoot(host);
    await act(async () => {
      root.render(
        <StrictMode>
          <WorldViewStoreProvider store={store}>
            <Probe />
          </WorldViewStoreProvider>
        </StrictMode>,
      );
    });
    // Let the rejection settle and any retry run.
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 20));
    });
    return seen;
  }

  it("is asked again, and reaches ready when the second attempt succeeds", async () => {
    const store = new WorldViewStore();
    const loader = vi
      .fn<() => Promise<string>>()
      .mockRejectedValueOnce(new Error("the daemon was not up yet"))
      .mockResolvedValue("the board");

    const seen = await observe(store, loader);

    expect(loader.mock.calls.length).toBeGreaterThan(1);
    expect(seen).toContain("ready");
    expect(store.read<string>("board").data).toBe("the board");
  });

  /**
   * The loop this must not become. Every rejection publishes a *new* error, so
   * retrying on the error itself fires again the instant the retry fails —
   * hammering a daemon that is already struggling, which is worse than the
   * defect it fixes.
   */
  it("is not asked forever when it keeps failing", async () => {
    const store = new WorldViewStore();
    const loader = vi.fn<() => Promise<string>>().mockRejectedValue(new Error("still not up"));

    await observe(store, loader);

    expect(store.read<string>("board").state).toBe("error");
    expect(loader.mock.calls.length).toBeLessThanOrEqual(4);
  });
});
