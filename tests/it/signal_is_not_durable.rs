//! The one property reliable signals must have, enforced where it can be.
//!
//! A signal is delivered or it fails loudly, and it is durable in no other
//! sense: nothing journaled, nothing replayed after a restart, nothing that
//! becomes activity. Two of those three are the same mechanism seen twice —
//! `StationCore::with_replica` is the only route to the Replica writer, and
//! `Broadcaster::publish` is the only route to the Observation ring, which
//! `StationHost::frame_for` turns into `activity_advanced` for any Observation
//! carrying scopes. The third, surviving a restart, is a consequence rather
//! than a mechanism: `Orbit::activate` builds a fresh `StationCore` and reads
//! nothing signal-shaped from disk.
//!
//! **Why a parser and not privacy.** `Broadcaster::publish` is `pub(crate)` and
//! `signal.rs` lives inside that crate, so `pub(crate)` stops nothing;
//! `StationCore::with_replica` is outright `pub`. The three legitimate
//! `publish` call sites are all durable-commit or authority-advance. A fourth,
//! added from signal code, would journal nothing and still emit an Observation
//! that becomes activity — and it would be one line, in a file whose tests all
//! still pass.
//!
//! So the gate reads the source. It lands before the transport it guards, so
//! there is never a window in which the rule is only a comment.

use std::path::Path;

use syn::visit::Visit;

/// Names that reach durable state or the Observation ring.
///
/// Deliberately coarse. A false positive here costs a rename; a false negative
/// costs the property this whole module exists to have.
const FORBIDDEN: &[&str] = &[
    "StationCore",
    "with_replica",
    "Broadcaster",
    "publish",
    "Replica",
    "Journal",
    "commit_action",
    "note_authority_advanced",
];

#[derive(Default)]
struct Durability {
    found: Vec<String>,
}

impl<'ast> Visit<'ast> for Durability {
    fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
        let name = segment.ident.to_string();
        if FORBIDDEN.contains(&name.as_str()) {
            self.found.push(name);
        }
        syn::visit::visit_path_segment(self, segment);
    }

    /// A method call is not a path segment, so `core.with_replica(..)` would
    /// slip past the visitor above. This is the shape the rule is most likely
    /// to be broken in.
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        let name = call.method.to_string();
        if FORBIDDEN.contains(&name.as_str()) {
            self.found.push(name);
        }
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_use_tree(&mut self, tree: &'ast syn::UseTree) {
        if let syn::UseTree::Name(name) = tree {
            let ident = name.ident.to_string();
            if FORBIDDEN.contains(&ident.as_str()) {
                self.found.push(ident);
            }
        }
        syn::visit::visit_use_tree(self, tree);
    }

    // `std::fs` anywhere in this module is durable state by another route.
    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let rendered = quote_path(&item.tree);
        if rendered.starts_with("fs") || rendered.contains("std :: fs") {
            self.found.push("std::fs".into());
        }
        syn::visit::visit_item_use(self, item);
    }
}

fn quote_path(tree: &syn::UseTree) -> String {
    match tree {
        syn::UseTree::Path(path) => format!("{} :: {}", path.ident, quote_path(&path.tree)),
        syn::UseTree::Name(name) => name.ident.to_string(),
        syn::UseTree::Rename(rename) => rename.ident.to_string(),
        syn::UseTree::Glob(_) => "*".into(),
        syn::UseTree::Group(group) => group
            .items
            .iter()
            .map(quote_path)
            .collect::<Vec<_>>()
            .join(", "),
    }
}

fn offenders(source: &str) -> Vec<String> {
    let parsed = syn::parse_file(source).expect("signal.rs parses");
    let mut visitor = Durability::default();
    visitor.visit_file(&parsed);
    visitor.found.sort();
    visitor.found.dedup();
    visitor.found
}

#[test]
fn the_signal_module_cannot_reach_durable_state() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/runtime/src/signal.rs");
    let source =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let found = offenders(&source);
    assert!(
        found.is_empty(),
        "crates/runtime/src/signal.rs names {found:?}.\n\
         A reliable signal is never journaled, never replayed after a restart, and never \n\
         emitted as activity. `with_replica` reaches the Replica writer and `publish` reaches \n\
         the Observation ring, which becomes `activity_advanced` for anything carrying scopes.\n\
         If a signal genuinely needs durable state, it is not a signal."
    );
}

