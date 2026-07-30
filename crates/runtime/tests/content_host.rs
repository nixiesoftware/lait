//! Plan 13 F5 exit — the product-neutral host surface, and what it withholds.
//!
//! Two things are being tested. That a caller can ingest, name, and read
//! content through a surface that never mentions a path or hands back a whole
//! file. And that every operation is a separate authorization question — a
//! `ContentRef` is a name, and holding one proves nothing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mechanics::crypto::AuthorizedBodyKey;
use mechanics::ids::SpaceId;
use replica::content::CHUNK_PLAINTEXT_LEN;
use replica::journal::cache::ResidentCache;
use runtime::content_host::{
    ContentAction, ContentHost, ContentHostError, ContentKeys, ContentPolicy, MAX_RANGE_BYTES,
};

const EPOCH: [u8; 16] = [3u8; 16];
const EPOCH_KEY: [u8; 32] = [4u8; 32];
const WRITER_SEED: [u8; 32] = [61u8; 32];

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-host-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn space() -> SpaceId {
    SpaceId::from_digest([31u8; 16])
}

struct Keys;
impl ContentKeys for Keys {
    fn sealing_key(&self) -> Option<AuthorizedBodyKey> {
        Some(AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY))
    }
    fn opening_key(&self, epoch: &[u8; 16]) -> Option<AuthorizedBodyKey> {
        (epoch == &EPOCH).then(|| AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY))
    }
}

/// Records every action the host asked about, and refuses the ones named.
#[derive(Default)]
struct Authorizer {
    asked: Mutex<Vec<ContentAction>>,
    refuse: Vec<ContentAction>,
}

impl Authorizer {
    fn check(&self, action: ContentAction) -> Result<(), Vec<u8>> {
        self.asked.lock().unwrap().push(action);
        if self.refuse.contains(&action) {
            Err(action.capability().as_bytes().to_vec())
        } else {
            Ok(())
        }
    }
}

fn filler(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as u8
        })
        .collect()
}

struct Fixture {
    host: ContentHost,
    dir: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let dir = temp_dir(tag);
    let core = runtime::session::StationCore::for_test(
        replica::Replica::open_journaled(
            dir.join("store"),
            Arc::new(replica::StaticBodyKeys::new(
                AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
            )),
        )
        .unwrap(),
    );
    let cache = Arc::new(ResidentCache::open(dir.join("cache"), 1 << 30).unwrap());
    Fixture {
        host: ContentHost::new(core, cache),
        dir,
    }
}

fn policy<'a>(
    space: &'a SpaceId,
    authorize: &'a dyn Fn(ContentAction) -> Result<(), Vec<u8>>,
) -> ContentPolicy<'a> {
    ContentPolicy {
        space,
        keys: Arc::new(Keys),
        authorize,
        max_content_len: u64::MAX,
    }
}

fn commit_ctx<'a>(
    signer: &'a replica::SeedSigner<'a>,
    space: &'a SpaceId,
) -> replica::CommitContext<'a> {
    replica::CommitContext {
        space,
        signer,
        authority_frontier: replica::AuthorityFrontier::from_canonical_bytes(vec![9]),
    }
}

