#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::arithmetic_side_effects,
        clippy::unreachable,
        clippy::unimplemented,
        clippy::unchecked_time_subtraction,
        clippy::todo,
        clippy::string_slice,
        clippy::panic_in_result_fn,
        clippy::panic,
        clippy::exit,
        clippy::as_conversions
    )
)]

//! Product-owned application and display package for Signage.

pub mod application;
mod athan;
mod display;
mod host;
mod protocol;
mod tv;

pub use protocol::{
    decode_call, decode_reply, encode_call, SignageCallHandler, SignageRequest, SignageResponse,
    OPERATION, VERSION,
};

pub const MOUNT: &str = "signage";
pub const DISPLAY_NAME: &str = "Signage";

pub fn implementation_id() -> [u8; 32] {
    signage::SignageWorld::implementation_descriptor()
        .id()
        .unwrap_or([0; 32])
}

pub fn package() -> Result<world_interface::WorldClientPackage, world_interface::Failure> {
    world_interface::WorldClientPackage::new(
        signage::contract::world_id(),
        MOUNT,
        world_interface::AgentSurface::designed(Vec::new(), "", &[]),
        decode_client_reply,
        // Compiled into this host build, so it travelled however the host did.
        // A World cannot know its own provenance; what it can say is that this
        // package is not read from a tree on somebody's disk.
        world_interface::Sealing::Sealed,
    )?
    .with_web_parser(host::parse_web)
    .with_local_handler(host::execute)
    .with_display(DISPLAY_NAME, Some("▣"), None)?
    .with_tagline("Author durable programs for managed displays")?
    .with_accent(0x009B_5DE5)?
    .with_display_surface(display::program_surface()?)
}

fn decode_client_reply(
    call: &runtime::world::call::Call,
    reply: runtime::world::call::Reply,
) -> Result<serde_json::Value, world_interface::Failure> {
    decode_reply(call, reply).map_err(|error| world_interface::Failure::new(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_registers_the_exact_signage_program_surface() {
        let package = package().unwrap();
        let id = world_interface::display::DisplaySurfaceId::new("signage.program").unwrap();
        let surface = package.display_surface(&id).unwrap();
        assert_eq!(
            surface.descriptor.runtime_implementation,
            implementation_id()
        );
        assert_eq!(surface.descriptor.contract_version, 4);
        assert!(surface
            .descriptor
            .outputs
            .contains(&world_interface::display::DisplayOutputKind::Media));
    }
}
