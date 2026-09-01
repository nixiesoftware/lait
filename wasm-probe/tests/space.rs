//! The 4b claim, whole: a browser worker joins a real Space as a
//! pre-admitted member device and pulls it over `lait/contact/2` — real
//! ticket, real ledger and replica on real OPFS, real transport through a
//! local relay, the production Contact grammar frame for frame — from a
//! native daemon holding real issue data.
//!
//! The harness (`ci/browser-live-space.sh`) founds the Space, writes issues,
//! admits a scratch second daemon whose seed IT chose, stops that daemon
//! (one DeviceId, one holder), and bakes the rendezvous in at compile time:
//! the invite ticket, the admitted seed, the relay, the approach peer.
//!
//! The browser side is deliberately a *member device pull*, never a second
//! self-inception: the seed already maps to an admitted actor in the
//! replicated ledger, and re-entering would mint a different actor.

#![cfg(all(target_arch = "wasm32", feature = "probe-contact"))]

use std::sync::Arc;

use contact::authority::{LedgerAuthority, SharedLedgerAuthority};
use contact::coordinates::SignedCoordinates;
use contact::pull::{pull_whole, Deadlines};
use comms::policy::{LocalNet, Network};
use comms::{DefaultFactory, Protocols, TransportFactory};
use journal::OpfsMedium;
use mechanics::ids::ActorId;
use mechanics::space::{Authority, Effect, Genesis};
use mechanics::station::Key;
use replica::Replica;
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};

wasm_bindgen_test_configure!(run_in_dedicated_worker);

fn unhex32(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex seed");
    }
    out
}

fn unique_dir(tag: &str) -> String {
    let mut noise = [0u8; 8];
    let _ = getrandom03::fill(&mut noise);
    let hex: String = noise.iter().map(|b| format!("{b:02x}")).collect();
    format!("space-{tag}-{hex}")
}

#[wasm_bindgen_test]
async fn a_browser_member_pulls_a_real_space_over_contact() {
    let relay = option_env!("LIVE_RELAY_URL").expect("harness sets LIVE_RELAY_URL");
    let seed = unhex32(option_env!("LIVE_SEED_HEX").expect("harness sets LIVE_SEED_HEX"));
    let ticket = option_env!("LIVE_TICKET").expect("harness sets LIVE_TICKET");
    let expect_bodies: u64 = option_env!("LIVE_EXPECT_BODIES")
        .expect("harness sets LIVE_EXPECT_BODIES")
        .parse()
        .expect("a count");

    // The ticket is the whole public bundle: space identity, genesis
    // material, and whom to approach.
    let coordinates = SignedCoordinates::parse_link(ticket).expect("ticket parses");
    let verified = coordinates.verify().expect("ticket verifies");
    let space = verified.space.clone();
    let founder_inception: mechanics::actor::SignedEvent =
        postcard::from_bytes(&coordinates.payload.founder_inception)
            .expect("founder inception decodes");
    let genesis = Genesis {
        space_id: space.clone(),
        founding_actors: vec![ActorId::from_incept_hash(&founder_inception.hash())],
        salt: coordinates.payload.salt,
        recovery_root: coordinates.payload.recovery_root,
    };

    // The member device's whole world, on real browser storage.
    let ledger_medium = OpfsMedium::open(&unique_dir("ledger")).await.expect("opfs");
    let mut ledger =
        Authority::create_on(Arc::new(ledger_medium), genesis).expect("fresh ledger");
    ledger
        .commit_batch(&[Effect::Actor(founder_inception).encode()], &[])
        .expect("founder inception lands");
    let authority = SharedLedgerAuthority::new(LedgerAuthority::new(space.clone(), ledger, seed));
    let replica_medium = OpfsMedium::open(&unique_dir("replica")).await.expect("opfs");
    let mut replica = Replica::open_on(Arc::new(replica_medium), Arc::new(authority.clone()))
        .expect("fresh replica");

    // The transport, exactly as 4a proved it: relay-only, learn-then-dial.
    let network = Network::Local(LocalNet {
        relays: vec![relay.to_owned()],
    });
    let transport = DefaultFactory
        .build(&seed, &network, Protocols::framed(&[]))
        .await
        .expect("browser endpoint");
    let responder = Key::from_key_bytes(coordinates.payload.approach_station);
    transport.learn(responder.as_device(), &[]);

    let bundle = authority.bundle();
    let outcome = pull_whole(
        transport.as_ref(),
        &responder,
        &space,
        &seed,
        &bundle,
        &mut replica,
        Deadlines::default(),
    )
    .await
    .expect("the pull completes");
    assert!(outcome.bytes_moved > 0, "material moved");

    // The pulled ledger admitted us: the keyring can unseal, the replica
    // holds the Space's bodies, and the manifest root is published.
    assert!(
        replica.body_count() >= expect_bodies,
        "pulled {} bodies, expected at least {expect_bodies}",
        replica.body_count()
    );
    let root = replica.published_root().expect("a published root");

    // Convergence's own idempotence check: pulling again moves the grammar,
    // changes nothing, and the root stands.
    let second = pull_whole(
        transport.as_ref(),
        &responder,
        &space,
        &seed,
        &bundle,
        &mut replica,
        Deadlines::default(),
    )
    .await
    .expect("the second pull completes");
    let _ = second;
    assert_eq!(
        replica.published_root().expect("still published"),
        root,
        "a repeated pull is idempotent"
    );
}
