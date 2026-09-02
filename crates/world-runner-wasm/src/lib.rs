// The wasm ABI moves u32 memory offsets and i32 handles across linear memory,
// and erases one callback's borrow lifetime for the duration of a single call.
// Those casts and that transmute are the boundary, not a defect.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::transmute_ptr_to_ptr,
    clippy::type_complexity,
    clippy::significant_drop_tightening
)]

//! The wasmtime reference host for a World runner compiled to WebAssembly.
//!
//! A native runner is a child process the daemon supervises over a socket; a
//! wasm runner is a module the daemon runs in-process. Both answer the same
//! postcard [`world_runner::Operation`]/[`world_runner::Reply`] frames, so this
//! crate implements the [`world_runner::HostedRunner`] seam and everything
//! above the frame — all of `world-sdk`, all product code — is unchanged.
//!
//! The one difference from the socket transport, documented because it is
//! load-bearing: a native request runs after the supervision lock is released
//! and a callback re-enters over a second connection; a wasm request holds the
//! store for its whole duration, because the guest's host callbacks are
//! synchronous nested calls (the guest calls the imported `host_call`, the host
//! answers it without re-entering the guest). The retained-Find-lease case,
//! where a native World's background thread issues `find.query_detached` after
//! the reply, does not arise here: a single guest instance is idle once
//! `wr_handle` returns, so it emits no post-return callback. The only
//! first-party path that retains a lease — the Issues Geometry projection —
//! runs its build inline on wasm (`products/issues` `GeometryExecutor` is a
//! cfg-split unit struct there), draining the lease through synchronous
//! callbacks before `wr_handle` returns. So the detached handler is correctly
//! dead on wasm, and no fifth "pump" export is owed — the "re-model detached
//! as in-request" that a thread-pool host would need is already the wasm
//! executor's behavior. The proof is `a_deferred_lease_drains_inline_on_wasm`.

use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Result};
use wasmtime::{
    Caller, Config, Engine, Instance, Linker, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder,
};
use world_runner::wasm_abi::{exports, imports, pack, unpack, GuestInit};
use world_runner::{
    CallbackHandler, Conversation, HostedRunner, Operation, Reply, ServiceDescriptor,
};

/// Above the guest's own working set, its linear memory must still hold a
/// 64 MiB frame in and a 64 MiB frame out. typst layout is memory-hungry, so
/// the ceiling is generous; it exists to bound a runaway allocation, not to
/// pace normal work.
const MEMORY_LIMIT: usize = 1 << 30;

/// Wall-clock budget for one guest request, mirroring the native runner's
/// 30-second read timeout, enforced by epoch interruption: a background ticker
/// advances the engine's epoch once a second, and a request that outlives its
/// deadline traps rather than burning the host.
const REQUEST_DEADLINE_TICKS: u64 = 30;

fn wt(error: wasmtime::Error, context: &str) -> anyhow::Error {
    anyhow!("{context}: {error:?}")
}

/// The caller's synchronous callback the guest's imported `host_call` reaches,
/// with its borrow lifetime erased to `'static`. It is installed and cleared
/// inside one `dispatch`, so the erased lifetime never outlives the real one.
type ErasedCallback =
    *mut (dyn FnMut(&str, &[u8]) -> std::result::Result<Vec<u8>, String> + 'static);

struct HostCtx {
    limits: StoreLimits,
    callback: Option<ErasedCallback>,
}

// SAFETY: the callback pointer is installed and cleared within a single
// `dispatch` on one thread; the store is not shared while a call is in flight.
unsafe impl Send for HostCtx {}

/// The bounds one guest runs under. Production values come from [`Limits::default`];
/// a test tightens them so a runaway or over-allocating guest is caught quickly.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// The ceiling on the guest's linear memory.
    pub memory_bytes: usize,
    /// How many epoch ticks a single request may run before it traps.
    pub deadline_ticks: u64,
    /// How often the background ticker advances the engine's epoch. One tick
    /// times `deadline_ticks` is the wall-clock request budget.
    pub tick: std::time::Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory_bytes: MEMORY_LIMIT,
            deadline_ticks: REQUEST_DEADLINE_TICKS,
            tick: std::time::Duration::from_secs(1),
        }
    }
}