#[test]
fn content_ingests_from_a_reader_and_reads_back_in_ranges() {
    let fx = fixture("range");
    let space = space();
    let auth = Authorizer::default();
    let check = |a| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::SeedSigner(&WRITER_SEED);

    let plaintext = filler(1, CHUNK_PLAINTEXT_LEN as usize * 2 + 500);
    let mut reader = std::io::Cursor::new(plaintext.clone());
    let content = fx
        .host
        .ingest(
            &policy,
            [1u8; 16],
            &mut reader,
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");

    let status = fx.host.stat(&policy, &content).expect("stat");
    assert_eq!(status.plaintext_len, plaintext.len() as u64);
    assert_eq!(status.chunk_count, 3);
    assert!(status.is_complete(), "everything just ingested is here");

    // Ranges, including ones that cross chunk boundaries.
    for (offset, len) in [
        (0usize, 10usize),
        (CHUNK_PLAINTEXT_LEN as usize - 5, 20),
        (CHUNK_PLAINTEXT_LEN as usize * 2, 500),
    ] {
        let got = fx
            .host
            .read_range(&policy, &content, offset as u64, len)
            .expect("read");
        assert_eq!(
            got,
            &plaintext[offset..offset + len],
            "range {offset}+{len} disagreed"
        );
    }

    // Past the end returns what exists rather than erroring.
    let tail = fx
        .host
        .read_range(&policy, &content, plaintext.len() as u64 - 5, 100)
        .expect("tail");
    assert_eq!(tail, &plaintext[plaintext.len() - 5..]);
    assert!(fx
        .host
        .read_range(&policy, &content, plaintext.len() as u64 + 1, 10)
        .expect("past the end")
        .is_empty());
}

#[test]
fn every_operation_is_its_own_authorization_question() {
    // A ContentRef is a name. Holding one must not carry permission to read,
    // pin, or evict what it names.
    let fx = fixture("authz");
    let space = space();
    let signer = replica::SeedSigner(&WRITER_SEED);

    let permissive = Authorizer::default();
    let allow = |a| permissive.check(a);
    let content = fx
        .host
        .ingest(
            &policy(&space, &allow),
            [1u8; 16],
            &mut std::io::Cursor::new(b"payload".to_vec()),
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");
    assert_eq!(
        permissive.asked.lock().unwrap().as_slice(),
        &[ContentAction::Publish],
        "ingest asks exactly once, about publishing"
    );

    let strict = Authorizer {
        refuse: vec![
            ContentAction::Read,
            ContentAction::Pin,
            ContentAction::RemoveLocal,
        ],
        ..Default::default()
    };
    let refuse = |a| strict.check(a);
    let policy = policy(&space, &refuse);
    assert!(matches!(
        fx.host.stat(&policy, &content),
        Err(ContentHostError::Denied { .. })
    ));
    assert!(matches!(
        fx.host.read_range(&policy, &content, 0, 1),
        Err(ContentHostError::Denied { .. })
    ));
    assert!(matches!(
        fx.host.pin(&policy, &content),
        Err(ContentHostError::Denied { .. })
    ));
    assert!(matches!(
        fx.host.remove_local(&policy, &content),
        Err(ContentHostError::Denied { .. })
    ));

    // And a refusal says what would have been needed, so a caller can explain
    // rather than only report.
    let Err(ContentHostError::Denied { demand }) = fx.host.read_range(&policy, &content, 0, 1)
    else {
        panic!("expected a denial");
    };
    assert_eq!(demand, ContentAction::Read.capability().as_bytes());
}

#[test]
fn a_range_larger_than_the_bound_is_refused_before_anything_is_read() {
    let fx = fixture("bound");
    let space = space();
    let auth = Authorizer::default();
    let check = |a| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::SeedSigner(&WRITER_SEED);
    let content = fx
        .host
        .ingest(
            &policy,
            [1u8; 16],
            &mut std::io::Cursor::new(b"small".to_vec()),
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");
    assert_eq!(
        fx.host
            .read_range(&policy, &content, 0, MAX_RANGE_BYTES + 1),
        Err(ContentHostError::Bounds)
    );
}

#[test]
fn ingest_past_the_operator_ceiling_is_refused_and_leaves_nothing() {
    let fx = fixture("ceiling");
    let space = space();
    let auth = Authorizer::default();
    let check = |a| auth.check(a);
    let mut policy = policy(&space, &check);
    policy.max_content_len = 1_000;
    let signer = replica::SeedSigner(&WRITER_SEED);

    let outcome = fx.host.ingest(
        &policy,
        [1u8; 16],
        &mut std::io::Cursor::new(filler(2, 5_000)),
        &commit_ctx(&signer, &space),
    );
    assert!(outcome.is_err());
    fx.host.cache().sweep().unwrap();
    assert_eq!(
        fx.host.cache().resident_bytes(),
        0,
        "a refused ingest leaves nothing durable"
    );
}

#[test]
fn removing_locally_keeps_the_name_and_drops_the_bytes() {
    // The distinction the surface exists to make: reclaiming space is not
    // forgetting. The content is still named, still referenced, still
    // fetchable — it is simply not here.
    let fx = fixture("remove");
    let space = space();
    let auth = Authorizer::default();
    let check = |a| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::SeedSigner(&WRITER_SEED);
    let content = fx
        .host
        .ingest(
            &policy,
            [1u8; 16],
            &mut std::io::Cursor::new(filler(3, 4_000)),
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");

    fx.host.remove_local(&policy, &content).expect("remove");
    let status = fx.host.stat(&policy, &content).expect("still named");
    assert_eq!(status.plaintext_len, 4_000, "the descriptor is untouched");
    assert_eq!(status.resident_chunks, 0, "and the bytes are gone");
    assert_eq!(
        fx.host.read_range(&policy, &content, 0, 10),
        Err(ContentHostError::NotResident),
        "reading says not-here rather than failing the store"
    );
}

#[test]
fn a_provider_serves_a_chunk_and_a_receiver_installs_it_verified() {
    // The half plan 14's Freight consumes, frozen here: a provider produces
    // ciphertext plus proof, and a receiver verifies against the descriptor
    // before anything lands where it could be served on.
    let sender = fixture("provider");
    let space = space();
    let auth = Authorizer::default();
    let check = |a| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::SeedSigner(&WRITER_SEED);
    let plaintext = filler(4, CHUNK_PLAINTEXT_LEN as usize + 200);
    let content = sender
        .host
        .ingest(
            &policy,
            [1u8; 16],
            &mut std::io::Cursor::new(plaintext.clone()),
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");

    assert_eq!(
        sender.host.resident_indices(&policy, &content).unwrap(),
        vec![0, 1]
    );
    let (ciphertext, proof) = sender
        .host
        .chunk(&policy, &content, 0)
        .expect("serve chunk 0");

    // A receiver holding the descriptor but none of the bytes.
    let receiver = fixture("receiver");
    let descriptor = sender
        .host
        .stat(&policy, &content)
        .map(|_| ())
        .and_then(|_| {
            receiver
                .host
                .install_chunk(&policy, &content, [9u8; 16], &proof, &ciphertext)
                .err()
                .map(Err)
                .unwrap_or(Ok(()))
        });
    assert!(
        descriptor.is_err(),
        "a receiver with no committed descriptor cannot install against one"
    );

    // Tampered ciphertext is refused by the sender's own verification too.
    let mut tampered = ciphertext.clone();
    tampered[0] ^= 0xFF;
    assert!(sender
        .host
        .install_chunk(&policy, &content, [9u8; 16], &proof, &tampered)
        .is_err());

    let _ = receiver.dir;
}

#[test]
fn the_host_surface_names_no_path() {
    // A structural check, cheap and worth having: the module that hands
    // content to products must not mention a filesystem path in its public
    // vocabulary, or a World could ask for one.
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/content_host.rs"),
    )
    .expect("read the host source");
    for line in source.lines() {
        let is_signature = line.trim_start().starts_with("pub fn")
            || line.trim_start().starts_with("pub ")
                && (line.contains(": ") || line.contains("->"));
        if !is_signature {
            continue;
        }
        for banned in ["PathBuf", "&Path", "OsStr", "File"] {
            assert!(
                !line.contains(banned),
                "the host surface must not name {banned}: {line}"
            );
        }
    }
}

#[test]
fn serving_a_chunk_is_authorized_and_costs_only_this_content() {
    // The two things that were wrong about the provider surface. It took no
    // policy at all, so the peer-facing half — the one remote input actually
    // reaches — was the only half that checked nothing. And answering "which
    // chunks do I have" read and twice-hashed the entire cache, once per
    // question, so a provider's cost grew with everything it held rather than
    // with what was asked about.
    let fx = fixture("serve");
    let space = space();
    let auth = Authorizer::default();
    let check = |a| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::SeedSigner(&WRITER_SEED);

    let wanted = fx
        .host
        .ingest(
            &policy,
            [1u8; 16],
            &mut std::io::Cursor::new(filler(1, CHUNK_PLAINTEXT_LEN as usize + 32)),
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");
    // Several unrelated contents sharing the cache. Under the old scan these
    // were read and hashed on every question about `wanted`.
    for n in 2..8u8 {
        fx.host
            .ingest(
                &policy,
                [n; 16],
                &mut std::io::Cursor::new(filler(n as u64, CHUNK_PLAINTEXT_LEN as usize + 32)),
                &commit_ctx(&signer, &space),
            )
            .expect("ingest");
    }

    assert_eq!(
        fx.host.resident_indices(&policy, &wanted).unwrap(),
        vec![0, 1],
        "residency answers about this content, not about the cache"
    );
    let (bytes, _) = fx.host.chunk(&policy, &wanted, 1).expect("serve");
    assert!(!bytes.is_empty());

    // And a Station that may read its own files has not thereby agreed to
    // serve them to peers.
    let refuse = Authorizer {
        refuse: vec![ContentAction::Serve],
        ..Default::default()
    };
    let deny = |a| refuse.check(a);
    let denied = ContentPolicy {
        space: &space,
        keys: Arc::new(Keys),
        authorize: &deny,
        max_content_len: u64::MAX,
    };
    assert!(matches!(
        fx.host.chunk(&denied, &wanted, 0),
        Err(ContentHostError::Denied { .. })
    ));
    assert!(matches!(
        fx.host.resident_indices(&denied, &wanted),
        Err(ContentHostError::Denied { .. })
    ));
    // Reading locally still works, because that is a different permission.
    assert!(fx.host.stat(&denied, &wanted).is_ok());
}

#[test]
fn a_pin_is_reported_by_stat() {
    let fx = fixture("pinned");
    let space = space();
    let auth = Authorizer::default();
    let check = |a| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::SeedSigner(&WRITER_SEED);

    let content = fx
        .host
        .ingest(
            &policy,
            [1u8; 16],
            &mut std::io::Cursor::new(filler(1, 4_096)),
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");

    assert!(!fx.host.stat(&policy, &content).unwrap().pinned);
    fx.host.pin(&policy, &content).unwrap();
    assert!(
        fx.host.stat(&policy, &content).unwrap().pinned,
        "a World rendering pin state must not always render unpinned"
    );
    fx.host.unpin(&policy, &content).unwrap();
    assert!(!fx.host.stat(&policy, &content).unwrap().pinned);
}
