//! The two pre-auth handshake decoders.
//!
//! `Offer` and `Proof` are the very first bytes of a connection — they decode
//! before any identity is established, because the signature that would
//! establish it is inside the thing being decoded.
#![no_main]

use libfuzzer_sys::fuzz_target;
use runtime::plane::contact::{Offer, Proof};

fuzz_target!(|data: &[u8]| {
    let _ = Offer::decode(data);
    let _ = Proof::decode(data);
});