/// The immutable module plus the live instance, re-instantiated on a trap.
pub struct WasmInstance {
    engine: Engine,
    module: Module,
    descriptor: ServiceDescriptor,
    init: GuestInit,
    limits: Limits,
    /// Shared with every [`WasmConversation`] this instance hands out, so a
    /// conversation is owned (independent of the supervision lock) exactly as
    /// the native `RequestClient` is.
    live: Arc<Mutex<Live>>,
}

struct Live {
    store: Store<HostCtx>,
    instance: Instance,
}

impl WasmInstance {
    /// Compile a module and instantiate it under production bounds, running
    /// `init` to build the guest service and read back its descriptor — the
    /// wasm analog of a process announcing readiness and answering Describe.
    pub fn launch(wasm: &[u8], init: GuestInit) -> Result<Self> {
        Self::launch_with_limits(wasm, init, Limits::default())
    }

    /// [`Self::launch`] under explicit bounds — the seam a test tightens to
    /// prove the memory ceiling and the request deadline actually bite.
    pub fn launch_with_limits(wasm: &[u8], init: GuestInit, limits: Limits) -> Result<Self> {
        let mut config = Config::new();
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(|e| wt(e, "build wasm engine"))?;
        let module = Module::new(&engine, wasm).map_err(|e| wt(e, "compile World module"))?;
        let (live, descriptor) = instantiate(&engine, &module, &init, &limits)?;
        // One ticker for the engine drives every store's epoch deadline.
        let ticker = engine.weak();
        let tick = limits.tick;
        std::thread::Builder::new()
            .name("world-runner-wasm-epoch".into())
            .spawn(move || loop {
                std::thread::sleep(tick);
                match ticker.upgrade() {
                    Some(engine) => engine.increment_epoch(),
                    None => return,
                }
            })
            .map_err(|e| anyhow!("spawn epoch ticker: {e}"))?;
        Ok(Self {
            engine,
            module,
            descriptor,
            init,
            limits,
            live: Arc::new(Mutex::new(live)),
        })
    }
}

impl HostedRunner for WasmInstance {
    fn descriptor(&self) -> &ServiceDescriptor {
        &self.descriptor
    }

    fn open(&mut self) -> Result<Box<dyn Conversation>> {
        Ok(Box::new(WasmConversation {
            live: Arc::clone(&self.live),
            engine: self.engine.clone(),
            module: self.module.clone(),
            descriptor: self.descriptor.clone(),
            init: self.init.clone(),
            limits: self.limits,
        }))
    }
}

/// One prepared request. Owns a handle to the shared live instance, so it does
/// not borrow the runner's supervision lock — mirroring the native client.
pub struct WasmConversation {
    live: Arc<Mutex<Live>>,
    engine: Engine,
    module: Module,
    descriptor: ServiceDescriptor,
    init: GuestInit,
    limits: Limits,
}

