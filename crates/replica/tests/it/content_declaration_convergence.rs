//! What happens to a content declaration whose Body the bundle does not change.
//!
//! Rule 8 of `validate_contact` (content completeness) resolves every content id
//! a manifest entry declares, and pushes the descriptors the receiver lacks into
//! the sealed bundle. Rule 8 runs over EVERY entry in the advertised manifest.
//! Incorporation does not: `incorporate_units` returns early when nothing is
//! planned, and `persist` only applies a declaration for a Body the bundle
//! actually touched.
//!
//! A declaration therefore crosses only on the Contact that also moves the
//! Body's material. Once the two replicas hold the same head, the declaration
//! can never cross again — and neither can the descriptor it names.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use mechanics::authorization::AuthorizedBodyKey;
use mechanics::ids::SpaceId;
use replica::body::{BodyBinding, Op, StaticBodyKeys, SupportedSchemas, MUTATION_ATOMIC};
use replica::body::{BodyId, BodyKey, EncodingId, SchemaId, WorldId};
use replica::content::{
    ContentDescriptor, ContentRef, CHUNK_PLAINTEXT_LEN, CONTENT_FORMAT_VERSION,
};
use replica::convergence::{AuthorityBatchReceipt, AuthorityIncorporator, StagedContactMaterial};
use replica::frontier::AuthorityFrontier;
use replica::transaction::{CommitAuthorization, CommitContext, SeedSigner};
use replica::Replica;

