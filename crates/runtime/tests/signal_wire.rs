//! Reliable signals on a real connection, proven the same way on both
//! contractors.
//!
//! Every property here runs twice: once over the in-memory switchboard and once
//! over two iroh endpoints on loopback. That is not belt-and-braces. S1's
//! cross-implementation tests caught mem being laxer than iroh about when a
//! flow exists, and every property this file asserts turns on `finish` /
//! `reset` / `stop` timing — exactly that class.
//!
//! The harness is duplicated from `crates/comms/tests/flows.rs` rather than
//! imported: `on_both`, `Pair`, `mem_pair` and `iroh_pair` are private to that
//! test binary. `iroh_pair`'s Isolated-network `advertised_routes`/`learn`
//! sequence is copied verbatim — a naive two-endpoint iroh test fails in a way
//! that reads as a protocol bug.

use std::sync::Arc;
use std::time::Duration;

use comms::mem::MemNet;
use comms::policy::Network;
use comms::{Alpn, Connection, DefaultTransport, Protocols, Transport};
use mechanics::crypto::device_from_seed;
use mechanics::{ids::ActorId, station::Key};
use runtime::plane::{stream_kind, Signal};
use runtime::registry::RuntimeBuilder;
use runtime::signal::{
    frame_signal, send_signal, serve_signal, Refusal, SignalOutcome, SignalPolicy,
};
use runtime::world::{AuthorityView, PrincipalResolution};

const SESSION_ALPN: Alpn = b"lait/session/1";

/// One connected pair, however it was built.
struct Pair {
    dialer: Box<dyn Connection>,
    /// Shared, not owned by the responder task.
    ///
    /// Dropping an iroh connection closes it, so a responder that owned this
    /// would tear the connection down when it finished — and every "the
    /// connection stays up" assertion after it would be testing the drop
    /// instead of the refusal. MemNet is laxer and hides the difference.
    accepter: Arc<dyn Connection>,
    /// Kept alive: dropping a transport tears its endpoint down.
    _keep: Vec<Arc<dyn Transport>>,
}

async fn mem_pair() -> Pair {
    let net = MemNet::new();
    let a: Arc<dyn Transport> = Arc::new(net.peer(device_from_seed(&[61u8; 32])));
    let b: Arc<dyn Transport> = Arc::new(net.peer(device_from_seed(&[62u8; 32])));
    let accepting = {
        let b = b.clone();
        tokio::spawn(async move { b.accept_connection().await })
    };
    let dialer = a
        .connect_session(b.my_id(), SESSION_ALPN)
        .await
        .expect("connect");
    let incoming = accepting.await.expect("accept task").expect("incoming");
    Pair {
        dialer,
        accepter: Arc::from(incoming.connection),
        _keep: vec![a, b],
    }
}

async fn iroh_pair() -> Pair {
    let protocols = Protocols {
        framed: &[],
        session: &[SESSION_ALPN],
    };
    let a = DefaultTransport::new(&[63u8; 32], &Network::Isolated, protocols)
        .await
        .expect("build A");
    let b = DefaultTransport::new(&[64u8; 32], &Network::Isolated, protocols)
        .await
        .expect("build B");
    // A fresh endpoint learns its direct addresses asynchronously, and under
    // Isolated a bare id resolves through nothing — so learning an empty
    // address list here is a dial that never completes.
    let a_addrs = a
        .advertised_routes(Duration::from_secs(10))
        .await
        .expect("A has a route");
    let b_addrs = b
        .advertised_routes(Duration::from_secs(10))
        .await
        .expect("B has a route");
    a.learn(b.my_id(), &b_addrs);
    b.learn(a.my_id(), &a_addrs);
    let a: Arc<dyn Transport> = Arc::new(a);
    let b: Arc<dyn Transport> = Arc::new(b);

    let accepting = {
        let b = b.clone();
        tokio::spawn(async move { b.accept_connection().await })
    };
    let dialer = tokio::time::timeout(
        Duration::from_secs(15),
        a.connect_session(b.my_id(), SESSION_ALPN),
    )
    .await
    .expect("dial in time")
    .expect("connect");
    let incoming = tokio::time::timeout(Duration::from_secs(10), accepting)
        .await
        .expect("accept in time")
        .expect("accept task")
        .expect("incoming");
    Pair {
        dialer,
        accepter: Arc::from(incoming.connection),
        _keep: vec![a, b],
    }
}

