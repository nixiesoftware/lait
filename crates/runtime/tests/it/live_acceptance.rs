//! The docket's acceptance scenarios for the Live plane, against real Stations.
//!
//! `live_transient.rs` proves the plane's pieces. This proves the claims the
//! docket makes about a Station running one: that awareness costs nothing
//! durable, that a restart forgets, that a flood is bounded, and that shutting
//! down does not leave a session behind.
//!
//! Every assertion here is against a public surface. There is no journal commit
//! counter to read — `commits_since_sweep` is private with no accessor — so
//! "nothing was written" is asserted by fingerprinting the bytes, which catches
//! strictly more anyway: a frontier can be unchanged across a commit that wrote
//! and then swept.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

use mechanics::authorization::AuthorizedBodyKey;
use mechanics::{
    ids::{ActorId, DeviceId},
    station::Key,
};
use replica::body::{EncodingId, SchemaId, WorldId};
use replica::body::{MutationModel, Schema};
use replica::frontier::{AuthorityFrontier, ReplicaFrontier};
use runtime::plane::live::LiveHandle;
use runtime::transient::{Target, TransientItem, TransientPayload};
use runtime::{
    plane::Activation, world::Builder, world::Context, world::Effect, world::Intent,
    world::Projection, world::Query, world::Rejection, world::World, Runtime, Station,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_root() -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-live-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A World with one schema and no behaviour. What is under test is the Station,
/// not the World.
struct Empty {
    id: WorldId,
    schemas: Vec<Schema>,
}

impl Empty {
    fn new() -> Self {
        Self {
            id: WorldId::parse("dev.example.pad").unwrap(),
            schemas: vec![Schema {
                id: SchemaId::parse("entry").unwrap(),
                version: 1,
                encoding: EncodingId::parse("bytes").unwrap(),
                mutation: MutationModel::Atomic,
                readable_predecessors: vec![],
            }],
        }
    }
}

impl World for Empty {
    fn id(&self) -> WorldId {
        self.id.clone()
    }
    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }
    fn submit(&self, _ctx: &mut Context<'_>, _intent: Intent) -> Result<Effect, Rejection> {
        Err(Rejection::InvalidRequest)
    }
    fn query(&self, _ctx: &Context<'_>, _query: Query) -> Result<Projection, Rejection> {
        Ok(Projection {
            demand: Vec::new(),
            schema: SchemaId::parse("entry").unwrap(),
            schema_version: 1,
            bytes: Vec::new(),
            frontier: ReplicaFrontier::EMPTY,
            publication: None,
        })
    }
}

struct Permissive;
impl runtime::world::AuthorityView for Permissive {
    fn resolve(&self, _device: &DeviceId) -> Option<runtime::world::PrincipalResolution> {
        Some(runtime::world::PrincipalResolution {
            actor: ActorId::from_incept_hash(&"a".repeat(64)),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![1]),
        })
    }
}

fn options() -> Activation {
    Activation {
        consent: Default::default(),
        exec: Default::default(),
        planes: Default::default(),
        content: Default::default(),
        find: Default::default(),
        drain_deadline: Duration::from_secs(5),
        comms: None,
        observation_capacity: 0,
    }
}

struct AnySigner;
impl replica::transaction::AuthoritySource for AnySigner {
    fn signer_authorized(&self, _signer: &[u8; 32], _f: &AuthorityFrontier) -> bool {
        true
    }
}

#[derive(Default)]
struct Accepting;
impl replica::convergence::AuthorityIncorporator for Accepting {
    fn incorporate_authority(
        &mut self,
        records: &[Vec<u8>],
    ) -> Result<replica::convergence::AuthorityBatchReceipt, replica::convergence::Failure> {
        Ok(replica::convergence::AuthorityBatchReceipt {
            space: mechanics::ids::SpaceId::from_digest([0u8; 16]),
            prior_frontier: AuthorityFrontier::from_canonical_bytes(vec![]),
            resulting_frontier: AuthorityFrontier::from_canonical_bytes(vec![1]),
            batch_digest: *blake3::hash(&records.concat()).as_bytes(),
        })
    }
}

/// Activation options with a real transport, so the plane drivers and the
/// dialer are actually spawned.
///
/// The offline options above mount nothing: `Orbit::activate` only reaches the
/// driver block when `comms` is `Some`. A dormancy test against an offline
/// Station therefore drains an empty task set, which is the shape of a test
/// that passes for a reason unrelated to what it claims.
fn options_with_comms(transport: Arc<dyn comms::Transport>, seed: [u8; 32]) -> Activation {
    Activation {
        consent: Default::default(),
        exec: Default::default(),
        planes: Default::default(),
        content: Default::default(),
        find: Default::default(),
        drain_deadline: Duration::from_secs(5),
        comms: Some(runtime::plane::CommsOptions {
            transport,
            station_seed: seed,
            authority: runtime::plane::contact::Authority {
                source: Arc::new(AnySigner),
                incorporator: Arc::new(std::sync::Mutex::new(Accepting)),
                export: Arc::new(Vec::new),
                frontier: Arc::new(|| AuthorityFrontier::from_canonical_bytes(vec![1])),
            },
            gossip: None,
            whole_deadline: Duration::from_secs(20),
            progress_deadline: Duration::from_secs(5),
            route_lease: Duration::from_secs(60),
        }),
        observation_capacity: 0,
    }
}