const WRITER_SEED: [u8; 32] = [83u8; 32];
const EPOCH: [u8; 16] = [21u8; 16];
const EPOCH_KEY: [u8; 32] = [22u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_store(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-decl-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn space() -> SpaceId {
    SpaceId::from_digest([43u8; 16])
}
fn world() -> WorldId {
    WorldId::parse("com.example.notes").unwrap()
}
fn body(n: u8) -> BodyKey {
    BodyKey::new(world(), BodyId::from_bytes([n; 16]))
}
fn keys() -> Arc<StaticBodyKeys> {
    Arc::new(StaticBodyKeys::new(
        AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
    ))
}
fn device() -> mechanics::ids::DeviceId {
    mechanics::actor::device_from_seed(&WRITER_SEED)
}
fn authority_frontier() -> AuthorityFrontier {
    AuthorityFrontier::from_canonical_bytes(vec![13])
}
fn binding() -> BodyBinding {
    BodyBinding {
        schema: SchemaId::parse("blob").unwrap(),
        schema_version: 1,
        encoding: EncodingId::parse("bytes").unwrap(),
        mutation_model: MUTATION_ATOMIC,
    }
}
fn supported() -> SupportedSchemas {
    let mut s = SupportedSchemas::new();
    s.declare(
        world(),
        SchemaId::parse("blob").unwrap(),
        1,
        EncodingId::parse("bytes").unwrap(),
        MUTATION_ATOMIC,
    );
    s
}
fn demand() -> Vec<u8> {
    use mechanics::authorization::{AuthorizationDemand, PolicyCapability, Resource};
    AuthorizationDemand::require(
        PolicyCapability::new("com.example.notes", "write"),
        Resource::root("com.example.notes"),
    )
    .encode_canonical()
    .expect("canonical demand")
}

struct WriterAuthorized;
impl replica::transaction::AuthoritySource for WriterAuthorized {
    fn signer_authorized(&self, signer: &[u8; 32], _f: &AuthorityFrontier) -> bool {
        *signer == device().key_bytes().unwrap()
    }
}

struct Incorporator;
impl AuthorityIncorporator for Incorporator {
    fn incorporate_authority(
        &mut self,
        records: &[Vec<u8>],
    ) -> Result<AuthorityBatchReceipt, replica::convergence::Failure> {
        Ok(AuthorityBatchReceipt {
            space: space(),
            prior_frontier: AuthorityFrontier::from_canonical_bytes(vec![]),
            resulting_frontier: authority_frontier(),
            batch_digest: *blake3::hash(&records.concat()).as_bytes(),
        })
    }
}

fn open(tag: &str) -> Replica {
    let mut r = Replica::open(temp_store(tag).join("store"), keys()).unwrap();
    r.set_supported(supported());
    r
}

fn commit_blob(r: &mut Replica, seq: u8, key: &BodyKey, bytes: &[u8]) {
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let mut request = [0u8; 16];
    request[0] = seq;
    r.commit_action(
        &ctx,
        &CommitAuthorization {
            actor: "act_0000000000000000000000000000000000000000000000000000000000000000",
            parent_manifest_root: [0u8; 32],
            demand: demand(),
            intent_digest: [7u8; 32],
            authorizer: &replica::transaction::StaticAuthorizer {
                world: world(),
                implementation_id: [0u8; 32],
            },
        },
        &world(),
        &device(),
        &request,
        &[7u8; 32],
        Vec::new(),
        Vec::new(),
        "blob",
        &[(
            key.clone(),
            Op::ReplaceAtomic {
                value: bytes.to_vec(),
            },
        )],
        &[(key.clone(), binding())],
        &[],
    )
    .expect("commit");
}

/// A structurally valid descriptor for `len` plaintext bytes. The bytes never
/// exist — this is the whole point of the plane: a descriptor is required
/// material, its chunks are residency.
fn descriptor_for(nonce: u8, len: u64) -> ContentDescriptor {
    let d = ContentDescriptor {
        format_version: CONTENT_FORMAT_VERSION,
        space: space().as_str().to_string(),
        content_nonce: [nonce; 16],
        plaintext_len: len,
        chunk_plaintext_len: CHUNK_PLAINTEXT_LEN,
        chunk_count: u32::try_from(len.div_ceil(u64::from(CHUNK_PLAINTEXT_LEN)).max(1)).unwrap(),
        ciphertext_merkle_root: [nonce; 32],
        epoch: EPOCH,
    };
    d.validate().expect("a well-formed descriptor");
    d
}

fn attach(r: &mut Replica, key: &BodyKey, descriptor: &ContentDescriptor) -> ContentRef {
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let reference = r
        .commit_content(&ctx, std::slice::from_ref(descriptor))
        .expect("commit descriptor")[0];
    let mut declarations = BTreeMap::new();
    declarations.insert(key.clone(), vec![reference]);
    r.declare_content(&ctx, declarations).expect("declare");
    reference
}

fn stage(r: &Replica) -> StagedContactMaterial {
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let material = r.export_material().unwrap();
    let (root, nodes) = r.export_manifest(&ctx).unwrap();
    let mut authority_records = vec![b"mechanics-authority-record".to_vec()];
    let mut bodies = Vec::new();
    for (tx, payloads) in &material {
        authority_records.push(tx.encode());
        for (key, envelope) in payloads {
            bodies.push((tx.id(), key.clone(), envelope.clone()));
        }
    }
    StagedContactMaterial {
        authority_records,
        manifest_root_bytes: root,
        manifest_nodes: nodes,
        bodies,
    }
}

/// Returns how many descriptors rule 8 decided the receiver still needed.
fn contact(receiver: &mut Replica, staged: &StagedContactMaterial) -> usize {
    let space = space();
    let signer = SeedSigner(&WRITER_SEED);
    let ctx = CommitContext {
        space: &space,
        signer: &signer,
        authority_frontier: authority_frontier(),
    };
    let mut incorporator = Incorporator;
    let bundle = receiver
        .validate_contact(staged, &WriterAuthorized, &mut incorporator)
        .expect("the advertisement validates");
    let wanted = bundle.descriptor_count();
    receiver
        .incorporate_bundle(&ctx, bundle, &WriterAuthorized)
        .expect("incorporates");
    wanted
}

fn published(r: &Replica) -> ([u8; 32], u64) {
    r.published_root().expect("a published catalog root")
}

#[test]
fn a_declaration_crosses_even_when_the_body_does_not() {
    let mut author = open("author");
    let mut peer = open("peer");

    // 1. The Body crosses first, before anything is attached to it. This is
    //    ordinary: an issue exists before someone drops a file on it.
    commit_blob(&mut author, 1, &body(1), b"an issue");
    assert_eq!(contact(&mut peer, &stage(&author)), 0);
    assert_eq!(published(&peer), published(&author), "converged");

    // 2. Now the author attaches. `declare_content` republishes the manifest —
    //    the entry for body(1) gains a content ref — but it does NOT change the
    //    Body's head set, because a declaration is about the Body, not in it.
    let content = attach(&mut author, &body(1), &descriptor_for(1, 4096));
    assert_eq!(author.declared_content(&body(1)), vec![content]);
    assert_ne!(
        published(&peer),
        published(&author),
        "the author's catalog moved"
    );

    // 3. The peer contacts again. Rule 8 resolves the declaration and the sealed
    //    bundle carries the descriptor the peer lacks...
    let wanted = contact(&mut peer, &stage(&author));
    assert_eq!(wanted, 1, "rule 8 put the descriptor in the bundle");

    // ...and incorporation now adopts both, even though not one Body head moved
    // and every guard on the way to `persist` said "nothing changed". Rule 8
    // validated these over the WHOLE manifest; dropping them because no Body
    // moved is what left two converged peers permanently disagreeing.
    assert!(
        peer.content_descriptor(&content).is_some(),
        "the descriptor rule 8 validated must be committed"
    );
    assert_eq!(
        peer.declared_content(&body(1)),
        vec![content],
        "and so must the declaration that named it"
    );
    assert_eq!(
        published(&peer),
        published(&author),
        "the published roots agree again — the whole point"
    );

    // 4. Idempotent: re-contacting neither re-adopts nor drifts the root.
    for _ in 0..3 {
        assert_eq!(
            contact(&mut peer, &stage(&author)),
            0,
            "an adopted declaration is not re-wanted"
        );
        assert_eq!(published(&peer), published(&author));
    }
}

#[test]
fn a_declaration_that_rides_its_own_body_change_crosses_normally() {
    // The control. Nothing here is broken when the Body moves in the same
    // bundle: this is why the ordinary attach-then-sync path looks healthy.
    let mut author = open("control-author");
    let mut peer = open("control-peer");

    commit_blob(&mut author, 1, &body(1), b"an issue");
    let content = attach(&mut author, &body(1), &descriptor_for(2, 4096));
    // The Body moves after the declaration, so its head crosses with it.
    commit_blob(&mut author, 2, &body(1), b"an issue, edited");

    assert_eq!(contact(&mut peer, &stage(&author)), 1);
    assert_eq!(peer.declared_content(&body(1)), vec![content]);
    assert!(peer.content_descriptor(&content).is_some());
    assert_eq!(published(&peer), published(&author), "roots agree");
}
