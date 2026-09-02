//! The wasm guest ABI: the contract a WebAssembly runner and its in-process
//! host agree on, so the existing postcard frames cross linear memory instead
//! of a socket.
//!
//! It is a transport substitution, not a new protocol. The frames in
//! [`crate::protocol`] are unchanged; what changes is that the guest exposes
//! four functions and imports one, and `(ptr, len)` pairs move bytes across
//! the boundary. This module holds only the names, the pointer packing, and
//! the one frame the socket transport never needed — the instantiation facts a
//! process reads from its environment but a wasm guest has none of. It compiles
//! on both sides: the host reads these to drive a module, the guest to answer.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Encode a value as a bare postcard frame — no length prefix. The wasm ABI
/// carries every length out of band in a `(ptr, len)` pair, so the stream
/// framing (`crate::encode_frame`, which prefixes four bytes) would be a second
/// length the decoder never strips. Both sides of the boundary use this pair.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, postcard::Error> {
    postcard::to_stdvec(value)
}

/// Decode a bare postcard frame written by [`encode`].
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, postcard::Error> {
    postcard::from_bytes(bytes)
}

/// The guest exports, by name. The host looks each up after instantiation.
pub mod exports {
    /// `alloc(len: i32) -> i32` — reserve `len` guest bytes for an inbound
    /// frame. The guest owns every allocation; the host writes into what this
    /// returns.
    pub const ALLOC: &str = "wr_alloc";
    /// `free(ptr: i32, len: i32)` — release a guest allocation.
    pub const FREE: &str = "wr_free";
    /// `init(ptr: i32, len: i32) -> i64` — build the World service from a
    /// postcard [`super::GuestInit`] at `(ptr, len)` and return the packed
    /// `(ptr, len)` of its postcard [`crate::ServiceDescriptor`]. Called once,
    /// mirroring a process's Ready→Describe handshake so admission can
    /// identity-check before dispatching.
    pub const INIT: &str = "wr_init";
    /// `handle(ptr: i32, len: i32) -> i64` — dispatch the postcard
    /// [`crate::protocol::Request`] at `(ptr, len)` and return the packed
    /// `(ptr, len)` of its postcard `Result<Reply, String>` outcome.
    pub const HANDLE: &str = "wr_handle";
}

/// The guest import, by name — the one route from guest back to host.
pub mod imports {
    /// The module every guest import is namespaced under.
    pub const MODULE: &str = "lait";
    /// `host_call(op_ptr, op_len, payload_ptr, payload_len) -> i64` — called
    /// during `handle`. The host reads the operation and payload from guest
    /// memory, answers it, writes a postcard `Result<Vec<u8>, String>` into a
    /// guest allocation, and returns its packed `(ptr, len)`.
    pub const HOST_CALL: &str = "host_call";
}

/// The facts a native runner reads from its environment (`LAIT_WORLD_ID`,
/// `LAIT_WORLD_VERSION`, `LAIT_WORLD_RELEASE`). A wasm guest has no
/// environment, so the host hands them to `init` — the same facts it already
/// holds on the [`crate::Release`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestInit {
    pub world: String,
    pub version: String,
    pub release: String,
}

/// Pack a guest `(ptr, len)` pair into the `i64` a guest export returns:
/// the pointer in the high 32 bits, the length in the low 32. Both are
/// u32 offsets into linear memory.
#[must_use]
pub fn pack(ptr: u32, len: u32) -> i64 {
    (((ptr as u64) << 32) | (len as u64)) as i64
}

/// Undo [`pack`].
#[must_use]
pub fn unpack(packed: i64) -> (u32, u32) {
    let bits = packed as u64;
    ((bits >> 32) as u32, (bits & 0xffff_ffff) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pointer_length_pair_survives_the_round_trip() {
        for (ptr, len) in [
            (0, 0),
            (1, 2),
            (0xdead_beef, 0x0102_0304),
            (u32::MAX, u32::MAX),
        ] {
            assert_eq!(unpack(pack(ptr, len)), (ptr, len));
        }
    }
}