fn station_with_comms(root: &std::path::Path, seed: [u8; 32]) -> Station {
    let net = comms::mem::MemNet::new();
    let transport: Arc<dyn comms::Transport> =
        Arc::new(net.peer(mechanics::actor::device_from_seed(&seed)));
    let world = Empty::new();
    let registry = Builder::new().register(Arc::new(world)).build().unwrap();
    Runtime::open(
        root.to_path_buf(),
        registry,
        Arc::new(Permissive),
        Arc::new(replica::body::StaticBodyKeys::new(
            AuthorizedBodyKey::for_authorized_epoch([1u8; 16], [2u8; 32]),
        )),
    )
    .create()
    .unwrap()
    .open(options_with_comms(transport, seed))
    .unwrap()
}

fn station_at(root: &std::path::Path) -> Station {
    let world = Empty::new();
    let registry = Builder::new().register(Arc::new(world)).build().unwrap();
    Runtime::open(
        root.to_path_buf(),
        registry,
        Arc::new(Permissive),
        Arc::new(replica::body::StaticBodyKeys::new(
            AuthorizedBodyKey::for_authorized_epoch([1u8; 16], [2u8; 32]),
        )),
    )
    .create()
    .unwrap()
    .open(options())
    .unwrap()
}

/// Every byte under the store directory, as one digest.
fn fingerprint(dir: &std::path::Path) -> [u8; 32] {
    fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.push((
                    path.strip_prefix(base)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/"),
                    bytes,
                ));
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    // Sorted: directory order is a filesystem's business, and two identical
    // stores must fingerprint identically.
    files.sort();
    let mut hasher = blake3::Hasher::new();
    for (name, bytes) in files {
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    *hasher.finalize().as_bytes()
}

fn peer_station(seed: u8) -> Key {
    Key::from_device(&mechanics::actor::device_from_seed(&[seed; 32])).expect("station")
}

fn caret_scope(body: u8) -> Target {
    Target::Field {
        world: "dev.example.pad".into(),
        body: [body; 16],
        field: "text".into(),
    }
}

fn presence(scope: Target, epoch: [u8; 16], seq: u64) -> TransientItem {
    TransientItem {
        connection_epoch: epoch,
        seq,
        scope,
        payload: TransientPayload::Presence,
    }
}

fn view_scope(body: u8) -> Target {
    Target::Body {
        world: "dev.example.pad".into(),
        body: [body; 16],
    }
}

#[test]
fn five_peers_moving_cursors_change_nothing_durable() {
    // Acceptance 5. The claim the whole plane rests on: awareness is free, in
    // the sense that costs nothing anybody has to keep.
    let root = temp_root();
    let station = station_at(&root);
    let live = station.live();

    let frontier = station.frontier();
    let before = fingerprint(station.store_dir());

    let now = Instant::now();
    for round in 1..=400u64 {
        for peer in 1..=5u8 {
            live.record(
                &peer_station(peer + 40),
                &presence(view_scope(peer), [peer; 16], round),
                now,
            );
            live.record(
                &peer_station(peer + 40),
                &TransientItem {
                    connection_epoch: [peer; 16],
                    seq: round,
                    scope: caret_scope(peer),
                    payload: TransientPayload::Typing,
                },
                now,
            );
        }
    }

    assert_eq!(station.frontier(), frontier, "no commit, no frontier move");
    assert_eq!(
        before,
        fingerprint(station.store_dir()),
        "and not one byte written: not the journal, not the objects, not the manifest"
    );
    // And the traffic did arrive, or the assertions above prove nothing.
    assert!(!live.view(None, now).entries.is_empty());
}

#[test]
fn a_restart_cannot_resurrect_a_cursor() {
    // Acceptance 6. A caret that outlived the tab holding it is a ghost, and a
    // presence that survived a crash is a lie about who is here.
    let root = temp_root();
    let station = station_at(&root);
    let now = Instant::now();
    station.live().record(
        &peer_station(41),
        &presence(view_scope(1), [9u8; 16], 1),
        now,
    );
    assert_eq!(station.live().view(None, now).entries.len(), 1);

    let orbit = station.vacate().expect("dormant");
    let station = orbit.open(options()).expect("reactivated");

    assert!(
        station.live().view(None, Instant::now()).entries.is_empty(),
        "a new activation knows nothing about who was here before it"
    );
}

#[test]
fn a_second_tab_supersedes_the_first_with_no_wire_field_for_it() {
    // One Station publishes one caret per (scope, kind). Two tabs belonging to
    // the same person are the same Station, so the second replaces the first —
    // and it does so because the slot key says so, not because either tab
    // carries a discriminator the other could forge.
    let handle = LiveHandle::new(None);
    let now = Instant::now();
    let peer = peer_station(41);

    handle.record(&peer, &presence(view_scope(1), [9u8; 16], 1), now);
    handle.record(&peer, &presence(view_scope(1), [9u8; 16], 2), now);
    let view = handle.view(None, now);
    assert_eq!(view.entries.len(), 1, "one Station, one presence per scope");

    // A different Station in the same scope is a different person, and both are
    // shown.
    handle.record(
        &peer_station(42),
        &presence(view_scope(1), [8u8; 16], 1),
        now,
    );
    assert_eq!(handle.view(None, now).entries.len(), 2);
}

#[test]
fn dormancy_joins_the_drivers_it_started() {
    // Acceptance 9's Live leg, for the ordinary path. The drain joins tracked
    // tasks on a deadline and then leaks by design, so what is asserted is that
    // a Station shutting down normally releases what it held — not that a rogue
    // task can be stopped, which it cannot.
    //
    // **With a transport**, which the first version of this did not have.
    // `Orbit::activate` only reaches the driver block when `comms` is `Some`, so
    // against an offline Station `vacate` drained an empty task set and the
    // test passed without either plane ever having run.
    let root = temp_root();
    let station = station_with_comms(&root, [51u8; 32]);
    let now = Instant::now();
    let live = station.live();
    for peer in 1..=5u8 {
        live.record(
            &peer_station(peer + 40),
            &presence(view_scope(peer), [peer; 16], 1),
            now,
        );
    }
    assert_eq!(live.view(None, now).entries.len(), 5);

    let started = tokio::time::Instant::now();
    let orbit = station.vacate().expect("drained inside the deadline");
    // Inside the deadline, not merely eventually. A drain that ran long would
    // still return — it leaks rather than blocking — so the elapsed time is
    // what distinguishes "joined" from "gave up and leaked".
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the drivers were joined rather than abandoned to the deadline"
    );
    // Reactivating proves the store lock came back, which it does not if a
    // driver thread is still holding the Station's directory.
    let station = orbit.open(options()).expect("the lock was released");
    assert!(station.live().view(None, Instant::now()).entries.is_empty());
}

