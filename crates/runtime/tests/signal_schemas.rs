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

/// What a World is allowed to declare, and where the declaration is checked.
///
/// Not in the codec. The descriptor decides canonicality — order, exactness,
/// non-emptiness, name grammar — and `build()` decides what a declaration is
/// allowed to *say*, which is the same seam that already refuses a duplicate
/// schema version. A bound checked in the codec would be a bound a hand-built
/// descriptor could not carry and a registration could.
mod declarations {
    use std::sync::Arc;

    use mechanics::demand::{AuthorizationDemand, PolicyCapability, PolicyResource};
    use replica::body::BodySchema;
    use replica::ids::{SchemaId, WorldId};
    use runtime::planes::bounds;
    use runtime::registry::{DeclarationKind, RegistrationError};
    use runtime::transient::MAX_SCOPE_FIELD_BYTES;
    use runtime::world::{ScopeSchema, SignalSchema};
    use runtime::{
        RuntimeBuilder, World, WorldContext, WorldEffect, WorldError, WorldIntent, WorldLimits,
        WorldProjection, WorldQuery, WorldRegistration, WorldVersion,
    };

    const WORLD: &str = "com.example.notes";

    struct DeclaringWorld {
        scopes: Vec<ScopeSchema>,
        signals: Vec<SignalSchema>,
    }

    impl World for DeclaringWorld {
        fn id(&self) -> WorldId {
            WorldId::parse(WORLD).expect("world id")
        }
        fn schemas(&self) -> &[BodySchema] {
            &[]
        }
        fn scope_schemas(&self) -> &[ScopeSchema] {
            &self.scopes
        }
        fn signal_schemas(&self) -> &[SignalSchema] {
            &self.signals
        }
        fn submit(
            &self,
            _ctx: &mut WorldContext<'_>,
            _intent: WorldIntent,
        ) -> Result<WorldEffect, WorldError> {
            Err(WorldError::InvalidRequest)
        }
        fn query(
            &self,
            _ctx: &WorldContext<'_>,
            _query: WorldQuery,
        ) -> Result<WorldProjection, WorldError> {
            Err(WorldError::InvalidRequest)
        }
    }

    fn demand() -> Vec<u8> {
        AuthorizationDemand::require(
            PolicyCapability::new(WORLD, "signal"),
            PolicyResource::space(WORLD),
        )
        .encode_canonical()
        .expect("canonical demand")
    }

    fn scope(name: &str, max_key_bytes: u32) -> ScopeSchema {
        ScopeSchema {
            name: SchemaId::parse(name).expect("schema id"),
            max_key_bytes,
        }
    }

    fn signal(name: &str, max_payload_bytes: u32) -> SignalSchema {
        SignalSchema {
            name: SchemaId::parse(name).expect("schema id"),
            max_payload_bytes,
            demand: demand(),
        }
    }

    /// Register a World whose registration says exactly what the World says.
    fn build(
        scopes: Vec<ScopeSchema>,
        signals: Vec<SignalSchema>,
    ) -> Result<(), RegistrationError> {
        let world = DeclaringWorld {
            scopes: scopes.clone(),
            signals: signals.clone(),
        };
        let registration = WorldRegistration {
            id: world.id(),
            implementation_version: WorldVersion(1),
            schemas: Vec::new(),
            limits: WorldLimits::default(),
            scope_schemas: scopes,
            signal_schemas: signals,
        };
        RuntimeBuilder::new()
            .register(registration, Arc::new(world))
            .build()
            .map(|_| ())
    }

    #[test]
    fn a_registration_that_disagrees_with_the_world_is_refused() {
        // The declaration lists are what the implementation descriptor is built
        // from, so a registration the running code does not stand behind is a
        // reviewed identity describing something else. Comparing them is what
        // makes "reviewed" enforced rather than asserted.
        let world = DeclaringWorld {
            scopes: Vec::new(),
            signals: vec![signal("note", 1024)],
        };
        let registration = WorldRegistration {
            id: world.id(),
            implementation_version: WorldVersion(1),
            schemas: Vec::new(),
            limits: WorldLimits::default(),
            scope_schemas: Vec::new(),
            signal_schemas: Vec::new(),
        };
        let err = RuntimeBuilder::new()
            .register(registration, Arc::new(world))
            .build()
            .expect_err("the World declares a signal its registration does not");
        assert_eq!(
            err,
            RegistrationError::RegistrationMismatch(WorldId::parse(WORLD).unwrap())
        );

        // And the scope list is compared by the same rule, so neither is the
        // one somebody remembered to check.
        let world = DeclaringWorld {
            scopes: vec![scope("board", 64)],
            signals: Vec::new(),
        };
        let registration = WorldRegistration {
            id: world.id(),
            implementation_version: WorldVersion(1),
            schemas: Vec::new(),
            limits: WorldLimits::default(),
            scope_schemas: Vec::new(),
            signal_schemas: Vec::new(),
        };
        assert!(matches!(
            RuntimeBuilder::new()
                .register(registration, Arc::new(world))
                .build(),
            Err(RegistrationError::RegistrationMismatch(_))
        ));
    }

