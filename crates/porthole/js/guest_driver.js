// Drives a World runner guest module from the engine module, in a browser,
// where the "host" that runs the guest wasm is the browser's own WebAssembly
// rather than wasmtime.
//
// The guest speaks the four-function ABI (wr_alloc/wr_free/wr_init/wr_handle
// exports, a `lait.host_call` import). This glue copies postcard frames across
// the two modules' separate linear memories and resolves `host_call` by
// synchronously calling the engine-side callback back. Everything is
// synchronous by the ABI's contract, which is legal here because the whole
// chain — including the ledger's OPFS reads — runs in a Worker on synchronous
// access handles.

const guests = new Map();
let nextId = 1;

// Compiled modules, cached so N instances of the one runner share a single
// (expensive) WebAssembly.Module compile.
const modules = new Map();
let nextModuleId = 1;

// Re-fetch the memory view every time: a `wr_alloc` may have grown linear
// memory, which detaches any prior ArrayBuffer view.
function bytes(guest) {
  return new Uint8Array(guest.exports.memory.buffer);
}

function pack(ptr, len) {
  return (BigInt(ptr >>> 0) << 32n) | BigInt(len >>> 0);
}

// Compile the runner module ONCE. Returns a module id an
// `instantiate_from` reuses; a 39 MiB module is compiled a single time and
// instantiated N times (the browser-native runner needs one instance per
// nested guest layer — world, control, client).
export function compile_module(wasm) {
  const mid = nextModuleId++;
  modules.set(mid, new WebAssembly.Module(wasm));
  return mid;
}

// The per-instance imports: `host_call`/`random`/`log` closures bound to this
// guest's id, plus the minimal `__wbindgen` stubs the module declares. Every
// instance of one module builds its own set (separate memory, separate
// hostCall).
function buildImports(id, hostCall, module) {
  const imports = {
    lait: {
      host_call: (opPtr, opLen, payloadPtr, payloadLen) => {
        const guest = guests.get(id);
        const view = bytes(guest);
        const op = new TextDecoder().decode(view.subarray(opPtr, opPtr + opLen));
        const payload = view.slice(payloadPtr, payloadPtr + payloadLen);
        const answer = hostCall(op, payload);
        // Land the answer in the guest's own memory via its allocator; the
        // guest reads and frees it.
        const outPtr = guest.exports.wr_alloc(answer.length);
        bytes(guest).set(answer, outPtr);
        return pack(outPtr, answer.length);
      },
      // Host entropy: a wasm runner takes randomness from the host rather than
      // reaching `crypto` itself. crypto.getRandomValues caps at 65536 bytes
      // per call, so fill in chunks.
      random: (ptr, len) => {
        const view = new Uint8Array(guests.get(id).exports.memory.buffer, ptr, len);
        for (let off = 0; off < len; off += 65536) {
          crypto.getRandomValues(view.subarray(off, Math.min(off + 65536, len)));
        }
      },
      log: (ptr, len) => {
        const view = new Uint8Array(guests.get(id).exports.memory.buffer, ptr, len);
        // eslint-disable-next-line no-console
        console.log("[guest]", new TextDecoder().decode(view));
      },
    },
  };
  // A near-pure guest still carries a little wasm-bindgen scaffolding from
  // web-time's clock (Date.now returns a number, so no externref ever
  // crosses). Provide minimal stubs for whatever `__wbindgen`/`__wbg` imports
  // the module declares — matched by shape, since some names carry a
  // per-build hash — so the module instantiates without its bindgen runtime.
  for (const imp of WebAssembly.Module.imports(module)) {
    if (!imp.module.startsWith("__wbindgen")) continue;
    const table = (imports[imp.module] ??= {});
    if (imp.name.startsWith("__wbg_now")) {
      table[imp.name] = () => Date.now();
    } else if (imp.name === "__wbindgen_throw") {
      table[imp.name] = (ptr, len) => {
        const view = bytes(guests.get(id));
        throw new Error(new TextDecoder().decode(view.subarray(ptr, ptr + len)));
      };
    } else {
      // __wbindgen_describe, externref table ops: unreachable for a guest that
      // never stores a JS value. No-ops keep instantiation happy.
      table[imp.name] = () => 0;
    }
  }
  return imports;
}

// Instantiate a guest from an already-compiled module id. `hostCall(op,
// payload)` is the engine-side callback (operation string + payload bytes ->
// answer bytes, a postcard `Result<Vec<u8>, String>`).
export function instantiate_from(moduleId, hostCall) {
  const module = modules.get(moduleId);
  if (!module) throw new Error("unknown compiled module id");
  const id = nextId++;
  const imports = buildImports(id, hostCall, module);
  const instance = new WebAssembly.Instance(module, imports);
  guests.set(id, { exports: instance.exports });
  return id;
}

// Instantiate a guest from its wasm bytes — compile then instantiate, for
// one-shot callers. Reuse `compile_module` + `instantiate_from` when more than
// one instance of the same module is needed.
export function instantiate_guest(wasm, hostCall) {
  return instantiate_from(compile_module(wasm), hostCall);
}

// Call one guest export that takes a (ptr, len) input frame and returns a
// packed (ptr, len) output frame. Copies the input into the guest, reads the
// output out, frees the output. The guest frees the input itself, as its
// export does under wasmtime too.
export function call_guest(id, name, frame) {
  const guest = guests.get(id);
  const inPtr = guest.exports.wr_alloc(frame.length);
  bytes(guest).set(frame, inPtr);
  const packed = guest.exports[name](inPtr, frame.length);
  const outPtr = Number(packed >> 32n);
  const outLen = Number(packed & 0xffffffffn);
  const out = bytes(guest).slice(outPtr, outPtr + outLen);
  guest.exports.wr_free(outPtr, outLen);
  return out;
}

export function drop_guest(id) {
  guests.delete(id);
}
