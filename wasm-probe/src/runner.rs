//! A World runner driven from the engine module, in a browser.
//!
//! The native daemon runs a wasm runner through wasmtime
//! (`crates/world-runner-wasm`). A browser has no wasmtime — the thing that
//! runs the guest wasm is the browser's own `WebAssembly`. So this is a second
//! implementation of the same [`world_runner::HostedRunner`]/[`Conversation`]
//! seam, whose host is JS glue (`js/guest_driver.js`) over the browser engine:
//! it instantiates the guest module, copies postcard frames across the two
//! modules' separate linear memories, and resolves the guest's synchronous
//! `host_call` import back into an engine-side callback.
//!
//! Everything is synchronous, as the four-function ABI requires. That is legal
//! because the whole chain runs in a Worker, where the ledger's OPFS I/O is a
//! synchronous access handle. Single-threaded and nested-synchronous — simpler
//! than the native path, which juggles a second connection and a supervision
//! lock. The native path's post-reply detached-callback thread has no analog
//! here and needs none: the one first-party lease-holder (Issues Geometry)
//! runs its build inline on wasm, draining the lease before the guest returns.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use wasm_bindgen::prelude::*;
use world_runner::wasm_abi::{decode, encode, exports, GuestInit};
use world_runner::{
    CallbackHandler, Conversation, HostedRunner, Operation, Reply, Request, ServiceDescriptor,
    PROTOCOL_VERSION,
};

#[wasm_bindgen(module = "/js/guest_driver.js")]
extern "C" {
    fn instantiate_guest(
        wasm: &[u8],
        host_call: &Closure<dyn FnMut(String, Vec<u8>) -> Vec<u8>>,
    ) -> u32;
    fn compile_module(wasm: &[u8]) -> u32;
    fn instantiate_from(
        module: u32,
        host_call: &Closure<dyn FnMut(String, Vec<u8>) -> Vec<u8>>,
    ) -> u32;
    #[wasm_bindgen(catch)]
    fn call_guest(id: u32, name: &str, frame: &[u8]) -> std::result::Result<Vec<u8>, JsValue>;
    fn drop_guest(id: u32);
}

/// A World runner module compiled once, ready to instantiate many times. The
/// browser-native runner needs one instance per nested guest layer (world,
/// control, client), and a 39 MiB module should be compiled a single time.
#[derive(Clone)]
pub struct WebModule {
    id: u32,
}

impl WebModule {
    /// Compile the runner module once.
    pub fn compile(wasm: &[u8]) -> Self {
        Self {
            id: compile_module(wasm),
        }
    }
}

