/**
 * The composition root's topology choice — head vs in-tab engine.
 *
 * `bindEngineLink` must be called once, before render. When the URL carries a
 * join link (an invite ticket + a relay, in the fragment — never a seed, which
 * the tab mints and keeps in OPFS), this spawns the engine Worker, boots it,
 * and binds `workerLink` to it: the daemon-less, in-tab path. Otherwise it
 * leaves `httpLink` bound: the ordinary head topology talking to a local daemon.
 *
 * The decision is the URL, never a dev flag (per `link.ts`). The Worker is
 * spawned here because only the root that spawns it can own its lifecycle
 * (`workerLink` deliberately does not supervise it).
 */

import { bindEngineLink } from "../link";
import { workerLink } from "../workerLink";

/** A join link's public parts. The seed is never here. */
export interface JoinParams {
  ticket: string;
  relay: string;
}

/**
 * Parse a join link from a URL fragment: `#join=<ticket>&relay=<url>`. Returns
 * `null` for an ordinary load (no ticket), which keeps the head topology.
 */
export function parseJoin(hash: string): JoinParams | null {
  const fragment = hash.startsWith("#") ? hash.slice(1) : hash;
  const params = new URLSearchParams(fragment);
  const ticket = params.get("join");
  const relay = params.get("relay");
  if (!ticket || !relay) return null;
  return { ticket, relay };
}

/**
 * The Issues release identity the runner is told, and where its wasm is served.
 * These are the product's own facts (finish-line #7 sources them from the
 * Library row in the full client; the viewer's own release build knows them).
 */
const ISSUES = {
  world: "com.lait.issues",
  version: "0.9.5",
  release: "release",
  mount: "issues",
  /** Same-origin in dev and in the viewer's own release bundle; the signed
   *  channel URL when a client fetches a pinned release. Bytes, never linked. */
  runnerUrl: "/lait_issues_runner.wasm",
};

/**
 * Choose the backend for this load. Resolves once the engine link is bound —
 * immediately for head topology, or after the Worker reports `ready` for the
 * in-tab path — so a caller can await it before render and avoid a rebind flash.
 */
export function bootstrapEngine(loc: Location = self.location): Promise<void> {
  const join = parseJoin(loc.hash);
  if (!join) return Promise.resolve(); // head topology: httpLink stays bound.

  const worker = new Worker(new URL("./engine.worker.ts", import.meta.url), {
    type: "module",
  });
  return new Promise<void>((resolve, reject) => {
    worker.addEventListener("message", (event: MessageEvent) => {
      const data = event.data as { type?: string; error?: string } | null;
      if (data?.type === "ready") {
        bindEngineLink(workerLink(worker));
        resolve();
      } else if (data?.type === "boot-failed") {
        reject(new Error(data.error ?? "the in-tab engine failed to boot"));
      }
    });
    worker.postMessage({
      type: "boot",
      relay: join.relay,
      ticket: join.ticket,
      ...ISSUES,
    });
  });
}