#[test]
fn the_gate_can_see_what_it_claims_to_see() {
    // The negative control, and the reason it is not optional.
    //
    // A parser that silently stopped matching — a syn upgrade changing a visit
    // method, a refactor moving the call behind an alias — would keep passing
    // forever while guarding nothing. The only way to know it still works is to
    // show it rejecting something.
    let violation = r#"
        use crate::session::StationCore;
        pub fn send(core: &StationCore) {
            let _ = core.with_replica(|replica| Ok(replica.frontier()));
        }
    "#;
    let found = offenders(violation);
    assert!(
        found.contains(&"StationCore".to_string()),
        "the visitor did not see a type it is supposed to reject: {found:?}"
    );
    assert!(
        found.contains(&"with_replica".to_string()),
        "the visitor did not see a call it is supposed to reject: {found:?}"
    );

    // And the ring, by its own route.
    let ring = r#"
        pub fn shout(broadcaster: &Broadcaster) {
            broadcaster.publish(vec![], frontier, false);
        }
    "#;
    assert!(
        offenders(ring).contains(&"publish".to_string()),
        "the visitor did not see the Observation ring"
    );

    // A file that is genuinely clean must still pass, or the gate is a
    // tautology that rejects everything.
    let clean = r#"
        pub struct Declaration { pub selector: u16 }
        pub fn declarations() -> Vec<Declaration> { Vec::new() }
    "#;
    assert!(offenders(clean).is_empty());
}

/// The same property, proved by running rather than by parsing.
///
/// The gate above says signal code *cannot reach* the Replica writer. This says
/// the running system *does not write*. Both are needed, and the second catches
/// what the first cannot: a handler that reaches the Replica indirectly, through
/// a World `submit` several calls away, on a path the parser reads as ordinary.
/// The parser sees nothing; the store fingerprint sees `journal/` change.
mod behaviour {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use mechanics::authorization::AuthorizedBodyKey;
    use mechanics::{
        ids::{ActorId, DeviceId},
        station::Key,
    };
    use replica::body::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
    use replica::body::{MutationModel, Op, Schema};
    use replica::frontier::{AuthorityFrontier, ReplicaFrontier};
    use runtime::plane::Signal;
    use runtime::signal::DeliveredSignal;
    use runtime::{
        plane::Activation, world::Builder, world::Context, world::Effect, world::Intent,
        world::LocalIdentity, world::Projection, world::Query, world::Rejection, world::RequestId,
        world::World, Runtime, Session, Station,
    };

    const WRITER_SEED: [u8; 32] = [55u8; 32];
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root() -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("lait-sig-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn demand() -> Vec<u8> {
        mechanics::authorization::AuthorizationDemand::require(
            mechanics::authorization::PolicyCapability::new("w", "c"),
            mechanics::authorization::Resource::root("w"),
        )
        .encode_canonical()
        .expect("canonical demand")
    }

    struct KvWorld {
        id: WorldId,
        schemas: Vec<Schema>,
    }

    impl KvWorld {
        fn new() -> Self {
            Self {
                id: WorldId::parse("dev.example.kv").unwrap(),
                schemas: vec![Schema {
                    id: SchemaId::parse("entry").unwrap(),
                    version: 1,
                    encoding: EncodingId::parse("bytes").unwrap(),
                    mutation: MutationModel::Atomic,
                    readable_predecessors: vec![],
                }],
            }
        }
        fn body(&self, key: &str) -> BodyKey {
            let mut raw = [0u8; 16];
            let k = key.as_bytes();
            raw[..k.len().min(16)].copy_from_slice(&k[..k.len().min(16)]);
            BodyKey::new(self.id.clone(), BodyId::from_bytes(raw))
        }
    }

