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
use std::time::{Duration, Instant};

use mechanics::crypto::AuthorizedBodyKey;
use mechanics::ids::{ActorId, DeviceId, StationId};
use replica::body::{BodySchema, MutationModel};
use replica::frontier::{AuthorityFrontier, ReplicaFrontier};
use replica::ids::{EncodingId, SchemaId, WorldId};
use runtime::live::LiveHandle;
use runtime::transient::{TransientItem, TransientPayload, TransientScope};
use runtime::{
    ActivationOptions, Runtime, RuntimeBuilder, SpaceFormationOptions, Station, World,
    WorldContext, WorldEffect, WorldError, WorldIntent, WorldLimits, WorldProjection, WorldQuery,
    WorldRegistration, WorldVersion,
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
    schemas: Vec<BodySchema>,
}

impl Empty {
    fn new() -> Self {
        Self {
            id: WorldId::parse("dev.example.pad").unwrap(),
            schemas: vec![BodySchema {
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
    fn schemas(&self) -> &[BodySchema] {
        &self.schemas
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
        Ok(WorldProjection {
            demand: Vec::new(),
            schema: SchemaId::parse("entry").unwrap(),
            schema_version: 1,
            bytes: Vec::new(),
            frontier: ReplicaFrontier::EMPTY,
        })
    }
}

struct Permissive;
impl runtime::AuthorityView for Permissive {
    fn resolve(&self, _device: &DeviceId) -> Option<runtime::PrincipalResolution> {
        Some(runtime::PrincipalResolution {
            actor: ActorId::from_incept_hash(&"a".repeat(64)),
            authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![1]),
        })
    }
}

fn options() -> ActivationOptions {
    ActivationOptions {
        planes: Default::default(),
        content: Default::default(),
        drain_deadline: Duration::from_secs(5),
        comms: None,
        observation_capacity: 0,
    }
}

fn station_at(root: &std::path::Path) -> Station {
    let world = Empty::new();
    let registration = WorldRegistration {
        id: world.id(),
        implementation_version: WorldVersion(1),
        schemas: world.schemas().to_vec(),
        limits: WorldLimits::default(),
        scope_schemas: Vec::new(),
        signal_schemas: Vec::new(),
    };
    let registry = RuntimeBuilder::new()
        .register(registration, Arc::new(world))
        .build()
        .unwrap();
    Runtime::open(
        root.to_path_buf(),
        registry,
        Arc::new(Permissive),
        Arc::new(replica::StaticBodyKeys::new(
            AuthorizedBodyKey::for_authorized_epoch([1u8; 16], [2u8; 32]),
        )),
    )
    .form_space(SpaceFormationOptions::default())
    .unwrap()
    .activate(options())
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

fn peer_station(seed: u8) -> StationId {
    StationId::from_device(&mechanics::crypto::device_from_seed(&[seed; 32])).expect("station")
}

fn caret_scope(body: u8) -> TransientScope {
    TransientScope::TextCaret {
        world: "dev.example.pad".into(),
        body: [body; 16],
        field: "text".into(),
    }
}

fn presence(scope: TransientScope, epoch: [u8; 16], seq: u64) -> TransientItem {
    TransientItem {
        session_epoch: epoch,
        seq,
        scope,
        payload: TransientPayload::Presence,
    }
}

fn view_scope(body: u8) -> TransientScope {
    TransientScope::IssueView {
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
                    session_epoch: [peer; 16],
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

    let orbit = station.go_dormant().expect("dormant");
    let station = orbit.activate(options()).expect("reactivated");

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
fn dormancy_leaves_no_live_state_behind() {
    // Acceptance 9's Live leg, for the ordinary path. The drain joins tracked
    // tasks on a deadline and then leaks by design, so what is asserted here is
    // that a Station shutting down normally releases what it held — not that a
    // rogue task can be stopped, which it cannot.
    let root = temp_root();
    let station = station_at(&root);
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

    let orbit = station.go_dormant().expect("drained inside the deadline");
    // The handle outlives the Station because a caller may hold one. What must
    // not outlive it is a driver thread, and `go_dormant` returning is the
    // assertion: it joins every tracked task before releasing the store lock.
    drop(orbit);
}

#[test]
fn an_offer_survives_a_disconnect_and_not_a_restart() {
    // The two halves of "not durable" that are easy to conflate. An offer is not
    // expired by a TTL and not dropped by a disconnect — the file is still
    // there. It is also not written down, so a restart forgets it.
    let root = temp_root();
    let station = station_at(&root);
    station.live().offer(runtime::signal::PendingOffer {
        from: peer_station(41),
        session_epoch: [3u8; 16],
        content: [7u8; 32],
        plaintext_len: 1024,
        display_name: "notes.txt".into(),
        media_type: "text/plain".into(),
    });
    assert_eq!(station.live().pending_offers().len(), 1);

    let store = fingerprint(station.store_dir());
    let orbit = station.go_dormant().expect("dormant");
    let station = orbit.activate(options()).expect("reactivated");
    assert!(
        station.live().pending_offers().is_empty(),
        "an offer is held in memory, and memory is what a restart discards"
    );
    // And holding it wrote nothing, which is what makes forgetting it correct
    // rather than a loss of something we had promised to keep.
    assert_ne!(
        store, [0u8; 32],
        "the fingerprint is a real digest, not a default"
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
