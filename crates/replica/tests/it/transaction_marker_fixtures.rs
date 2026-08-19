//! Transaction envelope protection-boundary matrix for the semantic-named
//! transaction, `lait/body-transaction/3`.

use mechanics::authorization::{AuthorizationDemand, PolicyCapability, Resource};
use mechanics::ids::SpaceId;
use replica::body::{BodyId, EncodingId, SchemaId, WorldId, MUTATION_ATOMIC};
use replica::frontier::AuthorityFrontier as AF;
use replica::frontier::{AuthorityFrontier, ReplicaFrontier};
use replica::transaction::{
    AuthoritySource, Descriptor, Error, SeedSigner, SignRequest, Transaction, NO_PARENT_ROOT,
};
use replica::transaction::{StaticAuthorizer, TransactionAuthorizer};

const SIGNER_SEED: [u8; 32] = [12u8; 32];

fn space() -> SpaceId {
    SpaceId::from_digest([6u8; 16])
}
fn world() -> WorldId {
    WorldId::parse("com.example.issues").unwrap()
}
fn signer_key() -> [u8; 32] {
    mechanics::actor::device_from_seed(&SIGNER_SEED)
        .key_bytes()
        .unwrap()
}
fn auth() -> AuthorityFrontier {
    AuthorityFrontier::from_canonical_bytes(vec![0xA1, 0xB2])
}
fn demand() -> Vec<u8> {
    AuthorizationDemand::require(
        PolicyCapability::new("com.example.issues", "write"),
        Resource::root("com.example.issues"),
    )
    .encode_canonical()
    .unwrap()
}

fn descriptor(body: [u8; 16], payload: &[u8]) -> Descriptor {
    let base = ReplicaFrontier::EMPTY;
    Descriptor {
        world: world(),
        body: BodyId::from_bytes(body),
        schema: SchemaId::parse("issue").unwrap(),
        schema_version: 1,
        encoding: EncodingId::parse("lait.body.v1").unwrap(),
        mutation_model: MUTATION_ATOMIC,
        base_frontier: base,
        resulting_frontier: ReplicaFrontier::new([9u8; 32], 1),
        material: fabric::Material {
            format_version: fabric::CAUSAL_FORMAT_VERSION,
            checkpoint: fabric::ArtifactRef {
                hash: journal::object_content_hash(payload),
                len: payload.len() as u64,
                epoch: [0u8; 16],
            },
            delta_tail: Vec::new(),
            history_root: None,
            history_count: 0,
            version: fabric::Version::empty(),
            plaintext_size: payload.len() as u64,
        },
    }
}

fn sign(descriptors: Vec<Descriptor>) -> Result<Transaction, mechanics::authorization::Refusal> {
    let authorizer = StaticAuthorizer {
        world: world(),
        implementation_id: [0u8; 32],
    };
    Transaction::sign_with(
        SignRequest {
            space: &space(),
            parent_manifest_root: NO_PARENT_ROOT,
            replica_frontier: ReplicaFrontier::new([1u8; 32], 1),
            authority_frontier: auth(),
            actor: "act_0000000000000000000000000000000000000000000000000000000000000000",
            operation: [6u8; 16],
            intent_digest: [4u8; 32],
            operations_digest: [5u8; 32],
            demand: demand(),
            descriptors,
        },
        &SeedSigner(&SIGNER_SEED),
        |core| authorizer.authorize(core),
    )
}

/// A valid two-descriptor transaction (descriptors sorted by BodyId).
fn valid_tx() -> Transaction {
    sign(vec![
        descriptor([0u8; 16], b"cipher-0"),
        descriptor([1u8; 16], b"cipher-1"),
    ])
    .unwrap()
}

#[test]
fn valid_transaction_verifies_and_roundtrips() {
    let tx = valid_tx();
    tx.verify().unwrap();
    let bytes = tx.encode();
    let back = Transaction::decode_canonical(&bytes).unwrap();
    assert_eq!(tx, back);
    // The id is the full signed-envelope digest and is stable across decode.
    assert_eq!(tx.id(), back.id());
    assert_eq!(back.core.operation, [6u8; 16]);
}

