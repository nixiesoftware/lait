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

import init, { boot } from "porthole";

import { engineRouter } from "./engineRouter";

/** The one-shot stand-up message the composition root sends. */
interface BootMessage {
  type: "boot";
  relay: string;
  ticket: string;
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
  const handle = await boot(
    message.relay,
    seedHex,
    message.ticket,
    runner,
    message.world,
    message.version,
    message.release,
    message.mount,
  );
  engineRouter(handle, scope);
  scope.postMessage({ type: "ready" });
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
