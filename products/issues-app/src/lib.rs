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

//! Application package for the bundled Issues World.
//!
//! [`issues`] remains the pure semantic World. This package owns the external
//! application protocol and the client interfaces a host mounts. It deliberately
//! has no dependency on the root binary, daemon, or root control protocol — and
//! no dependency on any particular head: it answers in values, and whoever
//! composed it decides what a person sees.

pub mod document;
pub mod host;
pub mod lifecycle;
pub mod mcp;
pub mod projections;
pub mod protocol;
pub mod router;

pub use protocol::{
    classify_failure, decode_call, decode_reply, encode_call, encode_reply, AccessAssignment,
    BoardPos, Filter, IssuesErrorKind, IssuesRequest, IssuesResponse, OPERATION, VERSION,
};
pub use router::{IssueRouter, IssuesCallHandler, RouterFacts};

/// The namespace every head addresses this package by.
///
/// It prefixes all public MCP tool names (`issues_list`, not `list`) and it is
/// the `{world}` segment of the HTTP RPC route. Changing this string renames
/// every tool an agent has learned and breaks every URL a head has built, so it
/// is published API and not a label.
pub const MOUNT: &str = "issues";

/// The complete client-facing package a host mounts.
pub fn package() -> Result<world_interface::WorldClientPackage, world_interface::Failure> {
    world_interface::WorldClientPackage::new(
        issues::contract::world_id(),
        MOUNT,
        mcp::tools(),
        "Work with issues, projects, planning, roles, and workflows in the selected Orbit.",
        decode_client_reply,
    )
    .map(|package| {
        package
            .with_failure_classifier(classify_failure)
            .with_local_handler(host::execute)
            .with_web_parser(host::parse_web)
            .with_confirmation(host::confirmation)
    })
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

    /// The mount is published API. Every agent that has ever called this server
    /// learned tools named `issues_*`; a mount rename silently renames all of
    /// them at once, and every one of those agents breaks on the same deploy.
    #[test]
    fn every_public_tool_keeps_its_issues_prefix() {
        let registry = world_interface::WorldClientRegistry::new()
            .with_package(package().unwrap())
            .unwrap();
        let names: Vec<String> = registry.mcp_tools().map(|tool| tool.public_name).collect();
        assert!(!names.is_empty());
        for name in &names {
            assert!(
                name.starts_with("issues_"),
                "{name} lost the issues_ namespace"
            );
        }
        assert!(names.contains(&"issues_list".to_string()), "{names:?}");
        // And the same string is the route segment a head resolves against.
        assert_eq!(registry.package_for_mount(MOUNT).unwrap().mount(), "issues");
    }

    #[test]
    fn a_denied_answer_is_the_callers_problem_not_an_internal_fault() {
        let denied =
            serde_json::to_value(IssuesResponse::denied("you need write standing")).unwrap();
        assert_eq!(
            classify_failure(&denied),
            Some((
                world_interface::Failure::Refusal,
                "you need write standing".into()
            ))
        );
        let missing = serde_json::to_value(IssuesResponse::not_found("no such issue")).unwrap();
        assert_eq!(
            classify_failure(&missing).map(|(failure, _)| failure),
            Some(world_interface::Failure::Invalid)
        );
        let fine = serde_json::to_value(IssuesResponse::List { rows: Vec::new() }).unwrap();
        assert_eq!(classify_failure(&fine), None);
    }
}