/// The engine-side callback for the duration of one dispatch, borrow lifetime
/// erased. Installed and cleared inside `dispatch`, so the erased lifetime
/// never outlives the real one — and single-threaded, so nothing else observes
/// it. It is the browser analogue of the native host's `Store` callback slot.
type ErasedCallback =
    *mut (dyn FnMut(&str, &[u8]) -> std::result::Result<Vec<u8>, String> + 'static);

thread_local! {
    static CURRENT: Cell<Option<ErasedCallback>> = const { Cell::new(None) };
}

/// The instantiated guest and the JS closure that keeps its `host_call` alive.
struct Live {
    id: u32,
    // JS holds a reference to this closure for the guest's life; dropping it
    // would invalidate the import. Kept here, never called from Rust.
    _host_call: Closure<dyn FnMut(String, Vec<u8>) -> Vec<u8>>,
}

impl Drop for Live {
    fn drop(&mut self) {
        drop_guest(self.id);
    }
}

/// One World runner backed by a browser-instantiated wasm module.
pub struct WebInstance {
    module: u32,
    init: GuestInit,
    descriptor: ServiceDescriptor,
    live: Rc<std::cell::RefCell<Live>>,
}

// SAFETY: `wasm32-unknown-unknown` is single-threaded — there are no threads to
// send these between — so the `Rc` and the wasm-bindgen closure they hold never
// cross one. The `HostedRunner`/`Conversation` traits ask for `Send` because
// the native backend genuinely moves work across threads; here it is vacuous.
unsafe impl Send for WebInstance {}
unsafe impl Send for WebConversation {}

impl WebInstance {
    /// Instantiate the guest module under the browser's WebAssembly WITHOUT
    /// running `init` — proves the module compiles, fits in the tab's memory,
    /// and its imports resolve, separate from whether its service builds. The
    /// answer to "does a 39 MiB typst/CRDT runner even load in a browser".
    pub fn instantiate_module(wasm: &[u8]) -> Result<()> {
        let host_call = Closure::new(|_op: String, _payload: Vec<u8>| Vec::new());
        let id = instantiate_guest(wasm, &host_call);
        drop_guest(id);
        Ok(())
    }

    /// Instantiate a guest module and run `init` to build its service and read
    /// back the descriptor — the browser analogue of a process announcing
    /// readiness and answering Describe. Compiles the module; use
    /// [`Self::launch_from`] to share one compile across several instances.
    pub fn launch(wasm: &[u8], init: GuestInit) -> Result<Self> {
        Self::launch_from(&WebModule::compile(wasm), init)
    }

    /// Instantiate from an already-compiled [`WebModule`] — the shippable
    /// shape when several instances of one runner are needed (one per nested
    /// guest layer), paying the 39 MiB compile once.
    pub fn launch_from(module: &WebModule, init: GuestInit) -> Result<Self> {
        let (live, descriptor) = instantiate(module.id, &init)?;
        Ok(Self {
            module: module.id,
            init,
            descriptor,
            live: Rc::new(std::cell::RefCell::new(live)),
        })
    }
}

/// Build a fresh guest from a compiled module and answer its Describe.
fn instantiate(module: u32, init: &GuestInit) -> Result<(Live, ServiceDescriptor)> {
    let host_call = Closure::new(move |op: String, payload: Vec<u8>| -> Vec<u8> {
        let result: std::result::Result<Vec<u8>, String> = CURRENT.with(|current| {
            match current.get() {
                // SAFETY: the pointer is installed before the guest runs and
                // cleared after, single-threaded.
                Some(callback) => unsafe { (*callback)(&op, &payload) },
                None => Err("World raised a callback outside a live request".to_string()),
            }
        });
        encode(&result).unwrap_or_default()
    });
    let id = instantiate_from(module, &host_call);
    let live = Live {
        id,
        _host_call: host_call,
    };
    let init_frame = encode(init).map_err(|e| anyhow!("encode guest init: {e}"))?;
    let descriptor_bytes =
        call_guest(id, exports::INIT, &init_frame).map_err(|_| anyhow!("guest init trapped"))?;
    let descriptor: ServiceDescriptor =
        decode(&descriptor_bytes).map_err(|e| anyhow!("guest init returned no descriptor: {e}"))?;
    Ok((live, descriptor))
}

impl HostedRunner for WebInstance {
    fn descriptor(&self) -> &ServiceDescriptor {
        &self.descriptor
    }

    fn open(&mut self) -> Result<Box<dyn Conversation>> {
        Ok(Box::new(WebConversation {
            live: Rc::clone(&self.live),
            module: self.module,
            init: self.init.clone(),
            descriptor: self.descriptor.clone(),
        }))
    }
}

/// One prepared request. Shares the live guest with its instance, so it is
/// owned and independent — matching the native client.
pub struct WebConversation {
    live: Rc<std::cell::RefCell<Live>>,
    module: u32,
    init: GuestInit,
    descriptor: ServiceDescriptor,
}

impl Conversation for WebConversation {
    fn dispatch(
        &mut self,
        operation: Operation,
        callback: &mut dyn FnMut(&str, &[u8]) -> std::result::Result<Vec<u8>, String>,
        // Dropped, and correctly: a wasm guest emits no callback after it
        // returns, and the only lease-holding World path drains inline (see the
        // module note). No pump is owed.
        _detached: Arc<dyn CallbackHandler>,
    ) -> Result<Reply> {
        let frame = encode(&Request {
            protocol: PROTOCOL_VERSION,
            token: String::new(),
            id: 0,
            operation,
        })
        .map_err(|e| anyhow!("encode wasm request: {e}"))?;

        // Install the callback for this call, cleared after even on a trap.
        // SAFETY: used only within this call and cleared below.
        let installed: ErasedCallback = unsafe {
            std::mem::transmute::<
                *mut (dyn FnMut(&str, &[u8]) -> std::result::Result<Vec<u8>, String> + '_),
                ErasedCallback,
            >(callback)
        };
        CURRENT.with(|current| current.set(Some(installed)));
        let result = call_guest(self.live.borrow().id, exports::HANDLE, &frame);
        CURRENT.with(|current| current.set(None));

        let outcome_bytes = match result {
            Ok(bytes) => bytes,
            Err(_) => {
                // A trap discards the guest instance; re-instantiate from the
                // cached module so the next call meets a fresh guest of the
                // same identity — the browser analogue of restart_if_gone, and
                // cheaper now that it does not recompile.
                if let Ok((fresh, descriptor)) = instantiate(self.module, &self.init) {
                    if descriptor == self.descriptor {
                        *self.live.borrow_mut() = fresh;
                    }
                }
                return Err(anyhow!("World runner trapped"));
            }
        };
        let outcome: std::result::Result<Reply, String> =
            decode(&outcome_bytes).map_err(|e| anyhow!("decode wasm reply: {e}"))?;
        outcome.map_err(|message| anyhow!(message))
    }
}
