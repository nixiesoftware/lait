/**
 * Types for the `porthole` wasm-pack module — the shippable in-tab engine
 * (`crates/porthole`). The built package lives outside `viewer/src` and is
 * gitignored, so tsc cannot see its generated `.d.ts`; this ambient declaration
 * lets `engine.worker.ts` typecheck, and a Vite alias resolves `"porthole"` to
 * `crates/porthole/pkg/porthole.js` at build time (the `?url` + manual-init
 * loading posture). It mirrors `porthole/pkg/porthole.d.ts` — keep it in step
 * with the `#[wasm_bindgen]` surface in `crates/porthole/src/handle.rs`.
 */
declare module "porthole" {
  /** The composed engine handle JS holds for the tab's life. */
  export class BrowserEngineHandle {
    handleLink(frameJson: string): string;
    handleSession(frameJson: string): string;
    watchCaret(questionJson: string): Promise<boolean>;
    publishCaret(issue: string, field: string, position: number): Promise<boolean>;
    drainRing(): string | undefined;
    drainCaret(): Promise<string | undefined>;
    repull(): Promise<number>;
    /** Capture the whole live Space (ledger + World) as a bucket-ready blob. */
    snapshot(): Uint8Array;
    /** Capture→cold-restore→decrypt round-trip proof (daemon-less hosting). */
    verify_snapshot_roundtrip(): string;
    free(): void;
  }

  /** Stand the whole engine up. `runner_wasm` is the 39 MiB World runner,
   *  fetched as bytes (not linked); the identity strings are inputs. */
  export function boot(
    relay: string,
    seed_hex: string,
    ticket: string,
    runner_wasm: Uint8Array,
    world: string,
    version: string,
    release: string,
    mount: string,
  ): Promise<BrowserEngineHandle>;

  /** Found a NEW Space in the tab — the daemon-less FOUNDING entry, `boot`
   *  minus the join ticket. Mints the Space, activates the World with its
   *  founder grants, and composes the same engine. */
  export function found(
    relay: string,
    seed_hex: string,
    runner_wasm: Uint8Array,
    world: string,
    version: string,
    release: string,
    mount: string,
  ): Promise<BrowserEngineHandle>;

  /** Recover a bare-visit founder whose local store `found` could not reopen
   *  (it rejected with `RESUME_INCOMPATIBLE`). Pass the durable snapshot fetched
   *  from the bucket to ADOPT it, or `undefined` to re-found. Clears the
   *  unreadable store, then composes the same engine `found` does. */
  export function recover(
    relay: string,
    seed_hex: string,
    snapshot: Uint8Array | undefined,
    runner_wasm: Uint8Array,
    world: string,
    version: string,
    release: string,
    mount: string,
  ): Promise<BrowserEngineHandle>;

  /** The bucket object key a bare-visit founder's Space publishes to, from the
   *  device seed alone — lets the Worker fetch the durable copy during recovery
   *  before any handle exists. */
  export function object_key_for_seed(seed_hex: string): string;

  /** Manual wasm init before `boot` (the `?url` posture, wasm-pack
   *  `--target web`). Modern wasm-bindgen takes a single options object —
   *  `await init({ module_or_path: wasmUrl })`; the bare-value form is
   *  deprecated and warns. */
  export default function init(
    options?:
      | { module_or_path: string | URL | Request | Response | WebAssembly.Module }
      | string
      | URL
      | Request
      | Response
      | WebAssembly.Module,
  ): Promise<unknown>;
}
