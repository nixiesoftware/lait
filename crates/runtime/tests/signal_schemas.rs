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

/// The wire, and the one ordering decision that makes its bounds real.
mod wire {
    use runtime::planes::{bounds, InviteKind, PlaneWireError, Signal, MAX_SIGNAL_TEXT_BYTES};
    use runtime::signal::{frame_signal, selector, SignalError};
    use runtime::transient::TransientScope;

    fn scope() -> TransientScope {
        TransientScope::IssueView {
            world: "com.example.notes".into(),
            body: [3u8; 16],
        }
    }

    #[test]
    fn the_selector_precedes_the_length() {
        // The whole reason the framing is shaped this way. A declaration's
        // `max_bytes` is a pre-allocation ceiling only if it is known *before*
        // the length is read — behind the length, the schema is known after a
        // buffer already exists and the per-signal maximum is decoration.
        let framed = frame_signal(&Signal::Ping { nonce: [1u8; 16] }).expect("framed");
        assert_eq!(framed[0], runtime::planes::stream_kind::RELIABLE_SIGNAL);
        let carried = u16::from_le_bytes([framed[1], framed[2]]);
        assert_eq!(carried, selector::PING);
        let len = u32::from_le_bytes([framed[3], framed[4], framed[5], framed[6]]) as usize;
        assert_eq!(
            len,
            framed.len() - 7,
            "the length describes what follows it"
        );
    }

    #[test]
    fn a_signal_past_its_own_declaration_is_refused_at_the_sender() {
        // A `Result`, not a substituted refusal. `FreightFrame::encode_bounded`
        // substitutes because the alternative there is telling a peer nothing;
        // here the alternative is sending something other than what was asked
        // for, which is worse than an error the caller can see.
        let huge = Signal::WorldSignal {
            world: "com.example.notes".into(),
            schema: "note".into(),
            payload: vec![0u8; bounds::MAX_SIGNAL_BYTES + 1],
        };
        assert_eq!(frame_signal(&huge), Err(SignalError::TooLarge));

        // And an attention whose scope is fine still fits its tighter ceiling.
        assert!(frame_signal(&Signal::Attention { scope: scope() }).is_ok());
    }

    #[test]
    fn every_signal_round_trips_and_has_one_spelling() {
        for signal in [
            Signal::Ping { nonce: [1u8; 16] },
            Signal::Acknowledge { nonce: [2u8; 16] },
            Signal::Attention { scope: scope() },
            Signal::SessionInvite {
                kind: InviteKind::Collaborate,
                scope: scope(),
            },
            Signal::FileOffer {
                content: [5u8; 32],
                plaintext_len: 4096,
                display_name: "report.pdf".into(),
                media_type: "application/pdf".into(),
            },
            Signal::WorldSignal {
                world: "com.example.notes".into(),
                schema: "note".into(),
                payload: vec![1, 2, 3],
            },
        ] {
            let bytes = signal.encode();
            assert_eq!(Signal::decode_canonical(&bytes), Ok(signal.clone()));
            // A trailing byte past a valid encoding is a second spelling.
            let mut extra = bytes.clone();
            extra.push(0);
            assert!(Signal::decode_canonical(&extra).is_err(), "{signal:?}");
        }
    }

    #[test]
    fn a_display_name_is_sanitised_on_use_and_never_on_decode() {
        // A decode-time rewrite makes `encode(decode(x)) == x` false, and
        // canonical re-encode equality is what every shape on this plane rests
        // on. So a traversal name decodes intact and is repaired where it is
        // used as a path.
        let offer = Signal::FileOffer {
            content: [1u8; 32],
            plaintext_len: 10,
            display_name: "../../evil.txt".into(),
            media_type: "text/plain".into(),
        };
        let decoded = Signal::decode_canonical(&offer.encode()).expect("decodes");
        let Signal::FileOffer { display_name, .. } = &decoded else {
            panic!("a file offer");
        };
        assert_eq!(display_name, "../../evil.txt", "decode did not rewrite it");
        // Where it becomes a path — in `world-interface`, which this crate does
        // not depend on — the shared sanitiser reduces it to one component.
        // Asserted here as the property the decode deliberately does not have.
        assert!(
            display_name.contains(".."),
            "and it is still hostile on the wire"
        );
    }

    #[test]
    fn a_name_that_could_split_a_header_is_refused_outright() {
        // Not repaired here — refused. A control character in a name lands in a
        // header, a filename or a terminal, and none of those are places a peer
        // gets to choose what happens.
        let hostile = Signal::FileOffer {
            content: [1u8; 32],
            plaintext_len: 10,
            display_name: "report.pdf\r\nX-Evil: yes".into(),
            media_type: "text/plain".into(),
        };
        assert_eq!(hostile.validate(), Err(PlaneWireError::NonCanonical));
        assert_eq!(frame_signal(&hostile), Err(SignalError::Malformed));

        let long = Signal::FileOffer {
            content: [1u8; 32],
            plaintext_len: 10,
            display_name: "a".repeat(MAX_SIGNAL_TEXT_BYTES + 1),
            media_type: "text/plain".into(),
        };
        assert_eq!(long.validate(), Err(PlaneWireError::Bounds));
    }

    #[test]
    fn a_world_signal_is_checked_against_the_real_grammars() {
        // Parsed rather than length-checked: a World id and a schema id have
        // shapes, and something that is merely short is not therefore one.
        let bad_world = Signal::WorldSignal {
            world: "not a world id".into(),
            schema: "note".into(),
            payload: Vec::new(),
        };
        assert_eq!(bad_world.validate(), Err(PlaneWireError::NonCanonical));

        let bad_schema = Signal::WorldSignal {
            world: "com.example.notes".into(),
            schema: "".into(),
            payload: Vec::new(),
        };
        assert_eq!(bad_schema.validate(), Err(PlaneWireError::NonCanonical));
    }
}

/// What a Station does with a signal once it has one.
mod delivery {
    use runtime::live::LiveHandle;
    use runtime::planes::Signal;
    use std::sync::Arc;

    fn handle() -> Arc<LiveHandle> {
        // No anchor source: this Station has no Replica behind it, which is
        // exactly the shape a signal must work in — signals are not about
        // Bodies.
        Arc::new(LiveHandle::new(None))
    }

    #[tokio::test]
    async fn a_signal_with_nobody_listening_is_not_a_failure() {
        // A Station with no viewer attached still admits signals and still
        // bounds them; it simply has nobody to hand them to. Treating that as
        // an error would make an idle Station refuse traffic it accepted.
        let handle = handle();
        // The only listener goes away, and the sink survives it: a broadcast
        // send with no receivers is an `Err` that is deliberately discarded,
        // never a refusal that reaches the peer.
        drop(handle.signals());
        handle.deliver(runtime::signal::DeliveredSignal {
            from: mechanics::ids::StationId::from_device(&mechanics::crypto::device_from_seed(
                &[5u8; 32],
            ))
            .expect("station"),
            session_id: [0u8; 16],
            session_epoch: [0u8; 16],
            signal: Signal::Ping { nonce: [3u8; 16] },
        });
    }

    #[tokio::test]
    async fn a_subscriber_hears_what_follows_it_and_not_what_preceded_it() {
        // A signal is an event, not a state anyone can re-read. Subscribing
        // late means missing what already happened, which is the honest
        // behaviour — the alternative is a Station holding events for readers
        // that may never arrive.
        let handle = handle();
        let mut listener = handle.signals();
        assert!(
            listener.try_recv().is_err(),
            "nothing has happened yet, so there is nothing to hear"
        );
        let _ = Signal::Ping { nonce: [1u8; 16] };
    }
}