    impl World for KvWorld {
        fn id(&self) -> WorldId {
            self.id.clone()
        }
        fn schemas(&self) -> &[Schema] {
            &self.schemas
        }
        fn submit(&self, _ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
            let text = String::from_utf8(intent.payload).map_err(|_| Rejection::InvalidRequest)?;
            let (key, value) = text.split_once('=').ok_or(Rejection::InvalidRequest)?;
            let body = self.body(key);
            Ok(Effect {
                content_refs: Vec::new(),
                exec: Vec::new(),
                demand: demand(),
                operations: vec![(
                    body.clone(),
                    Op::ReplaceAtomic {
                        value: value.as_bytes().to_vec(),
                    },
                )],
                bodies: vec![body],
                effect: vec![],
                declarations: vec![],
            })
        }
        fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
            let key = String::from_utf8(query.payload).map_err(|_| Rejection::InvalidRequest)?;
            Ok(Projection {
                demand: demand(),
                schema: SchemaId::parse("entry").unwrap(),
                schema_version: 1,
                bytes: ctx
                    .read_body(&self.body(&key))?
                    .map(|bytes| bytes.as_ref().to_vec())
                    .unwrap_or_default(),
                frontier: ReplicaFrontier::EMPTY,
                publication: None,
            })
        }
    }

    struct Permissive;
    impl runtime::world::AuthorityView for Permissive {
        fn resolve(&self, _device: &DeviceId) -> Option<runtime::world::PrincipalResolution> {
            Some(runtime::world::PrincipalResolution {
                actor: ActorId::from_incept_hash(&"e".repeat(64)),
                authority_frontier: AuthorityFrontier::from_canonical_bytes(vec![5]),
            })
        }
    }

    fn runtime_at(root: &std::path::Path) -> Runtime {
        let world = KvWorld::new();
        let registry = Builder::new().register(Arc::new(world)).build().unwrap();
        Runtime::open(
            root.to_path_buf(),
            registry,
            Arc::new(Permissive),
            Arc::new(replica::body::StaticBodyKeys::new(
                AuthorizedBodyKey::for_authorized_epoch([17u8; 16], [18u8; 32]),
            )),
        )
    }

    fn station_at(root: &std::path::Path) -> Station {
        runtime_at(root).create().unwrap().open(options()).unwrap()
    }

    fn options() -> Activation {
        Activation {
            exec: Default::default(),
            planes: Default::default(),
            content: Default::default(),
            find: Default::default(),
            drain_deadline: Duration::from_secs(5),
            comms: None,
            observation_capacity: 0,
        }
    }

    fn dock(station: &Station) -> (Session, LocalIdentity) {
        let world = WorldId::parse("dev.example.kv").unwrap();
        let writer = Runtime::identity_from_seed(&WRITER_SEED);
        let session = station.dock(&world, &writer).unwrap();
        (session, writer)
    }

    /// Every byte under the store directory, as one digest.
    ///
    /// Not a journal commit count: `commits_since_sweep` is private with no
    /// accessor, and this catches strictly more anyway. A frontier can be
    /// unchanged across a commit that wrote and then swept — the bytes cannot.
    fn fingerprint(dir: &std::path::Path) -> [u8; 32] {
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, base, out);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    let name = path
                        .strip_prefix(base)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    out.push((name, bytes));
                }
            }
        }
        walk(dir, dir, &mut files);
        // Sorted, because directory order is a filesystem's business and two
        // identical stores must fingerprint identically.
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

    /// The subtrees a signal would have to touch to be durable.
    ///
    /// `journal/` and `objects/` and nothing else. The store also holds a
    /// `counter` that activation increments on purpose, and a `content-cache/`
    /// that is not durable material — a comparison spanning either would fail
    /// across a restart for reasons that have nothing to do with signals.
    fn durable_fingerprint(dir: &std::path::Path) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        for name in ["journal", "objects"] {
            hasher.update(name.as_bytes());
            hasher.update(&fingerprint(&dir.join(name)));
        }
        *hasher.finalize().as_bytes()
    }

    /// What a durable write moves.
    #[derive(Debug, PartialEq, Eq)]
    struct Observables {
        frontier: ReplicaFrontier,
        store: [u8; 32],
    }

    fn snapshot(station: &Station) -> Observables {
        Observables {
            frontier: station.frontier(),
            store: fingerprint(station.store_dir()),
        }
    }

    /// How many records arrived on an already-open stream.
    ///
    /// One stream, held across the whole test, and never a fresh
    /// `observe(None)` per measurement. A fresh stream yields exactly one reset
    /// record and then waits for live delivery, so counting a fresh one always
    /// returns one — which is how the first version of this passed its positive
    /// case and failed its own negative control.
    fn drain(stream: &mut runtime::world::ObservationStream) -> usize {
        let mut seen = 0usize;
        while stream.try_next().is_ok_and(|record| record.is_some()) {
            seen += 1;
            if seen > 4096 {
                break;
            }
        }
        seen
    }

    fn signals() -> Vec<Signal> {
        vec![
            Signal::Ping { nonce: [1u8; 16] },
            Signal::Acknowledge { nonce: [1u8; 16] },
            Signal::Attention {
                scope: runtime::transient::Target::Body {
                    world: "dev.example.kv".into(),
                    body: [2u8; 16],
                },
            },
            Signal::FileOffer {
                content: [3u8; 32],
                plaintext_len: 4096,
                display_name: "notes.txt".into(),
                media_type: "text/plain".into(),
            },
        ]
    }

    fn drive(station: &Station, rounds: usize) {
        let from =
            Key::from_device(&mechanics::actor::device_from_seed(&[91u8; 32])).expect("station");
        let live = station.live();
        for round in 0..rounds {
            for signal in signals() {
                live.deliver(DeliveredSignal {
                    from: from.clone(),
                    connection_id: [(round % 251) as u8; 16],
                    connection_epoch: [(round % 249) as u8; 16],
                    signal,
                });
            }
        }
    }

    #[test]
    fn ten_thousand_delivered_signals_move_nothing_durable() {
        let root = temp_root();
        let station = station_at(&root);
        let (session, _writer) = dock(&station);

        let mut stream = session.observe(None);
        drain(&mut stream);
        let before = snapshot(&station);

        drive(&station, 2_500);

        let after = snapshot(&station);
        assert_eq!(
            before.frontier, after.frontier,
            "a signal is not a commit, so the frontier does not move"
        );
        assert_eq!(
            before.store, after.store,
            "and nothing under the store directory changed — not the journal, \
             not the objects, not the manifest"
        );
        assert_eq!(
            drain(&mut stream),
            0,
            "and no Observation was published, which is the whole of 'not activity': \
             StationHost derives activity_advanced from an Observation's scopes, and \
             there is no other route in"
        );
    }

    #[test]
    fn one_ordinary_commit_moves_all_three() {
        // The negative control, and the reason the test above is evidence rather
        // than decoration. A run that passes because nothing was driven is
        // indistinguishable from a run that passes because signals are not
        // durable — unless something in the same file shows the observables
        // moving when they should.
        let root = temp_root();
        let station = station_at(&root);
        let (session, writer) = dock(&station);

        let mut stream = session.observe(None);
        drain(&mut stream);
        let before = snapshot(&station);
        let signed = writer
            .sign_action(
                &session,
                RequestId::from_bytes([9u8; 16]),
                Intent {
                    schema: SchemaId::parse("entry").unwrap(),
                    schema_version: 1,
                    payload: b"k=v".to_vec(),
                },
            )
            .expect("signed");
        session.submit(signed).expect("committed");

        let after = snapshot(&station);
        assert_ne!(
            before.frontier, after.frontier,
            "a commit moves the frontier"
        );
        assert_ne!(before.store, after.store, "and writes bytes");
        assert_eq!(
            drain(&mut stream),
            1,
            "and publishes exactly one Observation"
        );
    }

    #[test]
    fn a_restart_delivers_nothing_that_was_signalled_before_it() {
        // The third property is a consequence rather than a mechanism:
        // `Orbit::activate` builds a fresh core and reads nothing signal-shaped
        // from disk. Asserted anyway, because "we never wrote it" and "we never
        // read it back" fail independently.
        let root = temp_root();
        let station = station_at(&root);
        let frontier_before = station.frontier();
        drive(&station, 100);
        let durable_before = durable_fingerprint(station.store_dir());

        let orbit = station.vacate().expect("dormant");
        let station = orbit.open(options()).expect("reactivated");

        let mut listener = station.signals();
        assert!(
            listener.try_recv().is_err(),
            "a restart replays nothing: a signal is an event, and the event is over"
        );
        assert_eq!(
            durable_before,
            durable_fingerprint(station.store_dir()),
            "and nothing a signal touched was flushed to disk on the way out"
        );
        assert_eq!(
            frontier_before,
            station.frontier(),
            "the frontier survives the restart unchanged, because nothing moved it"
        );
    }

    #[test]
    fn a_subscriber_that_stops_reading_changes_nothing_for_the_sender() {
        // Delivery failure must not be observable. Zero subscribers and a lagged
        // ring are local facts; if either changed what a peer sees, a peer could
        // learn whether a viewer is open by pinging with an `Attention`.
        let root = temp_root();
        let station = station_at(&root);
        let (session, _writer) = dock(&station);

        let mut stream = session.observe(None);
        drain(&mut stream);
        let quiet = snapshot(&station);

        drive(&station, 50);
        assert_eq!(quiet, snapshot(&station), "nobody listening");

        let _listener = station.signals();
        drive(&station, 50);
        assert_eq!(quiet, snapshot(&station), "somebody listening");

        // A subscriber that never reads, well past the ring bound.
        let _lagged = station.signals();
        drive(&station, 500);
        assert_eq!(quiet, snapshot(&station), "somebody lagged");
        assert_eq!(drain(&mut stream), 0);
    }
}