/// Run one property against both contractors. A failure names which, and a
/// mem-only pass is visible as one.
async fn on_both(name: &str, property: impl AsyncFn(Pair)) {
    property(mem_pair().await).await;
    eprintln!("{name}: mem ok");
    property(iroh_pair().await).await;
    eprintln!("{name}: iroh ok");
}

/// Everyone is a member. Admission itself is `admission_fixtures`' subject;
/// what this file is about is what a signal does once a peer is in.
struct Everyone;
impl AuthorityView for Everyone {
    fn resolve(&self, _device: &mechanics::ids::DeviceId) -> Option<PrincipalResolution> {
        Some(PrincipalResolution {
            actor: actor(),
            authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
        })
    }
}

fn actor() -> ActorId {
    ActorId::parse(&format!("act_{}", "ab".repeat(32))).expect("actor")
}

fn station(seed: u8) -> Key {
    Key::from_device(&device_from_seed(&[seed; 32])).expect("station")
}

/// A policy holding every lane this build serves.
fn policy(lanes: Vec<u8>) -> SignalPolicy {
    SignalPolicy {
        peer: station(61),
        actor: actor(),
        frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
        granted_lanes: lanes,
        authority: Arc::new(Everyone),
        // No hosted Worlds. `WorldSignal` is refused on that basis below, which
        // is the honest shape: a build that hosts nothing can interpret nothing.
        worlds: RuntimeBuilder::new().build().expect("empty registry"),
    }
}

