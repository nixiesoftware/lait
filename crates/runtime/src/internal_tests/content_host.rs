//! Plan 13 F5 exit — the product-neutral host surface, and what it withholds.
//!
//! Two things are being tested. That a caller can ingest, name, and read
//! content through a surface that never mentions a path or hands back a whole
//! file. And that every operation is a separate authorization question — a
//! `ContentRef` is a name, and holding one proves nothing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mechanics::authorization::AuthorizedBodyKey;
use mechanics::ids::SpaceId;
use replica::content::ContentRef;
use replica::content::Residency;
use replica::content::CHUNK_PLAINTEXT_LEN;
use runtime::content_host::{
    ContentAction, ContentHost, ContentKeys, ContentPolicy, Failure, MAX_RANGE_BYTES,
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
///
/// Keyed by capability so a test refuses "serving" without naming which bytes,
/// while `asked` keeps the content each gate was told about.
#[derive(Default)]
struct Authorizer {
    asked: Mutex<Vec<(&'static str, Option<ContentRef>)>>,
    refuse: Vec<&'static str>,
}

impl Authorizer {
    fn check(&self, action: ContentAction<'_>) -> Result<(), Vec<u8>> {
        let capability = action.capability();
        self.asked
            .lock()
            .unwrap()
            .push((capability, action.content().copied()));
        if self.refuse.contains(&capability) {
            Err(capability.as_bytes().to_vec())
        } else {
            Ok(())
        }
    }

    /// The capabilities asked about, in order.
    fn capabilities(&self) -> Vec<&'static str> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(|(capability, _)| *capability)
            .collect()
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
    core: Arc<runtime::session::StationCore>,
    dir: PathBuf,
}

fn fixture(tag: &str) -> Fixture {
    let dir = temp_dir(tag);
    let core = runtime::session::StationCore::for_test(
        replica::Replica::open(
            dir.join("store"),
            Arc::new(replica::body::StaticBodyKeys::new(
                AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY),
            )),
        )
        .unwrap(),
    );
    let cache = Arc::new(Residency::open(dir.join("cache"), 1 << 30).unwrap());
    let core = Arc::new(core);
    Fixture {
        host: ContentHost::new(core.clone(), cache),
        core,
        dir,
    }
}

fn policy<'a>(
    space: &'a SpaceId,
    authorize: &'a dyn for<'c> Fn(ContentAction<'c>) -> Result<(), Vec<u8>>,
) -> ContentPolicy<'a> {
    ContentPolicy {
        space,
        keys: Arc::new(Keys),
        authorize,
        max_content_len: u64::MAX,
    }
}

fn commit_ctx<'a>(
    signer: &'a replica::transaction::SeedSigner<'a>,
    space: &'a SpaceId,
) -> replica::transaction::CommitContext<'a> {
    replica::transaction::CommitContext {
        space,
        signer,
        authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![9]),
    }
}

#[test]
fn content_ingests_from_a_reader_and_reads_back_in_ranges() {
    let fx = fixture("range");
    let space = space();
    let auth = Authorizer::default();
    let check = |a: ContentAction<'_>| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);

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
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);

    let permissive = Authorizer::default();
    let allow = |a: ContentAction<'_>| permissive.check(a);
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
        permissive.capabilities().as_slice(),
        &["content.publish"],
        "ingest asks exactly once, about publishing"
    );

    let strict = Authorizer {
        refuse: vec!["content.read", "content.pin", "content.remove-local"],
        ..Default::default()
    };
    let refuse = |a: ContentAction<'_>| strict.check(a);
    let policy = policy(&space, &refuse);
    assert!(matches!(
        fx.host.stat(&policy, &content),
        Err(Failure::Denied { .. })
    ));
    assert!(matches!(
        fx.host.read_range(&policy, &content, 0, 1),
        Err(Failure::Denied { .. })
    ));
    assert!(matches!(
        fx.host.pin(&policy, &content),
        Err(Failure::Denied { .. })
    ));
    assert!(matches!(
        fx.host.remove_local(&policy, &content),
        Err(Failure::Denied { .. })
    ));

    // And a refusal says what would have been needed, so a caller can explain
    // rather than only report.
    let Err(Failure::Denied { demand }) = fx.host.read_range(&policy, &content, 0, 1) else {
        panic!("expected a denial");
    };
    assert_eq!(demand, b"content.read");
}

