//! The guest half of the wasm ABI: what a WebAssembly World runner links to
//! answer its host.
//!
//! A native runner calls [`crate::serve`], which builds its [`Service`] once
//! and then blocks on an accept loop. A wasm guest cannot block — the host
//! drives it by calling exported functions — so `serve` splits: this module
//! provides the exported `alloc`/`free`/`init`/`handle` glue and the imported
//! `host_call`, and a runner registers its service builder with
//! [`export_world_runner!`] instead of calling `serve`.
//!
//! Everything here is single-threaded: `wasm32-unknown-unknown` has no threads,
//! so a plain `OnceLock` holds the service and there is no lock to contend.

use std::sync::Arc;

use crate::protocol::{Operation, Reply, Request};
use crate::wasm_abi::{decode, encode, unpack, GuestInit};
use crate::{Host, Service};

/// Reserve `len` bytes the host writes an inbound frame into. Returns a raw
/// offset into linear memory; the matching [`dealloc`] releases it.
#[must_use]
pub fn alloc(len: usize) -> *mut u8 {
    let mut buffer = Vec::<u8>::with_capacity(len);
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    ptr
}

/// Release what [`alloc`] returned.
///
/// # Safety
/// `ptr`/`len` must be a pair a prior [`alloc`] returned, freed at most once.
pub unsafe fn dealloc(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len != 0 {
        drop(Vec::from_raw_parts(ptr, 0, len));
    }
}

/// Copy `bytes` into a fresh guest allocation and return its `(ptr, len)` for
/// the host to read and then free.
#[doc(hidden)]
#[must_use]
pub fn hand_out(bytes: &[u8]) -> (u32, u32) {
    let ptr = alloc(bytes.len());
    // SAFETY: `alloc` reserved exactly `bytes.len()` bytes at `ptr`.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
    (ptr as u32, bytes.len() as u32)
}

/// The guest's [`Host`]: every callback crosses the one imported function.
struct GuestHost;

impl Host for GuestHost {
    fn call(&self, operation: &str, payload: &[u8]) -> Result<Vec<u8>, String> {
        // SAFETY: the import honors the ABI — it reads the two byte ranges,
        // answers, and returns a packed pointer to a guest allocation holding a
        // postcard `Result<Vec<u8>, String>`.
        let packed = unsafe {
            host_call(
                operation.as_ptr() as u32,
                operation.len() as u32,
                payload.as_ptr() as u32,
                payload.len() as u32,
            )
        };
        let (ptr, len) = unpack(packed);
        // SAFETY: the host handed back a guest allocation of exactly `len`.
        let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize).to_vec() };
        // The response landed in guest memory (the host called our `alloc`), so
        // the guest frees it.
        unsafe { dealloc(ptr as *mut u8, len as usize) };
        decode::<Result<Vec<u8>, String>>(&bytes)
            .map_err(|error| format!("malformed host callback answer: {error}"))?
    }
}

#[link(wasm_import_module = "lait")]
extern "C" {
    /// The imported host callback — see [`crate::wasm_abi::imports`].
    fn host_call(op_ptr: u32, op_len: u32, payload_ptr: u32, payload_len: u32) -> i64;
}

/// Run one request against the registered service, returning the postcard
/// `Result<Reply, String>` outcome bytes.
#[doc(hidden)]
#[must_use]
pub fn dispatch(service: &Arc<dyn Service>, request_frame: &[u8]) -> Vec<u8> {
    let outcome: Result<Reply, String> = match decode::<Request>(request_frame) {
        Ok(request) => match request.operation {
            Operation::Ping => Ok(Reply::Pong),
            Operation::Describe => Ok(Reply::Descriptor(service.descriptor())),
            // A wasm guest is torn down by dropping its store; there is no
            // process to ask to stop. `Stopping` keeps the reply shape.
            Operation::Stop => Ok(Reply::Stopping),
            Operation::Call { operation, payload } => service
                .call(&operation, &payload, Arc::new(GuestHost))
                .map(|payload| Reply::Call { payload })
                .map_err(|error| format!("{operation}: {error}")),
        },
        Err(error) => Err(format!("malformed World request: {error}")),
    };
    let bytes = encode(&outcome).unwrap_or_default();
    // Cap the reply on the writing side, as the native `write_frame` does: an
    // oversized reply the host would reject after the fact leaks the guest
    // allocation it was handed out in. Refuse it here, before `hand_out`.
    if bytes.len() > crate::MAX_FRAME_BYTES {
        return encode(&Err::<Reply, String>(
            "World reply exceeded its bound".to_string(),
        ))
        .unwrap_or_default();
    }
    bytes
}