impl Conversation for WasmConversation {
    fn dispatch(
        &mut self,
        operation: Operation,
        callback: &mut dyn FnMut(&str, &[u8]) -> std::result::Result<Vec<u8>, String>,
        // Deliberately dropped, and correctly: a detached handler services
        // callbacks a retained Find lease raises AFTER the reply, and a single
        // guest instance is idle once `wr_handle` returns, so it emits none.
        // The native backend spawns a thread because its build runs after the
        // reply; the wasm build runs INLINE (the Issues `GeometryExecutor` is a
        // cfg-split unit struct on wasm), draining the lease through
        // synchronous callbacks before the reply. No pump export is owed — see
        // the module doc and `a_deferred_lease_drains_inline_on_wasm`.
        _detached: Arc<dyn CallbackHandler>,
    ) -> Result<Reply> {
        let request_frame = world_runner::wasm_abi::encode(&world_runner::Request {
            protocol: world_runner::PROTOCOL_VERSION,
            token: String::new(),
            id: 0,
            operation,
        })
        .map_err(|e| anyhow!("encode wasm request: {e}"))?;

        let mut guard = self
            .live
            .lock()
            .map_err(|_| anyhow!("World runner store lock was poisoned"))?;
        // One reborrow so `store` and `instance` are disjoint field borrows
        // through the guard rather than two borrows of the guard itself.
        let live = &mut *guard;
        live.store.set_epoch_deadline(self.limits.deadline_ticks);

        // Install the caller's callback for the life of this call; cleared
        // before the lock is released, even if the guest traps. SAFETY: the
        // pointer is used only within this call and cleared below.
        let installed: ErasedCallback = unsafe {
            std::mem::transmute::<
                *mut (dyn FnMut(&str, &[u8]) -> std::result::Result<Vec<u8>, String> + '_),
                ErasedCallback,
            >(callback)
        };
        live.store.data_mut().callback = Some(installed);
        let result = call_guest(
            &mut live.store,
            &live.instance,
            exports::HANDLE,
            &request_frame,
        );
        live.store.data_mut().callback = None;

        let outcome_bytes = match result {
            Ok(bytes) => bytes,
            Err(error) => {
                // A trap poisons the store; re-instantiate the immutable module
                // so the next call meets a fresh guest of the same identity.
                let (engine, module, init) = (&self.engine, &self.module, &self.init);
                if let Ok((fresh, descriptor)) = instantiate(engine, module, init, &self.limits) {
                    if descriptor == self.descriptor {
                        *live = fresh;
                    }
                }
                return Err(error).map_err(|e| anyhow!("World runner trapped: {e:?}"));
            }
        };
        let outcome: std::result::Result<Reply, String> =
            world_runner::wasm_abi::decode(&outcome_bytes)
                .map_err(|e| anyhow!("decode wasm reply: {e}"))?;
        outcome.map_err(|message| anyhow!(message))
    }
}

fn instantiate(
    engine: &Engine,
    module: &Module,
    init: &GuestInit,
    limits: &Limits,
) -> Result<(Live, ServiceDescriptor)> {
    let mut store = Store::new(
        engine,
        HostCtx {
            limits: StoreLimitsBuilder::new()
                .memory_size(limits.memory_bytes)
                .build(),
            callback: None,
        },
    );
    store.limiter(|ctx| &mut ctx.limits);
    store.set_epoch_deadline(limits.deadline_ticks);

    let mut linker = Linker::new(engine);
    linker
        .func_wrap(
            imports::MODULE,
            imports::HOST_CALL,
            |caller: Caller<'_, HostCtx>,
             op_ptr: u32,
             op_len: u32,
             payload_ptr: u32,
             payload_len: u32|
             -> std::result::Result<i64, wasmtime::Error> {
                host_call(caller, op_ptr, op_len, payload_ptr, payload_len)
            },
        )
        .map_err(|e| wt(e, "register host_call import"))?;

    let instance = linker
        .instantiate(&mut store, module)
        .map_err(|e| wt(e, "instantiate World module"))?;

    let init_frame = postcard::to_stdvec(init).map_err(|e| anyhow!("encode guest init: {e}"))?;
    let descriptor_bytes = call_guest(&mut store, &instance, exports::INIT, &init_frame)?;
    let descriptor: ServiceDescriptor = postcard::from_bytes(&descriptor_bytes)
        .map_err(|e| anyhow!("guest init returned no descriptor: {e}"))?;
    Ok((Live { store, instance }, descriptor))
}

/// Call one guest export that takes `(ptr, len)` of an input frame and returns
/// a packed `(ptr, len)` of an output frame. Copies the input into a guest
/// allocation, reads the output out, and frees the output.
fn call_guest(
    store: &mut Store<HostCtx>,
    instance: &Instance,
    export: &str,
    input: &[u8],
) -> Result<Vec<u8>> {
    if input.len() > world_runner::MAX_FRAME_BYTES {
        bail!("World frame exceeds its bound");
    }
    let memory = instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| anyhow!("World module exports no memory"))?;
    let alloc = instance
        .get_typed_func::<i32, i32>(&mut *store, exports::ALLOC)
        .map_err(|e| wt(e, "World module exports no alloc"))?;
    let free = instance
        .get_typed_func::<(i32, i32), ()>(&mut *store, exports::FREE)
        .map_err(|e| wt(e, "World module exports no free"))?;
    let handle = instance
        .get_typed_func::<(i32, i32), i64>(&mut *store, export)
        .map_err(|e| wt(e, "World module exports no entry"))?;

