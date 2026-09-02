//! The browser control vocabulary, pinned against the wire.
//!
//! `browser-control` classifies every control command a browser-composed
//! backend might receive — Answered, DaemonOnly, or NotYet. This test holds
//! that classification complete against `control::Request` itself, the enum
//! the daemon dispatches from, in both directions: a command added to the
//! wire fails here until it is placed, and a placement naming no real command
//! fails too. It is the `mcp::ShellTool` completeness pattern one layer up —
//! and it lives in the root crate because only here are the wire enum and the
//! classification both visible (browser-control cannot depend on the
//! native-only root crate, and the root crate must never depend on
//! browser-control except for this pin).

use browser_control::cmd::{Disposition, CLASSIFIED};
use browser_control::{disposition, refuse::Refusal};
use lait::control::representative_requests;

/// Every wire `cmd` tag, from the production enum's own representative set.
fn wire_cmds() -> std::collections::BTreeSet<String> {
    representative_requests()
        .iter()
        .map(|r| {
            serde_json::to_value(r).unwrap()["cmd"]
                .as_str()
                .expect("every request serializes with a cmd tag")
                .to_string()
        })
        .collect()
}

#[test]
fn every_wire_command_is_classified_and_nothing_phantom_is() {
    let wire = wire_cmds();
    let classified: std::collections::BTreeSet<String> = CLASSIFIED
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect();

    let unclassified: Vec<&String> = wire.difference(&classified).collect();
    assert!(
        unclassified.is_empty(),
        "these control commands reach a browser backend unclassified — place each \
         in browser_control::cmd::CLASSIFIED: {unclassified:?}"
    );
    let phantom: Vec<&String> = classified.difference(&wire).collect();
    assert!(
        phantom.is_empty(),
        "these are classified but name no wire command (a rename or a typo): {phantom:?}"
    );
}

#[test]
fn every_wire_command_resolves_a_disposition() {
    // The `disposition` lookup the Worker will call answers for every real
    // command — never a `None` that would strand a frame the backend does in
    // fact understand.
    for cmd in wire_cmds() {
        assert!(disposition(&cmd).is_some(), "{cmd} resolves no disposition");
    }
}

#[test]
fn the_load_bearing_placements_are_what_they_claim() {
    // A handful pinned by intent, so a careless bulk edit that flipped them
    // fails loudly rather than silently changing what a browser answers.
    assert_eq!(disposition("whoami"), Some(Disposition::Answered));
    assert_eq!(disposition("members"), Some(Disposition::Answered));
    // Privileged authority writes — the ShellTool-withheld class — are the
    // daemon's by nature, never answered browser-side.
    for daemon_only in [
        "member_remove",
        "key_rotate",
        "invite",
        "member_add",
        "agent_provision",
    ] {
        assert_eq!(
            disposition(daemon_only),
            Some(Disposition::DaemonOnly),
            "{daemon_only} must be daemon-only"
        );
    }
    // Identity-scoped daemon facilities.
    for daemon_only in [
        "book_put",
        "correspond_invite",
        "host_context",
        "host_orbit_forget",
    ] {
        assert_eq!(
            disposition(daemon_only),
            Some(Disposition::DaemonOnly),
            "{daemon_only}"
        );
    }
    // Station-scoped readings not answered yet — refused honestly, not denied.
    assert_eq!(disposition("status"), Some(Disposition::NotYet));
    assert_eq!(disposition("device_list"), Some(Disposition::NotYet));
}

#[test]
fn a_daemon_only_refusal_names_no_wire_command_as_the_wrong_mount() {
    // The refusal a browser backend gives must never imitate the native head's
    // wrong-mount refusal, which the viewer replays against. Check it over the
    // real command set, not just samples.
    for (cmd, placed) in CLASSIFIED {
        let refusal = match placed {
            Disposition::DaemonOnly => Refusal::daemon_only(cmd),
            Disposition::NotYet => Refusal::not_yet(cmd),
            Disposition::Answered => continue,
        };
        assert!(
            !refusal.message.starts_with("this head serves '"),
            "{cmd}'s refusal imitates the wrong-mount refusal"
        );
        assert_ne!(refusal.status, 404, "{cmd}'s refusal must not be a 404");
    }
}
