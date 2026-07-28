//! Application package for the bundled Issues World.
//!
//! [`issues`] remains the pure semantic World. This package owns the external
//! application protocol and the client interfaces mounted by the `lait`
//! navigation shell. It deliberately has no dependency on the root binary,
//! daemon, or root control protocol.

pub mod cli;
pub mod host;
pub mod lifecycle;
pub mod mcp;
pub mod presentation;
pub mod projections;
pub mod protocol;
pub mod router;

pub use protocol::{
    decode_call, decode_reply, encode_call, encode_reply, AccessAssignment, BoardPos, Filter,
    IssuesErrorKind, IssuesRequest, IssuesResponse, OPERATION, VERSION,
};
pub use router::{IssueRouter, IssuesCallHandler, RouterFacts};

/// The complete client-facing package mounted by the navigation shell.
pub fn package() -> Result<world_interface::WorldClientPackage, world_interface::InterfaceError> {
    world_interface::WorldClientPackage::new(
        issues::contract::world_id(),
        world_interface::CliMount::new("issues", cli::command, cli::parse),
        mcp::tools(),
        "Work with issues, projects, planning, roles, and workflows in the selected Orbit.",
        decode_client_reply,
    )
    .map(|package| package.with_presenter(presentation::present))
}

fn decode_client_reply(
    call: &world_bridge::WorldCall,
    reply: world_bridge::WorldReply,
) -> Result<serde_json::Value, world_interface::InterfaceError> {
    decode_reply(call, reply)
        .map_err(|error| world_interface::InterfaceError::new(error.to_string()))
}