    let in_ptr = alloc
        .call(&mut *store, input.len() as i32)
        .map_err(|e| wt(e, "guest alloc"))?;
    memory
        .write(&mut *store, in_ptr as usize, input)
        .map_err(|e| anyhow!("write guest memory: {e}"))?;
    // The guest frees the input inside its export; the host does not.
    let packed = handle
        .call(&mut *store, (in_ptr, input.len() as i32))
        .map_err(|e| wt(e, "guest dispatch"))?;
    let (out_ptr, out_len) = unpack(packed);
    if out_len as usize > world_runner::MAX_FRAME_BYTES {
        bail!("World reply exceeds its bound");
    }
    let mut out = vec![0_u8; out_len as usize];
    memory
        .read(&mut *store, out_ptr as usize, &mut out)
        .map_err(|e| anyhow!("read guest memory: {e}"))?;
    free.call(&mut *store, (out_ptr as i32, out_len as i32))
        .map_err(|e| wt(e, "free guest reply"))?;
    Ok(out)
}

/// The imported `host_call`: read the operation and payload from guest memory,
/// answer through the installed callback, and hand the answer back through a
/// fresh guest allocation.
fn host_call(
    mut caller: Caller<'_, HostCtx>,
    op_ptr: u32,
    op_len: u32,
    payload_ptr: u32,
    payload_len: u32,
) -> std::result::Result<i64, wasmtime::Error> {
    let memory: Memory = caller
        .get_export("memory")
        .and_then(|export| export.into_memory())
        .ok_or_else(|| wasmtime::Error::msg("World module exports no memory"))?;
    let operation = read_bytes(&caller, &memory, op_ptr, op_len)?;
    let operation = String::from_utf8(operation)
        .map_err(|_| wasmtime::Error::msg("callback operation is not UTF-8"))?;
    let payload = read_bytes(&caller, &memory, payload_ptr, payload_len)?;

    let answer: std::result::Result<Vec<u8>, String> = match caller.data().callback {
        // SAFETY: the pointer is valid for this dispatch; `dispatch` installs it
        // before the guest runs and clears it after, single-threaded.
        Some(callback) => unsafe { (*callback)(&operation, &payload) },
        None => Err("World raised a callback outside a live request".to_string()),
    };
    let answer_frame = world_runner::wasm_abi::encode(&answer)
        .map_err(|e| wasmtime::Error::msg(format!("encode callback answer: {e}")))?;

    // Allocate in guest memory via the guest's own alloc and write the answer
    // there; the guest reads and frees it.
    let alloc = caller
        .get_export(exports::ALLOC)
        .and_then(|export| export.into_func())
        .ok_or_else(|| wasmtime::Error::msg("World module exports no alloc"))?
        .typed::<i32, i32>(&caller)?;
    let out_ptr = alloc.call(&mut caller, answer_frame.len() as i32)?;
    memory
        .write(&mut caller, out_ptr as usize, &answer_frame)
        .map_err(|e| wasmtime::Error::msg(format!("write guest memory: {e}")))?;
    Ok(pack(out_ptr as u32, answer_frame.len() as u32))
}

fn read_bytes(
    caller: &Caller<'_, HostCtx>,
    memory: &Memory,
    ptr: u32,
    len: u32,
) -> std::result::Result<Vec<u8>, wasmtime::Error> {
    let mut out = vec![0_u8; len as usize];
    memory
        .read(caller, ptr as usize, &mut out)
        .map_err(|e| wasmtime::Error::msg(format!("read guest memory: {e}")))?;
    Ok(out)
}
