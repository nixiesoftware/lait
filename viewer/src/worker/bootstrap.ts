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
 * The foundation's public relay — the default rendezvous when a shared join link
 * carries no explicit relay. A bare `foundation.pub/i#join=<ticket>` keeps the
 * admission capability in the fragment (never sent to the server) and finds the
 * inviter through this relay. Mirrors `FOUNDATION_RELAY` in `src/config.rs`; a
 * local/dev link overrides it with its own `&relay=`.
 */
export const FOUNDATION_RELAY = "https://relay.foundation.pub";

/**
 * The shared "Public" board's reusable invite — the one admission capability
 * every visitor carries. A bare `foundation.pub/i` visit redeems this to JOIN
 * the single Public Space the foundation daemon founds and holds, so everyone
 * lands in the same board rather than founding an isolated Space of their own.
 * From there a person founds their own Spaces; Public stays the permanent
 * fixture they can always return to from the Space picker.
 *
 * Empty until the Public daemon is stood up and mints it: while empty, a bare
 * visit falls back to founding in-tab, so `/i` works before Public exists. The
 * value is a reusable ticket, not a seed — it admits, it does not identify, and
 * it is safe to ship in the static bundle (that is the whole point of "everyone
 * carries the invite"). Mint it once, against the daemon's Public Space.
 */
export const PUBLIC_INVITE = "";

/**
 * The daemon-less durability plane: the write gateway a tab publishes its Space
 * snapshot through, and the public bucket base it reads one back from. Wired
 * ONLY for a foundation-relay join (a real production join); a dev/local join
 * carries its own `&relay=` and gets no bucket sync, so an e2e stack never
 * publishes its throwaway Space to the production bucket. The gateway path is
 * `PUT <gateway>/s/<capability>` and the read is `GET <bucket>/spaces/<cap>`.
 */
export const FOUNDATION_GATEWAY =
  "https://foundation-snapshot-gateway-894246603476.us-central1.run.app";
export const FOUNDATION_SNAPSHOTS =
  "https://storage.googleapis.com/the-foundation-snapshots";

/**
 * Parse a join link from a URL fragment: `#join=<ticket>` (with an optional
 * `&relay=<url>`). Returns `null` for an ordinary load (no ticket), which keeps
 * the head topology. When the ticket is present but no relay is given, the
 * foundation relay is the default — so a shared `foundation.pub/i#join=<ticket>`
 * is a complete join; only the ticket is ever required.
 */
export function parseJoin(hash: string): JoinParams | null {
  const fragment = hash.startsWith("#") ? hash.slice(1) : hash;
  const params = new URLSearchParams(fragment);
  const ticket = params.get("join");
  if (!ticket) return null;
  const relay = params.get("relay") ?? FOUNDATION_RELAY;
  return { ticket, relay };
}

/**
 * The Issues release identity the runner is told, and where its wasm is served.
 * These are the product's own facts (finish-line #7 sources them from the
 * Library row in the full client; the viewer's own release build knows them).
 */
const ISSUES = {
  world: "com.lait.issues",
  version: "0.9.6",
  release: "release",
  mount: "issues",
  /** The engine wasm (~14 MiB) — fetched, not bundled (only its small JS glue
   *  is). Resolved against the build base (`import.meta.env.BASE_URL`, always a
   *  trailing-slashed prefix), so a bundle served under a path prefix — the
   *  `foundation.pub/i` join surface, built `--base=/i/` — fetches
   *  `/i/porthole_bg.wasm`, not the apex root. Same-origin either way. */
  engineWasmUrl: `${import.meta.env.BASE_URL}porthole_bg.wasm`,
  /** The 39 MiB World runner, fetched as bytes. Base-relative, as above. */
  runnerUrl: `${import.meta.env.BASE_URL}lait_issues_runner.wasm`,
};

/**
 * Choose the backend for this load. Resolves once the engine link is bound —
 * immediately for head topology, or after the Worker reports `ready` for the
 * in-tab path — so a caller can await it before render and avoid a rebind flash.
 */
export function bootstrapEngine(loc: Location = self.location): Promise<void> {
  const join = parseJoin(loc.hash);

  // The foundation join surface is the bundle served under `/i/` (built
  // `--base=/i/`). There a bare visit — no ticket — is not head topology (there
  // is no daemon head to talk to); it is a FOUND, minting a new Space in the
  // tab. The local app (BASE_URL `/`, served by its daemon) keeps head topology
  // on a bare load. This is the one signal that separates "static join surface"
  // from "the app the daemon serves".
  const foundationSurface = import.meta.env.BASE_URL === "/i/";
  if (!join && !foundationSurface) return Promise.resolve(); // local app head.

  // A bare visit to the foundation surface joins the shared "Public" board via
  // its bundled reusable invite, so every visitor converges in the SAME Space.
  // An explicit `#join=` link wins (a person entering some other Space). Until
  // Public exists (PUBLIC_INVITE empty) a bare visit still founds in-tab, so the
  // surface works before the board is stood up.
  const effectiveJoin: JoinParams | null =
    join ??
    (foundationSurface && PUBLIC_INVITE
      ? { ticket: PUBLIC_INVITE, relay: FOUNDATION_RELAY }
      : null);

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
    // A join (explicit link or the Public invite) uses its relay; a bare-visit
    // found uses the foundation relay so the founder is reachable there. Bucket
    // durability rides both, but only against the foundation relay — a dev join
    // (its own &relay=) never publishes its throwaway Space to the production
    // bucket.
    const relay = effectiveJoin?.relay ?? FOUNDATION_RELAY;
    const bucket =
      relay === FOUNDATION_RELAY
        ? { gatewayBase: FOUNDATION_GATEWAY, bucketBase: FOUNDATION_SNAPSHOTS }
        : {};
    worker.postMessage({
      type: "boot",
      relay,
      // Absent ticket ⇒ the Worker founds instead of joining.
      ...(effectiveJoin ? { ticket: effectiveJoin.ticket } : {}),
      ...ISSUES,
      ...bucket,
    });
  });
}