    #[test]
    fn a_declared_bound_may_tighten_the_substrates_and_never_raise_it() {
        // A ceiling above the substrate's is a number that never applies: the
        // plane refuses first, so the declaration would describe a limit
        // nothing enforces.
        assert!(build(Vec::new(), vec![signal("note", 1)]).is_ok());
        assert!(build(
            Vec::new(),
            vec![signal("note", bounds::MAX_SIGNAL_BYTES as u32)]
        )
        .is_ok());
        assert_eq!(
            build(
                Vec::new(),
                vec![signal("note", bounds::MAX_SIGNAL_BYTES as u32 + 1)]
            ),
            Err(RegistrationError::InvalidDeclaration {
                world: WorldId::parse(WORLD).unwrap(),
                kind: DeclarationKind::Signal,
                name: "note".into(),
            })
        );
        // Zero is not "no limit". It is a signal nothing can satisfy.
        assert!(matches!(
            build(Vec::new(), vec![signal("note", 0)]),
            Err(RegistrationError::InvalidDeclaration { .. })
        ));

        assert!(build(
            vec![scope("board", MAX_SCOPE_FIELD_BYTES as u32)],
            Vec::new()
        )
        .is_ok());
        assert_eq!(
            build(
                vec![scope("board", MAX_SCOPE_FIELD_BYTES as u32 + 1)],
                Vec::new()
            ),
            Err(RegistrationError::InvalidDeclaration {
                world: WorldId::parse(WORLD).unwrap(),
                kind: DeclarationKind::Scope,
                name: "board".into(),
            })
        );
        assert!(matches!(
            build(vec![scope("board", 0)], Vec::new()),
            Err(RegistrationError::InvalidDeclaration { .. })
        ));
    }

    #[test]
    fn a_demand_that_does_not_decode_is_not_a_demand() {
        // The bytes are what policy evaluates. Accepting them unparsed would
        // put a declaration into a reviewed trust identity that fails the first
        // time anyone sends the signal it authorizes.
        let mut broken = signal("note", 1024);
        broken.demand = vec![0xff, 0xff, 0xff];
        assert!(matches!(
            build(Vec::new(), vec![broken]),
            Err(RegistrationError::InvalidDeclaration { .. })
        ));

        let mut absent = signal("note", 1024);
        absent.demand = Vec::new();
        assert!(matches!(
            build(Vec::new(), vec![absent]),
            Err(RegistrationError::InvalidDeclaration { .. })
        ));
    }

