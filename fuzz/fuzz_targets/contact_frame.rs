//! The Contact frame decoder, against coverage-guided input.
//!
//! `contact_frame_fuzz.rs` in the runtime crate asserts the same property with
//! a proptest generator, and that is the version that runs on every push. This
//! is the one that explores: libFuzzer watches which branches the input
//! reached and mutates toward the ones it has not, which finds the inputs a
//! blind generator would need a very long time to stumble into.
//!
//! The property is identical and deliberately minimal — decoding untrusted
//! bytes must return a Result, never unwind. A panic here is reachable by
//! anyone who can open a connection, before any signature has been checked.
#![no_main]

use libfuzzer_sys::fuzz_target;
use runtime::plane::contact::ContactFrame;

fuzz_target!(|data: &[u8]| {
    // The result is deliberately discarded. Reaching the next line at all is
    // the assertion: no unwind, no abort, no infinite loop.
    let _ = ContactFrame::decode(data);
});