#[test]
fn a_range_larger_than_the_bound_is_refused_before_anything_is_read() {
    let fx = fixture("bound");
    let space = space();
    let auth = Authorizer::default();
    let check = |a: ContentAction<'_>| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);
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
        Err(Failure::Bounds)
    );
}

#[test]
fn ingest_past_the_operator_ceiling_is_refused_and_leaves_nothing() {
    let fx = fixture("ceiling");
    let space = space();
    let auth = Authorizer::default();
    let check = |a: ContentAction<'_>| auth.check(a);
    let mut policy = policy(&space, &check);
    policy.max_content_len = 1_000;
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);

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
    let check = |a: ContentAction<'_>| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);
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
        Err(Failure::NotResident),
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
    let check = |a: ContentAction<'_>| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);
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
    let check = |a: ContentAction<'_>| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);

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
        refuse: vec!["content.serve"],
        ..Default::default()
    };
    let deny = |a: ContentAction<'_>| refuse.check(a);
    let denied = ContentPolicy {
        space: &space,
        keys: Arc::new(Keys),
        authorize: &deny,
        max_content_len: u64::MAX,
    };
    assert!(matches!(
        fx.host.chunk(&denied, &wanted, 0),
        Err(Failure::Denied { .. })
    ));
    assert!(matches!(
        fx.host.resident_indices(&denied, &wanted),
        Err(Failure::Denied { .. })
    ));
    // Reading locally still works, because that is a different permission.
    assert!(fx.host.stat(&denied, &wanted).is_ok());
}

