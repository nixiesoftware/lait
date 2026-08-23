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

pub mod application;
pub mod decorate;
pub mod display;
pub mod document;
pub mod host;
pub mod lifecycle;
pub mod mcp;
pub mod projections;
pub mod protocol;
pub mod router;

pub use protocol::{
    classify_failure, decode_call, decode_reply, encode_call, encode_reply, AccessAssignment,
    BoardPos, ChangeEffect, ChangeLabel, ChangeOperation, ChangePosition, ChangeProject,
    ChangeWorkAction, Filter, IssuesErrorKind, IssuesRequest, IssuesResponse, OperationPhase,
    OperationReadiness, OperationReceipt, PublicationCoordinate, WorldPublicationCoordinate,
    OPERATION, VERSION,
};
pub use router::{IssueRouter, IssuesCallHandler, RouterFacts};

/// What an agent is handed with the tool list. The verbs exist so this text
/// does not have to be rediscovered in a human session.
const ISSUES_MCP_INSTRUCTIONS: &str = "\
Plans, Specs, labels, milestones, teams, and issues are first-class MCP tools. \
A Spec is project truth: spec_new (draft) → spec_state review (any contributor) → \
spec_state issued (spec.issue or space.admin). Never rewrite an issued revision; \
spec_revise a successor and issue that. kind=plan seeds Blueprint from issue \
roots — put order in issues_parent and issues_link kind=blocks, not in prose. \
An Observation (spec_observe) notes; a Link governs. A Baseline is a named set \
of exact issued Spec revisions: issue the Specs first, then baseline_new, then \
baseline_state issued, then issue_baseline to pin it on work. Labels: \
label_new / label / label_list. Milestones: milestone_set / milestone_list / \
issue_milestone. Teams: team_set / team_list / project_edit team=. \
whoami lists your grants; issuing names spec.issue when you lack it. \
This MCP node spends your key — the HTTP custody fence does not apply here.";

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
        world_interface::AgentSurface::designed(
            mcp::tools(),
            ISSUES_MCP_INSTRUCTIONS,
            mcp::WITHOUT_A_TOOL,
        ),
        decode_client_reply,
    )
    .and_then(|package| {
        package
            .with_failure_classifier(classify_failure)
            .with_local_handler(host::execute)
            .with_web_parser(host::parse_web)
            .with_confirmation(host::confirmation)
            .with_decorator(decorate::decorate_reply)
            .with_transient_body(
                |document| Ok(issues::contract::issue_body_id(document).as_bytes()),
            )
            // What a client draws, and where `Open` lands. The display name is
            // the product's name, not the mount: `issues` is a namespace key
            // that prefixes tool names and route segments, and a person reading
            // a list should see what the thing is called.
            .with_display(DISPLAY_NAME, Some("📋"), Some("/"))?
            .with_tagline("Plans, issues, and the Specs that govern them")?
            // This release's own artwork. The mark is the whorl
            // alone, because the full print at the size a row draws a mark is
            // a smudge; the hero is the whole print, which is what it was
            // composed to be.
            .with_artwork(Some(MARK), Some(HERO))?
            // The tracker's own accent, as a seed rather than an asset. A
            // client derives a plate, an accent or a mark from it locally,
            // which is what keeps listing free.
            .with_accent(0x004C_6EF5)?
            .with_routes(ROUTES)
            // A board on a screen. Registered here rather than anywhere near
            // the receiver, which is the whole claim the surface contract
            // makes: a second, unlike World reaches a television with no
            // television application update and no product vocabulary crossing
            // the boundary.
            .and_then(|package| package.with_display_surface(display::board_wall_surface()?))
    })
}

/// The places inside this World somebody can go straight to.
///
/// Declared here because this World owns its URL grammar: the viewer addresses
/// a top-level view as `/spaces/{space}/{view}`, and a client that built that
/// shape itself would be holding a copy of a grammar it does not own — and
/// would keep building it after the day it changed.
const ROUTES: &[world_interface::Route] = &[
    world_interface::Route::new("Board", "/spaces/{space}/board"),
    world_interface::Route::new("Issues", "/spaces/{space}/list"),
    world_interface::Route::new("Specs", "/spaces/{space}/specs"),
    world_interface::Route::new("Projects", "/spaces/{space}/projects"),
    world_interface::Route::new("Activity", "/spaces/{space}/activity"),
    world_interface::Route::new("Settings", "/spaces/{space}/settings"),
];

/// The mark: the print's whorl, cropped, for a row's square plate.
const MARK: &[u8] = include_bytes!("../assets/mark.png");

/// The hero: the whole print, for the frame behind this World's name.
const HERO: &[u8] = include_bytes!("../assets/hero.png");

/// What this World is called when a person sees it in a list.
///
/// Distinct from [`MOUNT`], and deliberately: the mount is published machine
/// input that must never change, while this may change freely because nothing
/// resolves by it.
pub const DISPLAY_NAME: &str = "Issues";

fn decode_client_reply(
    call: &runtime::world::call::Call,
    reply: runtime::world::call::Reply,
) -> Result<serde_json::Value, world_interface::Failure> {
    decode_reply(call, reply).map_err(|error| world_interface::Failure::new(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mount and the display name are different things, and the seam only
    /// earns its keep if they stay different: the mount is published machine
    /// input that renames every tool an agent learned, and the name is for a
    /// person to read.
    #[test]
    fn the_world_declares_a_display_name_distinct_from_its_mount() {
        let package = package().expect("the package declares");
        let display = package.display();
        assert_eq!(display.name(), DISPLAY_NAME);
        assert_ne!(
            display.name(),
            MOUNT,
            "the display name is the mount, so nothing was actually declared"
        );
        assert_eq!(
            display.entry_path(),
            Some("/"),
            "the World declares no entry path, so a Library cannot open it"
        );
    }

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
                world_interface::Failure::refusal(),
                "you need write standing".into()
            ))
        );
        let missing = serde_json::to_value(IssuesResponse::not_found("no such issue")).unwrap();
        assert_eq!(
            classify_failure(&missing).map(|(failure, _)| failure),
            Some(world_interface::Failure::invalid())
        );
        let bad = serde_json::to_value(IssuesResponse::invalid(
            "Baseline member is not an issued Spec revision",
        ))
        .unwrap();
        assert_eq!(
            classify_failure(&bad).map(|(failure, _)| failure),
            Some(world_interface::Failure::invalid())
        );
        let fine = serde_json::to_value(IssuesResponse::Ok { message: None }).unwrap();
        assert_eq!(classify_failure(&fine), None);
    }
}
