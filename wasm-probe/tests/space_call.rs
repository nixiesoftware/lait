//! S7.3b, the whole stack over real data: the browser pulls a Space a native
//! daemon founded — real ticket, real relay, real OPFS — and then the
//! daemon's own engine, composed in the same Worker (`runtime::browser`),
//! answers real queries from it through the real Issues runner. The issues
//! alice's daemon wrote come back in a tab, decrypted by the keyring the
//! pull replicated, authorized by the pulled ledger itself
//! (`LedgerAuthorityView`): activation, membership, and bob's contributor
//! read grant all arrived already-signed. Nothing here is a fixture.
//!
//! Order is load-bearing: the registry's schemas are declared on the fresh
//! Replica BEFORE the pull (`runtime::browser::declare_schemas` through the
//! pull helper's configure hook), because Convergence classifies each body
//! at incorporation and an undeclared schema is retained opaque — the
//! native Station makes the same declaration at activation, before its
//! Contact driver ever pulls.
//!
//! Runs under the `ci/browser-live-space.sh` harness, which bakes in the
//! rendezvous and the runner module. Skips honestly without them.

#![cfg(all(
    target_arch = "wasm32",
    feature = "probe-engine",
    feature = "probe-contact",
    issues_runner_wasm
))]

use std::sync::Arc;

use lait_issues::contract::{
    self, ChangeOperation, ChangeProject, IssueIntent, IssueQuery, PageRequest,
};
use mechanics::station::Epoch;
use runtime::world::{Builder, Intent, Query, RequestId, World};
use wasm_bindgen_test::{wasm_bindgen_test, wasm_bindgen_test_configure};
use wasm_probe::runner::WebInstance;
use wasm_probe::space_pull::{pull_space, unhex32};
use world_runner::wasm_abi::GuestInit;
use world_sdk::RemoteWorld;

wasm_bindgen_test_configure!(run_in_dedicated_worker);

const ISSUES_RUNNER: &[u8] = include_bytes!(env!("ISSUES_RUNNER_WASM"));

fn ask(session: &runtime::Session, query: IssueQuery) -> serde_json::Value {
    let projection = session
        .query(Query {
            schema: contract::issue_schema(),
            schema_version: contract::ISSUE_SCHEMA_VERSION,
            payload: query.to_json(),
            publication: None,
        })
        .expect("the query crosses engine and runner and answers");
    // Runtime — not the World — stamps the exact immutable read image.
    assert!(
        projection.publication.is_some(),
        "the projection carries its read image"
    );
    assert!(
        !projection.demand.is_empty(),
        "the read demand is mandatory"
    );
    serde_json::from_slice(&projection.bytes).expect("the projection is JSON")
}

