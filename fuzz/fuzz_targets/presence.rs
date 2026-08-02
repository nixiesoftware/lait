//! The presence challenge, both halves. Like the handshake, these decode
//! before the signature inside them has been checked.
#![no_main]

use libfuzzer_sys::fuzz_target;
use runtime::neighbor::{PresenceAck, PresenceProbe};

fuzz_target!(|data: &[u8]| {
    let _ = PresenceProbe::decode(data);
    let _ = PresenceAck::decode(data);
});
