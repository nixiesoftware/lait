//! Application package for the bundled Issues World.
//!
//! [`issues`] remains the pure semantic World. This package owns the external
//! application protocol and the client interfaces mounted by the `lait`
//! navigation shell. It deliberately has no dependency on the root binary,
//! daemon, control protocol, filesystem, or process lifecycle.

pub mod cli;
pub mod protocol;

pub use protocol::{
    decode_call, decode_reply, encode_call, encode_reply, BoardPos, Filter, IssuesRequest,
    OPERATION, VERSION,
};

/// The complete client-facing package mounted by the navigation shell.
pub fn package() -> Result<world_interface::WorldClientPackage, world_interface::InterfaceError> {
    world_interface::WorldClientPackage::new(
        issues::contract::world_id(),
        world_interface::CliMount::new("issues", cli::command, cli::parse),
        Vec::new(),
        "Work with issues, projects, planning, roles, and workflows in the selected Orbit.",
    )
}