#[wasm_bindgen_test]
async fn the_engine_in_a_tab_reads_and_writes_a_pulled_space() {
    let relay = option_env!("LIVE_RELAY_URL").expect("harness sets LIVE_RELAY_URL");
    let seed = unhex32(option_env!("LIVE_SEED_HEX").expect("harness sets LIVE_SEED_HEX"));
    let ticket = option_env!("LIVE_TICKET").expect("harness sets LIVE_TICKET");

    // The guest first: the real Issues runner under browser WebAssembly, and
    // the registry that holds it under its reviewed identity — the identity
    // the daemon activated in the ledger we are about to pull.
    let instance = WebInstance::launch(
        ISSUES_RUNNER,
        GuestInit {
            world: "com.lait.issues".into(),
            version: "0.9.5".into(),
            release: "local".into(),
        },
    )
    .expect("the Issues runner launches");
    let remote =
        RemoteWorld::connect_runner(Box::new(instance)).expect("the engine connects the runner");
    let implementation = remote.reviewed_implementation();
    let world_id = remote.descriptor().id.clone();
    let registry = Builder::new()
        .register_reviewed(Arc::new(remote), implementation)
        .build()
        .expect("the runner's declared contract registers");

    // The pull, with the schemas declared before incorporation.
    let pulled = pull_space(relay, seed, ticket, |replica| {
        runtime::browser::declare_schemas(replica, &registry);
    })
    .await;
    assert!(pulled.outcome.bytes_moved > 0, "material moved");

    // The engine over the pulled Space: the pulled ledger IS the authority.
    let authority = runtime::browser::LedgerAuthorityView(pulled.authority.clone());
    let station = runtime::browser::Station::compose(
        pulled.space.clone(),
        pulled.replica,
        Arc::new(authority),
        registry,
        Epoch::from_u64(1),
    )
    .expect("the browser Station composes over the pulled Replica");
    let identity = runtime::browser::Station::identity_from_seed(&seed);
    let session = station
        .dock(&world_id, &identity)
        .expect("the admitted member docks against the pulled ledger");

    // Alice's project, read back through the whole stack.
    let projects = ask(
        &session,
        IssueQuery::Projects {
            page: PageRequest::default(),
        },
    );
    let items = projects["items"].as_array().expect("a projects page");
    assert!(
        items
            .iter()
            .any(|p| p["key"] == "ENG" && p["name"] == "Engineering"),
        "alice's ENG project crossed: {projects}"
    );

    // Alice's issues, read back — this also opens the sealed catalog through
    // the keyring the pull replicated.
    let list = ask(
        &session,
        IssueQuery::List {
            project: None,
            label: None,
            status: None,
            milestone: None,
            mine: None,
            all: false,
            me: None,
            facets: Default::default(),
            page: PageRequest::default(),
        },
    );
    let rows = list["items"].as_array().expect("an issues page");
    let titles: Vec<&str> = rows.iter().filter_map(|r| r["title"].as_str()).collect();
    assert!(
        titles.contains(&"the tab pulls this issue") && titles.contains(&"and this one"),
        "alice's issues crossed whole: {titles:?}"
    );

    // Slice 8.2: bob WRITES, in the tab. A durable mutation over the pulled
    // Space — the daemon's own Session::submit runs on wasm: bob signs an
    // Issues ChangeSet with his docked device, the runner validates and stages
    // it, Runtime seals the bodies with the space key the pull replicated,
    // mints the receipt through the pulled ledger (bob's contributor grant
    // carries write standing), commits to OPFS, and publishes one invalidation
    // record — drained here through the wasm try_next. Nothing is a fixture:
    // real member, real key, real grant, real store.
    let mut stream = session.observe(None);
    let reset = stream
        .try_next()
        .expect("the stream lives")
        .expect("a first record");
    assert!(reset.reset, "observe rebaselines first");

    let action = identity
        .sign_action(
            &session,
            RequestId::mint(),
            Intent {
                schema: contract::issue_schema(),
                schema_version: contract::ISSUE_SCHEMA_VERSION,
                payload: IssueIntent::ChangeSet {
                    operations: vec![ChangeOperation::IssueCreate {
                        project: ChangeProject::Existing {
                            project: "ENG".into(),
                        },
                        title: "written from a tab".into(),
                        priority: "none".into(),
                        status: None,
                        parent: None,
                        assignees: Vec::new(),
                        labels: Vec::new(),
                        body: None,
                        due: None,
                        estimate: None,
                    }],
                    ts: 1_700_000_000_001,
                }
                .to_json(),
            },
        )
        .expect("bob signs the write with his docked device");
    let committed = session
        .submit(action)
        .expect("bob's write commits durably in the tab, sealed by the pulled key");
    assert!(
        !committed.bodies.is_empty(),
        "the commit wrote issue bodies"
    );

    // The commit published exactly one record, drained through the wasm
    // try_next — its publication is the one bob committed.
    let record = stream
        .try_next()
        .expect("the stream lives")
        .expect("the commit published a record");
    assert!(!record.reset, "a durable commit is not a reset");
    assert_eq!(
        record.publications[0].publication, committed.publication,
        "the observed record names bob's committed read image",
    );

    // And bob's issue reads back beside alice's, in the same tab.
    let after = ask(
        &session,
        IssueQuery::List {
            project: None,
            label: None,
            status: None,
            milestone: None,
            mine: None,
            all: false,
            me: None,
            facets: Default::default(),
            page: PageRequest::default(),
        },
    );
    let after_titles: Vec<&str> = after["items"]
        .as_array()
        .expect("an issues page")
        .iter()
        .filter_map(|r| r["title"].as_str())
        .collect();
    assert!(
        after_titles.contains(&"written from a tab"),
        "bob's issue, written in the tab, reads back: {after_titles:?}",
    );

    // Slice 8.x: bob's tab PUSHES its write OUT to alice's daemon — symmetric
    // convergence, the parity claim. Nothing dials a tab, so it pushes on the
    // dial it makes: `export_excess` builds bob's committed "written from a tab"
    // issue from the composed core, and `pull_receive` with `Some(excess)` both
    // pulls alice's material AND serves bob's on the same connection. Alice's
    // daemon (RECIPROCAL_CONVERGE granted) reverse-incorporates it — and the
    // harness then asserts alice holds bob's write, no dial from alice. The
    // pulled half installs into the live core through its own writer as before.
    let holdings = station.published_root();
    let push_bundle = pulled.authority.bundle();
    let excess = station
        .export_excess(&pulled.seed, &push_bundle)
        .expect("the tab builds its excess to push");
    assert!(
        !excess.bodies.is_empty() || !excess.authority_records.is_empty(),
        "bob's tab has a write to push"
    );
    let received = contact::pull::pull_receive(
        pulled.transport.as_ref(),
        &pulled.responder,
        &pulled.space,
        &pulled.seed,
        &push_bundle,
        holdings,
        Some(excess),
        contact::pull::Deadlines::default(),
    )
    .await
    .expect("the converge (pull + push) completes over the same transport");
    let bundle = pulled.authority.bundle();
    let signer = replica::transaction::SeedSigner(&pulled.seed);
    station
        .with_replica_convergence(|replica| {
            let ctx = replica::transaction::CommitContext {
                space: &pulled.space,
                signer: &signer,
                authority_frontier: (bundle.frontier)(),
            };
            let mut incorporator = bundle
                .incorporator
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let validated = replica.validate_contact(
                &received.staged,
                bundle.source.as_ref(),
                &mut *incorporator,
            )?;
            replica.incorporate_bundle(&ctx, validated, bundle.source.as_ref())
        })
        .expect("the live re-pull installs into the composed core through its writer");

    // The live core survived the convergence: it still answers, still holding
    // alice's issues and bob's write.
    let live = ask(
        &session,
        IssueQuery::List {
            project: None,
            label: None,
            status: None,
            milestone: None,
            mine: None,
            all: false,
            me: None,
            facets: Default::default(),
            page: PageRequest::default(),
        },
    );
    let live_titles: Vec<&str> = live["items"]
        .as_array()
        .expect("an issues page")
        .iter()
        .filter_map(|r| r["title"].as_str())
        .collect();
    assert!(
        live_titles.contains(&"written from a tab"),
        "the live core still answers after a re-pull installed through its writer: {live_titles:?}",
    );
}