#[test]
fn a_pin_is_reported_by_stat() {
    let fx = fixture("pinned");
    let space = space();
    let auth = Authorizer::default();
    let check = |a: ContentAction<'_>| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);

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

#[test]
fn an_availability_answer_costs_what_was_asked_not_what_is_held() {
    // A peer naming three indices must cost three checks even when the content
    // has four million. A request that can be turned into work by being about
    // something large is a request that can be turned into a denial of service.
    let fx = fixture("among");
    let space = space();
    let auth = Authorizer::default();
    let check = |a: ContentAction<'_>| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);

    let content = fx
        .host
        .ingest(
            &policy,
            [1u8; 16],
            &mut std::io::Cursor::new(filler(1, CHUNK_PLAINTEXT_LEN as usize * 3 + 10)),
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");

    assert_eq!(
        fx.host.resident_among(&policy, &content, &[2, 0]).unwrap(),
        vec![0, 2],
        "answered in canonical order, and only about what was asked"
    );
    assert_eq!(
        fx.host
            .resident_among(&policy, &content, &[1, 1, 1])
            .unwrap(),
        vec![1],
        "a repeated index is one answer"
    );
    assert!(
        fx.host
            .resident_among(&policy, &content, &[9_999])
            .unwrap()
            .is_empty(),
        "an index past the geometry is simply absent"
    );

    // An unknown content answers exactly as a known-but-absent one does.
    let unknown = replica::content::ContentRef {
        content_id: [0xAB; 32],
    };
    assert!(fx
        .host
        .resident_among(&policy, &unknown, &[0, 1, 2])
        .unwrap()
        .is_empty());
}

#[test]
fn a_ranged_chunk_carries_the_proof_for_the_whole_chunk() {
    // A transfer that dies at 90% of a chunk should resume at 90%. The proof
    // covers the whole chunk regardless of the range, which is what lets the
    // resuming peer check it is still talking about the same bytes before it
    // appends any.
    let fx = fixture("ranged");
    let space = space();
    let auth = Authorizer::default();
    let check = |a: ContentAction<'_>| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);

    let content = fx
        .host
        .ingest(
            &policy,
            [1u8; 16],
            &mut std::io::Cursor::new(filler(1, 40_000)),
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");

    let (whole, proof, total) = fx
        .host
        .chunk_range(&policy, &content, 0, 0, 1 << 20)
        .unwrap();
    assert_eq!(whole.len(), total as usize);
    let (tail, tail_proof, tail_total) = fx
        .host
        .chunk_range(&policy, &content, 0, total as u64 - 100, 1 << 20)
        .unwrap();
    assert_eq!(tail, whole[whole.len() - 100..]);
    assert_eq!(tail_proof, proof, "the proof does not depend on the range");
    assert_eq!(tail_total, total);

    // Past the end is a bound, not an empty answer: a peer asking beyond a
    // chunk it is resuming has lost track of where it is.
    assert!(matches!(
        fx.host
            .chunk_range(&policy, &content, 0, total as u64 + 1, 16),
        Err(Failure::Bounds)
    ));
}

#[test]
fn installing_one_staged_chunk_leaves_the_rest_of_the_transfer_alone() {
    // The blocking defect the review found. `discard_staged` is prefix-matched
    // over the whole operation, so installing chunk 3 of an eight-way transfer
    // would have deleted the partials for every other chunk in flight.
    let sender = fixture("staged-sender");
    let receiver = fixture("staged-receiver");
    let space = space();
    let auth = Authorizer::default();
    let check = |a: ContentAction<'_>| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);

    let plaintext = filler(3, CHUNK_PLAINTEXT_LEN as usize * 2 + 500);
    let content = sender
        .host
        .ingest(
            &policy,
            [1u8; 16],
            &mut std::io::Cursor::new(plaintext.clone()),
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");
    // The receiver needs the descriptor to judge anything.
    let descriptor = sender.host.descriptor_of(&policy, &content).unwrap();
    assert_eq!(descriptor.chunk_count, 3);
    receiver
        .core
        .with_replica_metadata(|replica| {
            replica.commit_content(
                &commit_ctx(&signer, &space),
                std::slice::from_ref(&descriptor),
            )
        })
        .expect("the receiver commits the descriptor it learned");

    let operation = [9u8; 16];
    // Stage all three chunks, as a real transfer would.
    let mut proofs = Vec::new();
    for index in 0..3u32 {
        let (bytes, proof, _) = sender
            .host
            .chunk_range(&policy, &content, index, 0, 1 << 20)
            .unwrap();
        receiver
            .host
            .cache()
            .append_staged(&operation, index, 0, &bytes)
            .unwrap();
        proofs.push(proof);
    }
    let staged_before = receiver.host.cache().staged_bytes();

    receiver
        .host
        .install_staged_chunk(&policy, &content, operation, 1, &proofs[1])
        .expect("install the middle chunk");

    assert_eq!(
        receiver.host.resident_indices(&policy, &content).unwrap(),
        vec![1]
    );
    assert!(
        receiver.host.cache().staged_len(&operation, 0) > 0
            && receiver.host.cache().staged_len(&operation, 2) > 0,
        "the other chunks are still staged"
    );
    assert!(receiver.host.cache().staged_bytes() < staged_before);
}

#[test]
fn a_range_read_costs_the_span_and_not_the_content() {
    // The same rule `resident_among` follows, applied to reading. A seek into
    // one chunk of a large file must cost one chunk's worth of questions —
    // otherwise every Range request a browser makes while scrubbing a video is
    // a full walk of the content, and the walk grows with the file.
    let fx = fixture("span-cost");
    let space = space();
    let auth = Authorizer::default();
    let check = |a: ContentAction<'_>| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);

    let chunk = CHUNK_PLAINTEXT_LEN as usize;
    let plaintext = filler(4, chunk * 8 + 64);
    let content = fx
        .host
        .ingest(
            &policy,
            [4u8; 16],
            &mut std::io::Cursor::new(plaintext.clone()),
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");
    let descriptor = fx.host.descriptor_of(&policy, &content).unwrap();
    assert_eq!(descriptor.chunk_count, 9);

    // One chunk's worth of bytes, entirely inside chunk 3.
    let before = fx.host.cache().residency_probes();
    let got = fx
        .host
        .read_range(&policy, &content, (chunk * 3) as u64, 100)
        .expect("read");
    let spanned = fx.host.cache().residency_probes() - before;
    assert_eq!(got, plaintext[chunk * 3..chunk * 3 + 100]);
    assert_eq!(
        spanned, 1,
        "a range inside one chunk asked about one chunk, not all {}",
        descriptor.chunk_count
    );

    // A span crossing a boundary costs both, and still nothing else.
    let before = fx.host.cache().residency_probes();
    fx.host
        .read_range(&policy, &content, (chunk * 2 - 10) as u64, 20)
        .expect("read across the seam");
    assert_eq!(fx.host.cache().residency_probes() - before, 2);

    // `stat` is the one call that genuinely walks everything, because
    // "how much of this is here" is a question about all of it.
    let before = fx.host.cache().residency_probes();
    let status = fx.host.stat(&policy, &content).unwrap();
    assert_eq!(status.resident_chunks, descriptor.chunk_count);
    assert!(
        fx.host.cache().residency_probes() - before >= descriptor.chunk_count as u64,
        "stat is allowed to walk; the read path is not"
    );
}

#[test]
fn a_hole_in_the_span_is_reported_before_any_chunk_is_opened() {
    // A missing chunk is an answer about the request, not a failure partway
    // through serving it. Deciding first means the work of opening, verifying,
    // and decrypting the chunks before the hole is never done — and on a
    // streaming surface, that a status line is not sent before the failure is
    // known.
    //
    // Observed without instrumenting the open path: chunk 0 is left resident
    // but corrupt, so opening it would fail loudly and differently. Reaching
    // `NotResident` proves it was never opened.
    let fx = fixture("hole-first");
    let space = space();
    let auth = Authorizer::default();
    let check = |a: ContentAction<'_>| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);

    let chunk = CHUNK_PLAINTEXT_LEN as usize;
    let content = fx
        .host
        .ingest(
            &policy,
            [5u8; 16],
            &mut std::io::Cursor::new(filler(5, chunk * 2 + 8)),
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");
    let descriptor = fx.host.descriptor_of(&policy, &content).unwrap();
    assert_eq!(descriptor.chunk_count, 3);

    // Chunk 2 goes missing; chunk 0 stays present and is made unopenable.
    let cache = fx.host.cache();
    cache.release_content(&descriptor.content_nonce).unwrap();
    cache
        .evict(&replica::content::chunk_slot(&descriptor, 2))
        .unwrap();
    // The cache files an entry under its slot's hex, in `chunks/`.
    let first = fx
        .dir
        .join("cache/chunks")
        .join(data_encoding::HEXLOWER.encode(&replica::content::chunk_slot(&descriptor, 0)));
    let mut corrupt = std::fs::read(&first).unwrap();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 0xFF;
    std::fs::write(&first, &corrupt).unwrap();

    // Reading chunk 0 alone finds the corruption — proof the trap is armed.
    assert!(
        !matches!(
            fx.host.read_range(&policy, &content, 0, 16),
            Err(Failure::NotResident)
        ),
        "chunk 0 is resident, so its own failure must not be NotResident"
    );

    // Reading a span that reaches the hole never gets that far.
    assert!(
        matches!(
            fx.host.read_range(&policy, &content, 0, chunk * 3),
            Err(Failure::NotResident)
        ),
        "the hole is the answer, and it arrives before chunk 0 is touched"
    );
}

#[test]
fn an_ingest_holds_its_content_until_something_declares_it() {
    // The window this closes is between two calls that a person stands
    // between: the upload finishes, and then they choose an issue. Nothing on
    // disk tells a sweep those two are related, so `ingest` says so.
    let fx = fixture("pending");
    let space = space();
    let auth = Authorizer::default();
    let check = |a: ContentAction<'_>| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);
    let ctx = commit_ctx(&signer, &space);

    let content = fx
        .host
        .ingest(
            &policy,
            [7u8; 16],
            &mut std::io::Cursor::new(filler(7, 4096)),
            &ctx,
        )
        .expect("ingest");

    // A sweep landing in the window collects nothing, and the content is still
    // readable afterwards — the bytes were never at risk, the descriptor was.
    let collected = fx
        .core
        .with_replica_metadata(|replica| replica.sweep_unreferenced_content(&ctx, None))
        .expect("sweep");
    assert!(
        collected.is_empty(),
        "a fresh upload is not garbage: {collected:?}"
    );
    assert_eq!(
        fx.host.read_range(&policy, &content, 0, 16).unwrap(),
        filler(7, 4096)[..16]
    );

    // Let go of the hold and the same sweep collects, so the window is a
    // window and not a permanent exemption.
    fx.core
        .with_replica_control(|replica| {
            replica.release_content_hold(&content);
            Ok(())
        })
        .unwrap();
    assert_eq!(
        fx.core
            .with_replica_metadata(|replica| replica.sweep_unreferenced_content(&ctx, None))
            .unwrap(),
        vec![content]
    );
}

#[test]
fn a_zero_length_read_is_an_empty_answer_and_not_an_underflow() {
    // A span of nothing has no last chunk. Without the guard, `end - 1`
    // underflows: a panic under overflow-checks, and in release a `last` of
    // u32::MAX that walks the whole chunk space and reports a fully resident
    // content as NotResident.
    //
    // Answered rather than refused, because asking for zero bytes is a legal
    // thing to do — a loop whose remaining length has reached zero asks exactly
    // this, and every caller of a ranged read eventually has one.
    let fx = fixture("zero-span");
    let space = space();
    let auth = Authorizer::default();
    let check = |a: ContentAction<'_>| auth.check(a);
    let policy = policy(&space, &check);
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);
    let content = fx
        .host
        .ingest(
            &policy,
            [8u8; 16],
            &mut std::io::Cursor::new(filler(8, 4096)),
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");

    assert_eq!(
        fx.host.read_range(&policy, &content, 0, 0).unwrap(),
        Vec::<u8>::new(),
        "zero bytes from the start"
    );
    assert_eq!(
        fx.host.read_range(&policy, &content, 100, 0).unwrap(),
        Vec::<u8>::new(),
        "zero bytes from the middle"
    );
    // And it costs nothing to answer: a span of nothing asks about no chunks.
    let before = fx.host.cache().residency_probes();
    let _ = fx.host.read_range(&policy, &content, 0, 0).unwrap();
    assert_eq!(fx.host.cache().residency_probes(), before);
}

#[test]
fn serving_can_be_refused_for_the_bytes_named_and_allowed_for_others() {
    // The gate is told which content it is about, so a predicate can scope to
    // the bytes rather than only to the Space. Nothing shipping scopes yet --
    // no member holds `content.serve` -- but a plane that cannot express the
    // decision could not adopt the grant without changing shape.
    let fx = fixture("serve-scope");
    let space = space();
    let signer = replica::transaction::SeedSigner(&WRITER_SEED);

    let permissive = Authorizer::default();
    let allow = |a: ContentAction<'_>| permissive.check(a);
    let open = policy(&space, &allow);

    let withheld = fx
        .host
        .ingest(
            &open,
            [1u8; 16],
            &mut std::io::Cursor::new(filler(1, 64)),
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");
    let shared = fx
        .host
        .ingest(
            &open,
            [2u8; 16],
            &mut std::io::Cursor::new(filler(2, 64)),
            &commit_ctx(&signer, &space),
        )
        .expect("ingest");

    let scoped = |a: ContentAction<'_>| match a {
        ContentAction::Serve(content) if content == &withheld => {
            Err(a.capability().as_bytes().to_vec())
        }
        _ => Ok(()),
    };
    let policy = policy(&space, &scoped);

    assert!(
        matches!(
            fx.host.chunk(&policy, &withheld, 0),
            Err(Failure::Denied { .. })
        ),
        "the named content is refused"
    );
    let (bytes, _) = fx.host.chunk(&policy, &shared, 0).expect("serve the other");
    assert!(!bytes.is_empty(), "an unnamed content is unaffected");

    // The other gates on the same bytes are a separate question, and refusing
    // to serve is not refusing to read.
    assert!(fx.host.read_range(&policy, &withheld, 0, 8).is_ok());
}