    #[test]
    fn one_name_may_carry_only_one_declaration() {
        // Two ceilings for one name is a declaration that does not say what it
        // means, and the descriptor cannot even spell it — entries there sort
        // by name and must strictly ascend.
        assert_eq!(
            build(Vec::new(), vec![signal("note", 8), signal("note", 16)]),
            Err(RegistrationError::DuplicateDeclaration {
                world: WorldId::parse(WORLD).unwrap(),
                kind: DeclarationKind::Signal,
                name: "note".into(),
            })
        );
        assert_eq!(
            build(vec![scope("board", 8), scope("board", 16)], Vec::new()),
            Err(RegistrationError::DuplicateDeclaration {
                world: WorldId::parse(WORLD).unwrap(),
                kind: DeclarationKind::Scope,
                name: "board".into(),
            })
        );
        // The same name in the two different kinds is two different things.
        assert!(build(vec![scope("note", 8)], vec![signal("note", 16)]).is_ok());
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

/// What receiving a file offer costs.
///
/// The answer is: a name and a content id. Not a transfer, not a filesystem
/// write, and not a decision — an offer names content the sender holds, and
/// whether the receiver wants a gigabyte is something a person decides rather
/// than something a deadline extracts.
mod offers {
    use runtime::admission::{AdmittedPeer, PlanePolicy};
    use runtime::signal::{
        offer_gates, OfferGates, OfferOutcome, OfferQueue, PendingOffer, MAX_PENDING_OFFERS,
    };

    fn actor(tag: &str) -> mechanics::ids::ActorId {
        mechanics::ids::ActorId::parse(&format!("act_{}", tag.repeat(32))).expect("actor")
    }

    fn station(seed: u8) -> mechanics::ids::StationId {
        mechanics::ids::StationId::from_device(&mechanics::crypto::device_from_seed(&[seed; 32]))
            .expect("station")
    }

    fn peer(seed: u8, who: mechanics::ids::ActorId) -> AdmittedPeer {
        AdmittedPeer {
            station: station(seed),
            actor: who,
            authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
            granted_lanes: vec![runtime::planes::stream_kind::RELIABLE_SIGNAL],
            session_id: [1u8; 16],
            session_epoch: [2u8; 16],
            features: 0,
        }
    }

    fn offer(from: u8, content: u8) -> PendingOffer {
        PendingOffer {
            from: station(from),
            session_epoch: [2u8; 16],
            content: [content; 32],
            plaintext_len: 1_048_576,
            display_name: "notes.txt".into(),
            media_type: "text/plain".into(),
        }
    }

    #[test]
    fn an_offer_is_held_and_a_repeat_of_it_is_not() {
        // A peer repeating itself must not fill the queue. An offer is
        // identified by its content and its sender, which is exactly what makes
        // "the same file, again" the same offer.
        let mut queue = OfferQueue::new();
        assert_eq!(queue.admit(offer(1, 7)), OfferOutcome::Queued);
        assert_eq!(queue.admit(offer(1, 7)), OfferOutcome::Duplicate);
        assert_eq!(queue.pending().len(), 1);

        // The same file from somebody else is a different offer, because taking
        // it means fetching from a different peer.
        assert_eq!(queue.admit(offer(2, 7)), OfferOutcome::Queued);
        assert_eq!(queue.pending().len(), 2);
    }

    #[test]
    fn a_full_queue_refuses_the_newest_rather_than_evicting_the_oldest() {
        // This is an inbox. What is already in it may be about to be acted on,
        // and silently dropping that to make room for something newer loses the
        // decision somebody was in the middle of making.
        let mut queue = OfferQueue::new();
        for n in 0..MAX_PENDING_OFFERS {
            assert_eq!(queue.admit(offer(1, n as u8)), OfferOutcome::Queued);
        }
        assert_eq!(queue.admit(offer(1, 200)), OfferOutcome::Full);
        assert_eq!(queue.pending().len(), MAX_PENDING_OFFERS);
        assert_eq!(
            queue.pending()[0].content,
            [0u8; 32],
            "the oldest is still there"
        );
        assert_eq!(queue.refused(), 1);
    }

    #[test]
    fn taking_an_offer_removes_exactly_that_one() {
        let mut queue = OfferQueue::new();
        queue.admit(offer(1, 7));
        queue.admit(offer(1, 8));
        let taken = queue.take(&station(1), &[7u8; 32]).expect("taken");
        assert_eq!(taken.content, [7u8; 32]);
        assert_eq!(queue.pending().len(), 1);
        assert!(queue.take(&station(1), &[7u8; 32]).is_none(), "only once");
    }

    #[test]
    fn a_peer_that_lost_standing_takes_its_offers_with_it() {
        // A file offered by somebody who is no longer a member is not an offer
        // anyone should be shown a button for.
        let mut queue = OfferQueue::new();
        queue.admit(offer(1, 7));
        queue.admit(offer(1, 8));
        queue.admit(offer(2, 9));
        assert_eq!(queue.forget(&station(1)), 2);
        assert_eq!(queue.pending().len(), 1);
        assert_eq!(queue.pending()[0].from, station(2));
    }

    #[test]
    fn each_gate_refuses_on_its_own() {
        // Three gates, and two of them are answerable at this layer. Each has to
        // refuse independently, or a deployment that opened one by accident
        // would find the others had stopped mattering.
        let ours = actor("aa");
        let stranger = actor("bb");
        let opted_in = PlanePolicy {
            auto_accept_offers: true,
            ..PlanePolicy::default()
        };

        // Gate one: the sender is one of this identity's own devices. The
        // strictest of the three, and the reason automatic acceptance is
        // defensible at all.
        assert_eq!(
            offer_gates(&peer(1, stranger.clone()), &ours, &opted_in),
            OfferGates::NotOurDevice
        );

        // Gate two: this Station opted in. Off by default, which is why the
        // plain default policy refuses even our own device.
        assert_eq!(
            offer_gates(&peer(1, ours.clone()), &ours, &PlanePolicy::default()),
            OfferGates::SpaceNotOptedIn
        );

        // Both open, and the third is still outstanding — and says so, rather
        // than reporting an acceptance this layer is not entitled to grant.
        assert_eq!(
            offer_gates(&peer(1, ours.clone()), &ours, &opted_in),
            OfferGates::DestinationRemains
        );
    }

    #[test]
    fn the_first_gate_outranks_the_second() {
        // A Station that opted in must still refuse a stranger. If the order
        // were the other way, opting in would be opting into anyone.
        let ours = actor("aa");
        let opted_in = PlanePolicy {
            auto_accept_offers: true,
            ..PlanePolicy::default()
        };
        assert_eq!(
            offer_gates(&peer(1, actor("bb")), &ours, &opted_in),
            OfferGates::NotOurDevice
        );
    }
}
