//! The Beacon decoder — the most exposed parser in the system.
//!
//! A Contact frame arrives on a connection someone chose to open. A Beacon
//! arrives from anyone gossiping on the topic, and
//! `SignedBeacon::decode_canonical` runs on every announcement the network
//! carries. Its `Vec<RouteHint>`, each with its own `Vec<u8>`, is nested
//! variable-length data behind a size cap — the classic place for a length to
//! be trusted that should not be.
#![no_main]

use libfuzzer_sys::fuzz_target;
use runtime::beacon::SignedBeacon;

fuzz_target!(|data: &[u8]| {
    let _ = SignedBeacon::decode_canonical(data);
});