#[test]
fn opaque_artifact_reference_check_needs_no_key() {
    // An opaque retainer validates the protected artifact by content address;
    // no decryption key or plaintext hash is involved.
    let d = descriptor([0u8; 16], b"the-ciphertext");
    let reference = d.artifact_refs().next().unwrap();
    assert_eq!(
        reference.hash,
        journal::object_content_hash(b"the-ciphertext")
    );
    assert_ne!(
        reference.hash,
        journal::object_content_hash(b"other-ciphertext")
    );
}

#[test]
fn version_and_algorithm_rejection() {
    let mut tx = valid_tx();
    tx.core.version = 4;
    assert_eq!(tx.verify(), Err(Error::UnsupportedVersion(4)));
    let mut tx = valid_tx();
    tx.signature_algorithm = 9;
    assert_eq!(tx.verify(), Err(Error::UnsupportedSignatureAlgorithm(9)));
}

#[test]
fn empty_descriptor_set_is_rejected() {
    let tx = sign(vec![]).unwrap();
    assert_eq!(tx.verify(), Err(Error::BadDescriptorCount));
}

#[test]
fn unsorted_or_duplicate_descriptors_are_rejected() {
    // Re-signed with reversed order: the signature is valid but ordering wrong.
    let resigned = sign(vec![
        descriptor([1u8; 16], b"cipher-1"),
        descriptor([0u8; 16], b"cipher-0"),
    ])
    .unwrap();
    assert_eq!(resigned.verify(), Err(Error::UnsortedOrDuplicate));

    // Duplicate key.
    let d = descriptor([0u8; 16], b"x");
    let dup = sign(vec![d.clone(), d]).unwrap();
    assert_eq!(dup.verify(), Err(Error::UnsortedOrDuplicate));
}

#[test]
fn a_tampered_receipt_binding_is_rejected() {
    // The receipt is byte-bound to the core: flip the receipt's core-digest
    // binding and verify refuses (the envelope-positional heir of the old
    // "transplanted descriptor" rule).
    let mut tx = valid_tx();
    let mut receipt =
        mechanics::authorization::AuthorizationReceipt::decode(&tx.authorization_receipt).unwrap();
    receipt.body_transaction_core_digest[0] ^= 0xff;
    tx.authorization_receipt = receipt.encode();
    assert!(matches!(
        tx.verify(),
        Err(Error::ReceiptUnbound(_)) | Err(Error::BadSignature)
    ));
}

/// A stub mechanics authority view: authorizes only a named signer key.
struct OnlyAuthorizes([u8; 32]);
impl AuthoritySource for OnlyAuthorizes {
    fn signer_authorized(&self, signer: &[u8; 32], _frontier: &AF) -> bool {
        *signer == self.0
    }
}

#[test]
fn structural_verify_is_not_an_authority_check() {
    // A structurally valid, correctly-signed transaction passes verify() even
    // when its signer has no standing — this is why retention must use
    // verify_authorized.
    let tx = valid_tx();
    tx.verify().unwrap();

    // Mechanics view that authorizes nobody: the transaction is refused.
    struct Nobody;
    impl AuthoritySource for Nobody {
        fn signer_authorized(&self, _s: &[u8; 32], _f: &AF) -> bool {
            false
        }
    }
    // Matched on the variant with its reason intact: an unauthorized signer is
    // now told apart from the thirteen other ways a receipt can fail to bind,
    // which is the entire reason the reason is carried.
    match tx.verify_authorized(&Nobody) {
        Err(Error::AuthorityUnverified(reason)) => {
            assert!(!reason.is_empty(), "the refusal must say why");
        }
        other => panic!("expected an authority refusal carrying its reason, got {other:?}"),
    }

    // A view that authorizes the actual signer: accepted (the default
    // verify_transaction checks signer standing at the referenced frontier).
    assert!(tx.verify_authorized(&OnlyAuthorizes(signer_key())).is_ok());
}

#[test]
fn tampered_signature_is_rejected() {
    let mut tx = valid_tx();
    tx.signature[0] ^= 0xff;
    assert_eq!(tx.verify(), Err(Error::BadSignature));
}

#[test]
fn trailing_bytes_are_non_canonical() {
    let mut bytes = valid_tx().encode();
    bytes.push(0);
    assert_eq!(
        Transaction::decode_canonical(&bytes),
        Err(Error::NonCanonical)
    );
}
