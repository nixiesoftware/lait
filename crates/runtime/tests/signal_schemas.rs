//! What every signal declaration must satisfy before anything can send one.
//!
//! A declaration is what makes a signal bounded, authorized and answerable, so
//! these are the properties that hold for all of them at once — the kind that
//! are easy to break by adding a seventh declaration and only checking the one
//! you added.

use runtime::planes::bounds;
use runtime::signal::{core_declarations, declaration_for, selector, ResponsePolicy, SignalError};

#[test]
fn no_declaration_may_exceed_the_planes_own_ceiling() {
    // A per-signal bound above the plane's would be a number that never
    // applies: the transport refuses first, and the declaration's ceiling would
    // be a comment describing a limit nothing enforces.
    for declaration in core_declarations() {
        assert!(
            declaration.max_bytes <= bounds::MAX_SIGNAL_BYTES,
            "selector {:#06x} declares {} bytes, past the plane's {}",
            declaration.selector,
            declaration.max_bytes,
            bounds::MAX_SIGNAL_BYTES
        );
        assert!(
            declaration.max_bytes > 0,
            "selector {:#06x} declares a bound nothing can satisfy",
            declaration.selector
        );
    }
}

#[test]
fn selectors_are_distinct_and_every_one_resolves() {
    // A collision is two signals that decode as each other, which is the kind
    // of thing that only shows up when the second one ships.
    let declarations = core_declarations();
    let mut seen = std::collections::BTreeSet::new();
    for declaration in &declarations {
        assert!(
            seen.insert(declaration.selector),
            "selector {:#06x} is declared twice",
            declaration.selector
        );
    }
    for declaration in &declarations {
        assert_eq!(
            declaration_for(declaration.selector).as_ref(),
            Some(declaration),
            "a declared selector must resolve to its own declaration"
        );
    }
}

#[test]
fn an_undeclared_selector_is_refused_rather_than_guessed() {
    // A signal nobody declared is one nothing knows how large it may be or who
    // may send it. There is no safe default for either, so there is no default.
    assert_eq!(declaration_for(0xFFFF), None);
    assert_eq!(declaration_for(0x0000), None);
    assert_eq!(SignalError::NotRegistered.code(), "signal-not-registered");
}

#[test]
fn only_a_ping_expects_an_answer() {
    // An answer is a second round trip and a second deadline. A signal that
    // does not need one should not pay for one — and an acknowledgement that
    // could itself be acknowledged is how a ping becomes a loop.
    for declaration in core_declarations() {
        let expected = if declaration.selector == selector::PING {
            ResponsePolicy::Acknowledge
        } else {
            ResponsePolicy::Forbidden
        };
        assert_eq!(
            declaration.response, expected,
            "selector {:#06x}",
            declaration.selector
        );
    }
}

#[test]
fn a_file_offer_is_an_offer_and_not_an_acceptance() {
    // Whether the receiver wants the file is a decision a person makes, not a
    // protocol answer due inside a deadline. If this ever becomes
    // `Acknowledge`, somebody has made a person's choice into a timeout.
    let offer = declaration_for(selector::FILE_OFFER).expect("declared");
    assert_eq!(offer.response, ResponsePolicy::Forbidden);
}

#[test]
fn every_failure_has_a_distinct_stable_code() {
    // The codes are what a client branches on. Two failures sharing one is a
    // client that cannot tell them apart, which defeats the point of typing
    // them at all.
    let errors = [
        SignalError::NotRegistered,
        SignalError::Denied,
        SignalError::TooLarge,
        SignalError::Malformed,
        SignalError::Deadline,
        SignalError::OverBudget,
        SignalError::PeerRefused,
        SignalError::LaneNotGranted,
    ];
    let mut seen = std::collections::BTreeSet::new();
    for error in &errors {
        let code = error.code();
        assert!(code.starts_with("signal-"), "{code} is not namespaced");
        assert!(seen.insert(code), "{code} is used twice");
    }
}