/// Build one service through the registered builder and return the postcard
/// [`crate::ServiceDescriptor`] bytes — empty if init could not be read, which
/// the host observes as a failed connect.
#[doc(hidden)]
#[must_use]
pub fn init(
    build: fn(GuestInit) -> Arc<dyn Service>,
    init_frame: &[u8],
) -> (Arc<dyn Service>, Vec<u8>) {
    let facts = decode::<GuestInit>(init_frame).unwrap_or(GuestInit {
        world: String::new(),
        version: String::new(),
        release: String::new(),
    });
    let service = build(facts);
    let descriptor = encode(&service.descriptor()).unwrap_or_default();
    (service, descriptor)
}

/// Register a World runner's service builder and emit the four guest exports.
///
/// A runner calls this once at file scope instead of [`crate::serve`]. The
/// closure receives the host-supplied [`GuestInit`] facts (the wasm stand-in
/// for `LAIT_WORLD_ID` and friends) and returns the built service.
///
/// ```ignore
/// world_runner::export_world_runner!(|init| build_service(&init.world, &init.version));
/// ```
#[macro_export]
macro_rules! export_world_runner {
    ($build:expr) => {
        static __WR_SERVICE: ::std::sync::OnceLock<::std::sync::Arc<dyn $crate::Service>> =
            ::std::sync::OnceLock::new();

        #[no_mangle]
        pub extern "C" fn wr_alloc(len: i32) -> i32 {
            $crate::guest::alloc(len as usize) as i32
        }

        #[no_mangle]
        pub extern "C" fn wr_free(ptr: i32, len: i32) {
            // SAFETY: the host frees only pairs a guest export handed it.
            unsafe { $crate::guest::dealloc(ptr as *mut u8, len as usize) }
        }

        #[no_mangle]
        pub extern "C" fn wr_init(ptr: i32, len: i32) -> i64 {
            // SAFETY: the host wrote a GuestInit frame at (ptr, len).
            let frame =
                unsafe { ::std::slice::from_raw_parts(ptr as *const u8, len as usize).to_vec() };
            unsafe { $crate::guest::dealloc(ptr as *mut u8, len as usize) };
            let build: fn($crate::wasm_abi::GuestInit) -> ::std::sync::Arc<dyn $crate::Service> =
                $build;
            let (service, descriptor) = $crate::guest::init(build, &frame);
            let _ = __WR_SERVICE.set(service);
            let (out_ptr, out_len) = $crate::guest::hand_out(&descriptor);
            $crate::wasm_abi::pack(out_ptr, out_len)
        }

        #[no_mangle]
        pub extern "C" fn wr_handle(ptr: i32, len: i32) -> i64 {
            // SAFETY: the host wrote a Request frame at (ptr, len).
            let frame =
                unsafe { ::std::slice::from_raw_parts(ptr as *const u8, len as usize).to_vec() };
            unsafe { $crate::guest::dealloc(ptr as *mut u8, len as usize) };
            let service = __WR_SERVICE
                .get()
                .expect("World runner handled a request before init");
            let out = $crate::guest::dispatch(service, &frame);
            let (out_ptr, out_len) = $crate::guest::hand_out(&out);
            $crate::wasm_abi::pack(out_ptr, out_len)
        }
    };
}