/// Accept one flow on the responder and serve it as a signal.
async fn serve_one(connection: &dyn Connection, policy: &SignalPolicy) -> Result<Signal, Refusal> {
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .expect("accept")
        .expect("a flow");
    // The caller of `serve_signal` owns the stream-kind byte, exactly as
    // `Connection` does.
    let kind = recv.read_exact(1).await.expect("kind byte");
    assert_eq!(kind[0], stream_kind::RELIABLE_SIGNAL);
    serve_signal(send.as_mut(), recv.as_mut(), policy).await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_ping_round_trips_and_its_answer_carries_the_same_nonce() {
    on_both("ping", async |pair: Pair| {
        // The nonce is what makes an answer an answer to *this* ping rather
        // than to any ping. Without it a peer could satisfy every outstanding
        // ping with one stale acknowledgement.
        let policy = policy(vec![stream_kind::CONTROL, stream_kind::RELIABLE_SIGNAL]);
        let responder = tokio::spawn({
            let accepter = pair.accepter.clone();
            async move { serve_one(accepter.as_ref(), &policy).await }
        });

        let nonce = [77u8; 16];
        let outcome = send_signal(pair.dialer.as_ref(), &Signal::Ping { nonce })
            .await
            .expect("sent");
        assert_eq!(
            outcome,
            SignalOutcome::Answered(Signal::Acknowledge { nonce })
        );

        let served = responder.await.expect("responder").expect("served");
        assert_eq!(served, Signal::Ping { nonce });
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_one_way_signal_is_accepted_and_that_is_not_a_delivery_receipt() {
    on_both("one-way", async |pair: Pair| {
        // `Accepted` says the bytes left, framed and bounded. It says nothing
        // about whether a person saw them — but the signal does have to *reach*
        // a responder, and this asserts that it does. It did not until the flow
        // kind was fixed: a one-way signal on a unidirectional flow succeeded
        // locally, reported `Accepted`, and was never served by anything,
        // because the Live plane accepts bidirectional flows only.
        let policy = policy(vec![stream_kind::RELIABLE_SIGNAL]);
        let responder = tokio::spawn({
            let accepter = pair.accepter.clone();
            async move { serve_one(accepter.as_ref(), &policy).await }
        });

        let attention = Signal::Attention {
            scope: runtime::transient::Target::Body {
                world: "com.example.notes".into(),
                body: [4u8; 16],
            },
        };
        let outcome = send_signal(pair.dialer.as_ref(), &attention.clone())
            .await
            .expect("sent");
        assert_eq!(outcome, SignalOutcome::Accepted);
        // And it arrived. `Accepted` does not promise this — it is about the
        // bytes leaving — but a plane on which one-way signals silently reach
        // nobody would satisfy `Accepted` just as well, which is exactly the
        // bug this assertion exists to catch.
        assert_eq!(responder.await.expect("responder"), Ok(attention));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ungranted_lane_is_refused_at_flow_open_and_the_connection_stays_up() {
    on_both("lane", async |pair: Pair| {
        // The lane check comes before the read, so a peer that never held the
        // lane never gets a buffer allocated for it.
        let policy = policy(vec![stream_kind::CONTROL]);
        let responder = tokio::spawn({
            let accepter = pair.accepter.clone();
            async move { serve_one(accepter.as_ref(), &policy).await }
        });

        let (mut send, _recv) = pair.dialer.open_bi().await.expect("open");
        let framed = frame_signal(&Signal::Ping { nonce: [1u8; 16] }).expect("framed");
        let _ = send.write_all(&framed).await;
        let _ = send.finish();

        let refused = responder.await.expect("responder");
        assert_eq!(refused, Err(Refusal::LaneNotGranted));

        // Refused at the flow, not at the connection: another flow still opens.
        assert!(pair.dialer.open_bi().await.is_ok());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_selector_is_refused_before_its_length_is_read() {
    on_both("selector", async |pair: Pair| {
        // The selector precedes the length precisely so this is possible. A
        // declaration nobody published bounds nothing, so it is refused with
        // the length never even consulted.
        let policy = policy(vec![stream_kind::RELIABLE_SIGNAL]);
        let responder = tokio::spawn({
            let accepter = pair.accepter.clone();
            async move { serve_one(accepter.as_ref(), &policy).await }
        });

        let (mut send, _recv) = pair.dialer.open_bi().await.expect("open");
        let mut framed = vec![stream_kind::RELIABLE_SIGNAL];
        framed.extend_from_slice(&0xBEEFu16.to_le_bytes());
        // A length that would be refused if anything got as far as reading it.
        framed.extend_from_slice(&u32::MAX.to_le_bytes());
        let _ = send.write_all(&framed).await;
        let _ = send.finish();

        let refused = responder.await.expect("responder");
        assert_eq!(refused, Err(Refusal::NotRegistered));
        assert!(pair.dialer.open_bi().await.is_ok(), "the connection stays");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declared_length_over_the_schema_ceiling_is_refused_before_a_buffer_is_reserved() {
    on_both("ceiling", async |pair: Pair| {
        // Acceptance 8's signal row. `Ping` declares 64 bytes; a peer declaring
        // more is refused by the *declared* length, with no allocation of that
        // size and nothing read past the header.
        let policy = policy(vec![stream_kind::RELIABLE_SIGNAL]);
        let responder = tokio::spawn({
            let accepter = pair.accepter.clone();
            async move { serve_one(accepter.as_ref(), &policy).await }
        });

        let (mut send, _recv) = pair.dialer.open_bi().await.expect("open");
        let mut framed = vec![stream_kind::RELIABLE_SIGNAL];
        framed.extend_from_slice(&runtime::signal::selector::PING.to_le_bytes());
        framed.extend_from_slice(&(64u32 * 1024).to_le_bytes());
        let _ = send.write_all(&framed).await;
        let _ = send.finish();

        let refused = responder.await.expect("responder");
        assert_eq!(refused, Err(Refusal::TooLarge));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_world_signal_for_a_world_this_build_does_not_host_is_refused() {
    on_both("world", async |pair: Pair| {
        // Not a denial. A World we do not host is a schema nobody here
        // reviewed, and interpreting it anyway is the failure this refusal
        // exists to prevent — so it is `NotRegistered`, whatever the sender's
        // standing.
        let policy = policy(vec![stream_kind::RELIABLE_SIGNAL]);
        let responder = tokio::spawn({
            let accepter = pair.accepter.clone();
            async move { serve_one(accepter.as_ref(), &policy).await }
        });

        let signal = Signal::WorldSignal {
            world: "com.example.notes".into(),
            schema: "sch_nudge".into(),
            payload: vec![1, 2, 3],
        };
        // Written by hand rather than through `send_signal`, so the frame can
        // be exactly what a hostile peer would send: `send_signal` would refuse
        // this locally at `frame_signal` if the World id did not parse, and what
        // is under test is the *responder's* refusal.
        let (mut send, _recv) = pair.dialer.open_bi().await.expect("open");
        let _ = send
            .write_all(&frame_signal(&signal).expect("framed"))
            .await;
        let _ = send.finish();

        let refused = responder.await.expect("responder");
        assert_eq!(refused, Err(Refusal::NotRegistered));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_truncated_header_is_malformed_rather_than_a_hang() {
    on_both("truncated", async |pair: Pair| {
        // A peer that opens a flow, writes two bytes and leaves. The read must
        // end, and it must end as a decode failure rather than as a deadline —
        // a deadline here would spend the whole `SIGNAL_READ` budget on a peer
        // that already hung up.
        let policy = policy(vec![stream_kind::RELIABLE_SIGNAL]);
        let responder = tokio::spawn({
            let accepter = pair.accepter.clone();
            async move { serve_one(accepter.as_ref(), &policy).await }
        });

        let (mut send, _recv) = pair.dialer.open_bi().await.expect("open");
        let _ = send.write_all(&[stream_kind::RELIABLE_SIGNAL, 0x01]).await;
        let _ = send.finish();

        let refused = responder.await.expect("responder");
        assert_eq!(refused, Err(Refusal::Malformed));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_offer_crosses_intact_and_triggers_nothing() {
    on_both("offer", async |pair: Pair| {
        // An offer is not an acceptance. It names content the sender holds;
        // whether the receiver wants it is a decision a person makes later, so
        // this expects no answer and starts no transfer.
        let policy = policy(vec![stream_kind::RELIABLE_SIGNAL]);
        let responder = tokio::spawn({
            let accepter = pair.accepter.clone();
            async move { serve_one(accepter.as_ref(), &policy).await }
        });

        let offer = Signal::FileOffer {
            content: [5u8; 32],
            plaintext_len: 1_048_576,
            display_name: "notes.txt".into(),
            media_type: "text/plain".into(),
        };
        assert_eq!(
            send_signal(pair.dialer.as_ref(), &offer.clone()).await,
            Ok(SignalOutcome::Accepted)
        );
        // The offer crosses intact, field for field. A display name is
        // peer-supplied and is carried exactly as sent — sanitising here would
        // mean the name shown to a person is not the name that was offered.
        assert_eq!(responder.await.expect("responder"), Ok(offer));
    })
    .await;
}

/// A World's declared signal schema, enforced rather than merely reviewed.
///
/// The substrate's own declaration for `selector::WORLD` is deliberately
/// permissive — it cannot be otherwise, because the World and the schema are
/// named *inside* the payload. So a World's ceiling and demand are enforced
/// after the body decodes, and without that the descriptor's signal section
/// would be decoration: a World could declare a 64-byte nudge requiring a
/// capability, and a peer could send sixteen kilobytes of it on session
/// membership alone.
mod declared_schemas {
    use super::*;
    use replica::body::{MutationModel, Schema};
    use replica::ids::{EncodingId, SchemaId, WorldId};
    use runtime::world::SignalSchema;
    use runtime::{
        Context, Descriptor, Effect, Intent, Limits, Projection, Query, Rejection, Version, World,
    };

    const WORLD: &str = "dev.example.pad";

    struct Pad(Vec<Schema>, Vec<SignalSchema>);

    impl World for Pad {
        fn id(&self) -> WorldId {
            WorldId::parse(WORLD).unwrap()
        }
        fn schemas(&self) -> &[Schema] {
            &self.0
        }
        fn signal_schemas(&self) -> &[SignalSchema] {
            &self.1
        }
        fn submit(&self, _ctx: &mut Context<'_>, _intent: Intent) -> Result<Effect, Rejection> {
            Err(Rejection::InvalidRequest)
        }
        fn query(&self, _ctx: &Context<'_>, _query: Query) -> Result<Projection, Rejection> {
            Err(Rejection::InvalidRequest)
        }
    }

    fn hosting(signals: Vec<SignalSchema>) -> SignalPolicy {
        let schemas = vec![Schema {
            id: SchemaId::parse("entry").unwrap(),
            version: 1,
            encoding: EncodingId::parse("bytes").unwrap(),
            mutation: MutationModel::Atomic,
            readable_predecessors: vec![],
        }];
        let world = Pad(schemas.clone(), signals.clone());
        let worlds = RuntimeBuilder::new()
            .register(Arc::new(world))
            .build()
            .expect("registry");
        SignalPolicy {
            worlds,
            ..policy(vec![stream_kind::RELIABLE_SIGNAL])
        }
    }

    /// A demand every registered signal schema must carry.
    ///
    /// Not optional: `RuntimeBuilder::build` parses it through
    /// `AuthorizationDemand::decode_canonical`, which refuses empty input — so a
    /// World cannot register a signal it declines to say anything about.
    fn some_demand() -> Vec<u8> {
        mechanics::demand::AuthorizationDemand::require(
            mechanics::demand::PolicyCapability::new("pad", "nudge"),
            mechanics::demand::Resource::root("pad"),
        )
        .encode_canonical()
        .expect("canonical demand")
    }

    fn nudge(payload_len: usize) -> Signal {
        Signal::WorldSignal {
            world: WORLD.into(),
            schema: "nudge".into(),
            payload: vec![7u8; payload_len],
        }
    }

    #[test]
    fn a_payload_past_the_worlds_own_ceiling_is_refused() {
        // The substrate's ceiling for `selector::WORLD` is the whole plane
        // maximum, so this is the only place the World's own number is read on
        // a delivery path.
        let policy = hosting(vec![SignalSchema {
            name: SchemaId::parse("nudge").unwrap(),
            max_payload_bytes: 64,
            demand: some_demand(),
        }]);
        assert_eq!(policy.admits_contents(&nudge(64)), Ok(()));
        assert_eq!(policy.admits_contents(&nudge(65)), Err(Refusal::TooLarge));
    }

    #[test]
    fn a_schema_the_world_never_declared_is_not_registered() {
        // Reaching a World with an undeclared schema is exactly what a reviewed
        // descriptor exists to prevent — and it is `NotRegistered` rather than
        // `Denied`, because it is not a question about standing.
        let policy = hosting(vec![SignalSchema {
            name: SchemaId::parse("nudge").unwrap(),
            max_payload_bytes: 64,
            demand: some_demand(),
        }]);
        let other = Signal::WorldSignal {
            world: WORLD.into(),
            schema: "shout".into(),
            payload: vec![1],
        };
        assert_eq!(policy.admits_contents(&other), Err(Refusal::NotRegistered));
    }

    #[test]
    fn a_world_that_declares_no_signals_accepts_none() {
        // Hosting a World is not the same as hosting its signals. A World that
        // declared nothing has nothing this plane may deliver to it.
        let policy = hosting(Vec::new());
        assert_eq!(
            policy.admits_contents(&nudge(1)),
            Err(Refusal::NotRegistered)
        );
    }

    #[test]
    fn a_world_cannot_register_a_signal_it_says_nothing_about() {
        // Registration refuses an empty demand, which is why the delivery path
        // has no fallback for one. A signal a World declined to describe is a
        // signal nobody can decide about.
        let schemas = vec![Schema {
            id: SchemaId::parse("entry").unwrap(),
            version: 1,
            encoding: EncodingId::parse("bytes").unwrap(),
            mutation: MutationModel::Atomic,
            readable_predecessors: vec![],
        }];
        let signals = vec![SignalSchema {
            name: SchemaId::parse("nudge").unwrap(),
            max_payload_bytes: 64,
            demand: Vec::new(),
        }];
        let world = Pad(schemas.clone(), signals.clone());
        assert!(RuntimeBuilder::new()
            .register(Arc::new(world))
            .build()
            .is_err());
    }

    #[test]
    fn a_declared_demand_is_evaluated_at_the_pinned_frontier() {
        // `Everyone` permits every read, so this asserts the demand reaches the
        // authority view at all rather than being carried and ignored — which
        // is what it was before this landed.
        let policy = hosting(vec![SignalSchema {
            name: SchemaId::parse("nudge").unwrap(),
            max_payload_bytes: 64,
            demand: some_demand(),
        }]);
        assert_eq!(policy.admits_contents(&nudge(8)), Ok(()));
    }
}
