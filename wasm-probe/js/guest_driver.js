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

// Re-fetch the memory view every time: a `wr_alloc` may have grown linear
// memory, which detaches any prior ArrayBuffer view.
function bytes(guest) {
  return new Uint8Array(guest.exports.memory.buffer);
}

function pack(ptr, len) {
  return (BigInt(ptr >>> 0) << 32n) | BigInt(len >>> 0);
}

// Instantiate a guest from its wasm bytes. `hostCall(op, payload)` is the
// engine-side callback: it takes the operation string and the payload bytes
// and returns the answer bytes (a postcard `Result<Vec<u8>, String>`).
export function instantiate_guest(wasm, hostCall) {
  const id = nextId++;
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
    },
  };
  const instance = new WebAssembly.Instance(new WebAssembly.Module(wasm), imports);
  guests.set(id, { exports: instance.exports });
  return id;
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