#[test]
fn an_offer_survives_a_disconnect_and_not_a_restart() {
    // The two halves of "not durable" that are easy to conflate. An offer is not
    // expired by a TTL and not dropped by a disconnect — the file is still
    // there. It is also not written down, so a restart forgets it.
    let root = temp_root();
    let station = station_at(&root);
    let offering = peer_station(41);

    // Taken *before* the offer, so the comparison after it means something. The
    // first version of this took one fingerprint afterwards and asserted only
    // that it was not all-zeroes — true of any digest, including a digest over a
    // store the offer had just been written into.
    let before = fingerprint(station.store_dir());
    station.live().offer(runtime::signal::PendingOffer {
        from: offering.clone(),
        connection_epoch: [3u8; 16],
        content: [7u8; 32],
        plaintext_len: 1024,
        display_name: "notes.txt".into(),
        media_type: "text/plain".into(),
    });
    assert_eq!(station.live().pending_offers().len(), 1);
    assert_eq!(
        before,
        fingerprint(station.store_dir()),
        "holding an offer writes nothing"
    );

    // A disconnect, which is what `forget` is — the half of this test's own name
    // that the first version never exercised. The offer stays: the file is still
    // there, and the peer whose laptop slept is still worth fetching from.
    station.live().forget(&offering);
    assert_eq!(
        station.live().pending_offers().len(),
        1,
        "a disconnect drops presence, not offers"
    );

    let orbit = station.vacate().expect("dormant");
    let station = orbit.open(options()).expect("reactivated");
    assert!(
        station.live().pending_offers().is_empty(),
        "an offer is held in memory, and memory is what a restart discards"
    );
}

#[test]
fn the_view_a_station_offers_is_the_view_its_own_plane_wrote() {
    // `Station::live()` hands out the same handle the driver writes, not a copy.
    // A second handle would be a Station whose browser and whose peers disagree
    // about who is present, and the disagreement would be invisible.
    let root = temp_root();
    let station = station_at(&root);
    let now = Instant::now();
    let first = station.live();
    let second = station.live();
    first.record(
        &peer_station(41),
        &presence(view_scope(1), [9u8; 16], 1),
        now,
    );
    assert_eq!(second.view(None, now).entries.len(), 1);
}
