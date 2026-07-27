//! Application package for the bundled Issues World.
//!
//! [`issues`] remains the pure semantic World. This package owns the external
//! application protocol and the client interfaces mounted by the `lait`
//! navigation shell. It deliberately has no dependency on the root binary,
//! daemon, control protocol, filesystem, or process lifecycle.

pub mod protocol;

pub use protocol::{
    decode_call, decode_reply, encode_call, encode_reply, BoardPos, Filter, IssuesRequest,
    OPERATION, VERSION,
};
