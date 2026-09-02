//! S7.3, the engine crossing: a REAL Call through the REAL engine, entirely
//! in a browser. The daemon's own Station machinery — `runtime::browser`
//! composes the same `StationCore`/`Session`/dock path the native lifecycle
//! runs — queries the real Issues runner under the browser's own WebAssembly,
//! and the runner's semantic read comes BACK across the ABI as
//! `host_call("context.read_body")` / `context.find`, answered by Runtime's
//! `Context` over a Worker-owned Replica.
//!
//! The Replica is deliberately empty: the claim here is the whole read path —
//! dock (principal facts, activation, publication build with real extract
//! calls into the guest), query dispatch into the guest, the read callback
//! into the snapshot, and Runtime's stamping of the projection — not the
//! data. The authority is the trait's documented fixture posture (grants
//! everything, activates the runner's reviewed implementation); the
//! pulled-ledger authority (`runtime::browser::LedgerAuthorityView`) rides
//! the browser-live-space harness, where activation and capability grants
//! arrive already-signed in a real pulled ledger.
//!
//! Runs under `wasm-pack test --headless --chrome --test call`.
//! Skips honestly when the harness did not build the runner.

#![cfg(all(target_arch = "wasm32", feature = "probe-engine", issues_runner_wasm))]

use std::sync::Arc;

use journal::MemMedium;
use lait_issues::contract::{self, IssueQuery, PageRequest};
use mechanics::ids::{ActorId, DeviceId, SpaceId};
use mechanics::station::Epoch;
use replica::frontier::AuthorityFrontier;
use replica::Replica;
use runtime::world::{AuthorityView, Builder, PrincipalResolution, Query, World};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use wasm_probe::runner::WebInstance;
use world_runner::wasm_abi::GuestInit;
use world_sdk::RemoteWorld;

wasm_bindgen_test_configure!(run_in_dedicated_worker);

const ISSUES_RUNNER: &[u8] = include_bytes!(env!("ISSUES_RUNNER_WASM"));

/// The documented fixture posture of [`AuthorityView`]: every device resolves,
/// every read passes, and the active implementation is the one the runner
/// itself declared as reviewed. Production composes the pulled ledger instead.
struct FixtureAuthority {
    implementation: [u8; 32],
}

impl AuthorityView for FixtureAuthority {
    fn resolve(&self, device: &DeviceId) -> Option<PrincipalResolution> {
        Some(PrincipalResolution {
            actor: ActorId::from_incept_hash(device.as_str()),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(Vec::new()),
        })
    }

    fn active_implementation(
        &self,
        _world: &replica::body::WorldId,
        _authority_frontier: &AuthorityFrontier,
    ) -> Result<Option<[u8; 32]>, String> {
        Ok(Some(self.implementation))
    }
}

/// No sealing or opening material: an empty in-memory Replica reads nothing
/// protected, and a write would fail closed — exactly what this proof wants.
struct NoKeys;

impl replica::body::BodyKeySource for NoKeys {
    fn sealing_key(&self) -> Option<mechanics::authorization::AuthorizedBodyKey> {
        None
    }
    fn opening_key(
        &self,
        _epoch: &[u8; 16],
    ) -> Option<mechanics::authorization::AuthorizedBodyKey> {
        None
    }
}

#[wasm_bindgen_test]
fn a_real_query_crosses_the_engine_and_the_runner_in_a_browser() {
    // The guest: the real 39 MiB Issues runner, up under browser WebAssembly.
    let instance = WebInstance::launch(
        ISSUES_RUNNER,
        GuestInit {
            world: "com.lait.issues".into(),
            version: "0.9.5".into(),
            release: "local".into(),
        },
    )
    .expect("the Issues runner launches");
    let remote = RemoteWorld::connect_runner(Box::new(instance))
        .expect("the engine connects over the browser runner");
    let implementation = remote.reviewed_implementation();
    assert_ne!(implementation, [0; 32], "the runner declares its review");
    let world_id = remote.descriptor().id.clone();

    // The engine: the daemon's own registry/StationCore/Session machinery,
    // composed by this Worker over an empty in-memory Replica.
    let registry = Builder::new()
        .register_reviewed(Arc::new(remote), implementation)
        .build()
        .expect("the runner's declared contract registers");
    let replica = Replica::open_on(Arc::new(MemMedium::new()), Arc::new(NoKeys))
        .expect("an empty Replica opens on a browser medium");
    let station = runtime::browser::Station::compose(
        SpaceId::parse("ws_00000000000000000000000000").expect("a well-formed Space id"),
        replica,
        Arc::new(FixtureAuthority { implementation }),
        registry,
        Epoch::from_u64(1),
    )
    .expect("the browser Station composes");

    // Dock is already a real crossing: ensuring the World publication runs
    // the corpus build, whose extract calls dispatch into the guest.
    let identity = runtime::browser::Station::identity_from_seed(&[7u8; 32]);
    let session = station
        .dock(&world_id, &identity)
        .expect("a browser caller docks");

    // The Call: the real product question, dispatched into the guest, whose
    // semantic reads come back as context callbacks answered from the
    // Replica's snapshot. Empty store, so an honest empty page.
    let projection = session
        .query(Query {
            schema: contract::issue_schema(),
            schema_version: contract::ISSUE_SCHEMA_VERSION,
            payload: IssueQuery::Projects {
                page: PageRequest::default(),
            }
            .to_json(),
            publication: None,
        })
        .expect("the query crosses engine and runner and answers");

    // Runtime — not the World — stamped the read image and frontier.
    assert!(
        projection.publication.is_some(),
        "the projection carries its exact immutable read image"
    );
    assert!(
        !projection.demand.is_empty(),
        "the read demand is mandatory"
    );
    let page: serde_json::Value =
        serde_json::from_slice(&projection.bytes).expect("the projects page is JSON");
    let items = page["items"].as_array().expect("the page names its items");
    assert!(items.is_empty(), "an empty Space projects no projects");
}
