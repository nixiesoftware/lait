/// <reference lib="webworker" />
/**
 * The Worker composition root — the half that owns the wasm.
 *
 * A dedicated Worker is mandatory, not a nicety: the engine's OPFS store uses
 * `FileSystemSyncAccessHandle`, which exists only in a Worker, and the runner
 * ABI is nested-synchronous over it. This entry loads the `porthole` module,
 * sources the device seed, fetches the 39 MiB World runner, stands the engine
 * up with `boot`, and hands the frame routing to `engineRouter`.
 *
 * Seed custody (finish-line #1): the tab MINTS its own device seed with
 * WebCrypto and KEEPS it in OPFS — never in the URL, never handed in by a
 * harness. A reload reuses the kept seed (a stable device); cleared site data
 * mints a new one (a new actor the invite must re-admit — the known caveat).
 *
 * The composition root posts one `boot` message, awaits `ready`, then binds
 * `workerLink(worker)` so no frame is posted before the router is listening.
 */

import init, { boot, found } from "porthole";

import { engineRouter } from "./engineRouter";

/** The one-shot stand-up message the composition root sends. */
interface BootMessage {
  type: "boot";
  relay: string;
  /** The invite ticket for a JOIN. Absent for a FOUND — a bare foundation-surface
   *  visit that mints a new Space in the tab instead of joining one. */
  ticket?: string;
  /** Where the engine wasm (porthole_bg.wasm, ~14 MiB) is served — same-origin
   *  in dev, the signed channel in a release. Only the small JS glue is
   *  bundled; the wasm itself is fetched and pinned, like the runner. */
  engineWasmUrl: string;
  /** Where the 39 MiB World runner wasm is served. Fetched as bytes, not linked. */
  runnerUrl: string;
  world: string;
  version: string;
  release: string;
  mount: string;
  /** The write gateway and public bucket base for daemon-less durability. Set
   *  only on a real foundation-relay join (see bootstrap.ts); absent for a dev
   *  join, which skips bucket sync entirely. */
  gatewayBase?: string;
  bucketBase?: string;
}

/** The engine methods the bucket-sync loop drives, past the boot handle's
 *  TypeScript surface (the wasm-bindgen glue is untyped here). */
interface BucketHandle {
  objectKey(): string;
  publishEnvelope(expectedGeneration: bigint): Uint8Array;
  absorbSnapshot(bytes: Uint8Array): boolean;
}

/**
 * Daemon-less durability, in the background: catch up to the bucket at boot,
 * then keep it current. Best-effort throughout — every failure degrades to
 * "try again next tick", never a surfaced error, because the live pull already
 * gave the tab a working Space and the bucket is the durability layer beneath
 * it, not the thing the tab depends on to function.
 *
 * The generation is never predicted: the first publish tries `0` (create), and
 * a `412` hands back the current generation to re-sign against — the
 * read-merge-write retry, exactly as two racing writers resolve it.
 */
function runBucketSync(handle: BucketHandle, gatewayBase: string, bucketBase: string): void {
  const key = handle.objectKey();
  const basename = key.replace(/^spaces\//, "");
  const getUrl = `${bucketBase}/${key}`;
  const putUrl = `${gatewayBase}/s/${basename}`;
  let lastGeneration = 0n;

  const bootstrap = async () => {
    try {
      const got = await fetch(getUrl, { cache: "no-store" });
      if (got.ok) {
        const advanced = handle.absorbSnapshot(new Uint8Array(await got.arrayBuffer()));
        console.log(`[lait] bucket bootstrap: ${advanced ? "caught up" : "already current"}`);
      }
    } catch (error) {
      console.warn("[lait] bucket bootstrap skipped:", error);
    }
  };

  const putEnvelope = async (generation: bigint): Promise<Response> => {
    // Copy into a plain ArrayBuffer: the wasm view is backed by the shared
    // engine memory, which is not an accepted fetch body.
    const envelope = handle.publishEnvelope(generation);
    const body = new Uint8Array(envelope).slice().buffer;
    return fetch(putUrl, {
      method: "PUT",
      body,
      headers: { "content-type": "application/octet-stream" },
    });
  };

  const publish = async () => {
    try {
      let response = await putEnvelope(lastGeneration);
      if (response.status === 412) {
        // The generation moved under us; re-sign against the one it reports.
        const current = BigInt((await response.json()).current);
        response = await putEnvelope(current);
      }
      if (response.ok) {
        lastGeneration = BigInt((await response.json()).generation);
      }
    } catch (error) {
      // Leave lastGeneration; the next tick retries against it or re-reads.
      console.warn("[lait] bucket publish deferred:", error);
    }
  };

  void bootstrap().then(publish);
  // A modest heartbeat: a write is durable within the interval without a
  // convergence hook to publish on. Tighten to publish-on-change later.
  setInterval(() => void publish(), 60_000);
}

const scope = self as unknown as DedicatedWorkerGlobalScope;

/** 32 bytes as 64 lowercase hex chars — the seed form `boot` takes. */
function toHex(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/**
 * The device seed, minted once and kept in OPFS. WebCrypto mints it; a
 * sync access handle in this Worker persists and reads it. A read failure
 * (private mode, cleared data) falls through to a fresh mint — a new device.
 */
async function deviceSeed(): Promise<string> {
  const root = await navigator.storage.getDirectory();
  const file = await root.getFileHandle("device.seed", { create: true });
  const handle = await file.createSyncAccessHandle();
  try {
    const size = handle.getSize();
    if (size === 32) {
      const buf = new Uint8Array(32);
      handle.read(buf, { at: 0 });
      return toHex(buf);
    }
    // No seed yet (or a corrupt length): mint one and keep it.
    const seed = new Uint8Array(32);
    crypto.getRandomValues(seed);
    handle.truncate(0);
    handle.write(seed, { at: 0 });
    handle.flush();
    return toHex(seed);
  } finally {
    handle.close();
  }
}

async function stand(message: BootMessage): Promise<void> {
  await init(message.engineWasmUrl);
  const seedHex = await deviceSeed();
  const runner = new Uint8Array(
    await (await fetch(message.runnerUrl)).arrayBuffer(),
  );
  // A ticket is a JOIN; its absence is a FOUND — mint a new Space in the tab.
  const handle = message.ticket
    ? await boot(
        message.relay,
        seedHex,
        message.ticket,
        runner,
        message.world,
        message.version,
        message.release,
        message.mount,
      )
    : await found(
        message.relay,
        seedHex,
        runner,
        message.world,
        message.version,
        message.release,
        message.mount,
      );
  engineRouter(handle, scope);
  scope.postMessage({ type: "ready" });

  // Daemon-less durability, only on a real foundation join (both set together).
  if (message.gatewayBase && message.bucketBase) {
    runBucketSync(
      handle as unknown as BucketHandle,
      message.gatewayBase,
      message.bucketBase,
    );
  }
}

// The boot handshake runs once; after it, `engineRouter` owns `message`s.
scope.addEventListener(
  "message",
  (event: MessageEvent) => {
    const data = event.data as { type?: string } | null;
    if (!data || data.type !== "boot") return;
    stand(data as BootMessage).catch((error: unknown) => {
      // Say it out loud as well as posting it: the host resolves and rejects
      // through the same `render` callback, so a swallowed rejection is the
      // difference between a diagnosable failure and a blank tab.
      console.error("[lait] the in-tab engine failed to boot:", error);
      scope.postMessage({ type: "boot-failed", error: String(error) });
    });
  },
  { once: true },
);
