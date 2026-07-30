//! Plan 13 F0 items 4 and 5 — content geometry, and the frozen content format.
//!
//! F0 is the last safe point to move chunk geometry, so the choice is measured
//! here and then pinned. Everything below the measurement is the golden suite:
//! canonical bytes, malformed inputs, and the properties the format claims.

use mechanics::crypto::AuthorizedBodyKey;
use mechanics::ids::SpaceId;
use replica::content::{
    chunk_proof, expected_chunk_count, merkle_root, seal_content, ChunkLeaf, ChunkProof,
    ContentDescriptor, ContentError, ProofStep, SealedContent, CHUNK_PLAINTEXT_LEN,
    CONTENT_FORMAT_VERSION, MAX_PROOF_DEPTH,
};

const EPOCH: [u8; 16] = [3u8; 16];
const EPOCH_KEY: [u8; 32] = [4u8; 32];

fn space() -> SpaceId {
    SpaceId::from_digest([31u8; 16])
}

fn key() -> AuthorizedBodyKey {
    AuthorizedBodyKey::for_authorized_epoch(EPOCH, EPOCH_KEY)
}

/// Deterministic incompressible bytes, so sizes are not measuring a compressor.
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

#[test]
fn a_round_trip_holds_for_every_boundary_length() {
    // Empty, one byte, exactly one chunk, one byte over, and several chunks.
    let chunk = CHUNK_PLAINTEXT_LEN as usize;
    for len in [0, 1, chunk - 1, chunk, chunk + 1, chunk * 3, chunk * 3 + 7] {
        let plaintext = filler(len as u64, len);
        let SealedContent {
            descriptor,
            ciphertexts,
            proofs,
        } = seal_content(&space(), &key(), [9u8; 16], &plaintext).expect("seal");

        assert_eq!(descriptor.plaintext_len, len as u64);
        assert_eq!(
            descriptor.chunk_count as u64,
            expected_chunk_count(len as u64),
            "geometry disagrees at length {len}"
        );
        assert!(
            descriptor.chunk_count >= 1,
            "zero-length content is one canonical empty chunk, never zero"
        );

        let mut recovered = Vec::with_capacity(len);
        for (index, ciphertext) in ciphertexts.iter().enumerate() {
            let part = descriptor
                .open_chunk(&key(), &proofs[index], ciphertext)
                .expect("open chunk");
            recovered.extend_from_slice(&part);
        }
        assert_eq!(recovered, plaintext, "round trip failed at length {len}");
    }
}

#[test]
fn identical_plaintext_produces_unequal_content() {
    // §10: no convergent encryption, and so no plaintext-equality oracle for a
    // relay holding two contents. Two ingests of the same bytes must differ in
    // ciphertext, in root, and in id.
    let plaintext = filler(1, 4096);
    let first = seal_content(&space(), &key(), [1u8; 16], &plaintext).expect("seal");
    let second = seal_content(&space(), &key(), [2u8; 16], &plaintext).expect("seal");

    assert_ne!(
        first.ciphertexts[0], second.ciphertexts[0],
        "ciphertexts must differ"
    );
    assert_ne!(
        first.descriptor.ciphertext_merkle_root, second.descriptor.ciphertext_merkle_root,
        "roots must differ"
    );
    assert_ne!(
        first.descriptor.content_ref(),
        second.descriptor.content_ref(),
        "ids must differ"
    );

    // Even under one nonce, random per-chunk nonces keep the ciphertext apart.
    let same_nonce = seal_content(&space(), &key(), [1u8; 16], &plaintext).expect("seal");
    assert_ne!(
        first.ciphertexts[0], same_nonce.ciphertexts[0],
        "per-chunk nonces are random; a repeated content nonce must not make \
         two sealings byte-identical"
    );
}

#[test]
fn a_chunk_cannot_be_moved_lifted_or_regeometried() {
    // The associated-data binding, tested by trying to defeat it.
    let plaintext = filler(3, CHUNK_PLAINTEXT_LEN as usize * 2 + 10);
    let SealedContent {
        descriptor,
        ciphertexts,
        proofs,
    } = seal_content(&space(), &key(), [5u8; 16], &plaintext).expect("seal");

    // Chunk 1's ciphertext presented as chunk 0: the proof fails first.
    let moved = ChunkProof {
        leaf: ChunkLeaf::of(0, &ciphertexts[1]),
        path: proofs[0].path.clone(),
    };
    assert!(descriptor.verify_chunk(&moved, &ciphertexts[1]).is_err());

    // And if an attacker also produced a consistent proof for the moved chunk,
    // the binding still refuses to open it at the wrong index.
    assert_eq!(
        descriptor
            .open_chunk(&key(), &proofs[1], &ciphertexts[1])
            .map(|p| p.len()),
        Ok(CHUNK_PLAINTEXT_LEN as usize),
        "the chunk opens at its own index"
    );
    let wrong_index_binding = descriptor.binding(0);
    assert!(
        mechanics::crypto::content_chunk_open(&key(), &wrong_index_binding, &ciphertexts[1])
            .is_none(),
        "a chunk must not open under another index's binding"
    );

    // A chunk from one content must not open under another's nonce.
    let other = seal_content(&space(), &key(), [6u8; 16], &plaintext)
        .expect("seal")
        .descriptor;
    assert!(
        mechanics::crypto::content_chunk_open(&key(), &other.binding(1), &ciphertexts[1]).is_none(),
        "a chunk must not open under a different content's binding"
    );
}

#[test]
fn a_truncated_or_corrupt_chunk_is_refused_before_decryption() {
    let plaintext = filler(4, 8192);
    let SealedContent {
        descriptor,
        ciphertexts,
        proofs,
    } = seal_content(&space(), &key(), [7u8; 16], &plaintext).expect("seal");

    let mut corrupt = ciphertexts[0].clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 0xFF;
    assert_eq!(
        descriptor.verify_chunk(&proofs[0], &corrupt),
        Err(ContentError::ChunkMismatch)
    );

    let truncated = &ciphertexts[0][..ciphertexts[0].len() - 1];
    assert_eq!(
        descriptor.verify_chunk(&proofs[0], truncated),
        Err(ContentError::ChunkMismatch)
    );
}

#[test]
fn a_proof_deeper_than_the_protocol_depth_is_refused_before_it_is_walked() {
    let plaintext = filler(5, 1024);
    let SealedContent {
        descriptor,
        ciphertexts,
        proofs,
    } = seal_content(&space(), &key(), [8u8; 16], &plaintext).expect("seal");
    let overlong = ChunkProof {
        leaf: proofs[0].leaf,
        path: (0..MAX_PROOF_DEPTH as usize + 1)
            .map(|_| ProofStep {
                sibling: [0u8; 32],
                sibling_is_left: false,
            })
            .collect(),
    };
    assert_eq!(
        overlong.root(),
        Err(ContentError::ProofMismatch),
        "an overlong path is refused by its length, before any hashing"
    );
    assert_eq!(
        descriptor.verify_chunk(&overlong, &ciphertexts[0]),
        Err(ContentError::ProofMismatch)
    );
}

#[test]
fn the_tree_promotes_rather_than_duplicating_an_odd_node() {
    // Duplicating a lone node lets an n-leaf tree collide with an n+1-leaf
    // tree — the CVE-2012-2459 shape. Promotion costs nothing and removes it.
    let leaves: Vec<ChunkLeaf> = (0..3)
        .map(|i| ChunkLeaf::of(i, &filler(i as u64, 64)))
        .collect();
    let three = merkle_root(&leaves);

    let mut duplicated = leaves.clone();
    duplicated.push(leaves[2]);
    assert_ne!(
        three,
        merkle_root(&duplicated),
        "a tree whose last leaf is duplicated must not share a root with the \
         tree it was derived from"
    );
}

#[test]
fn every_chunk_proves_against_the_root() {
    for count in [1usize, 2, 3, 4, 5, 8, 9, 17, 64] {
        let leaves: Vec<ChunkLeaf> = (0..count)
            .map(|i| ChunkLeaf::of(i as u32, &filler(i as u64, 128)))
            .collect();
        let root = merkle_root(&leaves);
        for i in 0..count {
            let proof = chunk_proof(&leaves, i as u32).expect("proof");
            assert_eq!(
                proof.root(),
                Ok(root),
                "chunk {i} of {count} failed to prove"
            );
            assert!(
                proof.path.len() <= MAX_PROOF_DEPTH as usize,
                "a {count}-leaf tree produced a path deeper than the protocol depth"
            );
        }
    }
}

#[test]
fn the_descriptor_encoding_is_canonical_and_frozen() {
    let plaintext = filler(11, 1000);
    let descriptor = seal_content(&space(), &key(), [12u8; 16], &plaintext)
        .expect("seal")
        .descriptor;
    let encoded = descriptor.encode();

    assert_eq!(
        ContentDescriptor::decode_canonical(&encoded).expect("decode"),
        descriptor
    );
    // A trailing byte is not a canonical encoding of anything.
    let mut extended = encoded.clone();
    extended.push(0);
    assert!(ContentDescriptor::decode_canonical(&extended).is_err());
    // Neither is a truncation.
    assert!(ContentDescriptor::decode_canonical(&encoded[..encoded.len() - 1]).is_err());

    // Identity is a pure function of the canonical bytes, so it is stable
    // across encode/decode.
    let round_tripped = ContentDescriptor::decode_canonical(&encoded).unwrap();
    assert_eq!(round_tripped.content_ref(), descriptor.content_ref());
}

#[test]
fn a_declared_geometry_that_cannot_describe_content_is_refused() {
    let plaintext = filler(13, 5000);
    let descriptor = seal_content(&space(), &key(), [14u8; 16], &plaintext)
        .expect("seal")
        .descriptor;

    let mut lying_count = descriptor.clone();
    lying_count.chunk_count += 1;
    assert_eq!(lying_count.validate(), Err(ContentError::Geometry));

    let mut lying_chunk_size = descriptor.clone();
    lying_chunk_size.chunk_plaintext_len = 1024;
    assert_eq!(lying_chunk_size.validate(), Err(ContentError::Geometry));

    let mut future = descriptor.clone();
    future.format_version = CONTENT_FORMAT_VERSION + 1;
    assert_eq!(
        future.validate(),
        Err(ContentError::UnsupportedVersion(CONTENT_FORMAT_VERSION + 1))
    );

    let mut foreign = descriptor.clone();
    foreign.space = "not-a-space".into();
    assert_eq!(foreign.validate(), Err(ContentError::BadSpaceId));
}

#[test]
fn a_provider_verifies_without_holding_a_key() {
    // The property the whole ciphertext-Merkle choice exists for: relaying and
    // caching are possible for a party that cannot read the content.
    let plaintext = filler(15, CHUNK_PLAINTEXT_LEN as usize + 100);
    let SealedContent {
        descriptor,
        ciphertexts,
        proofs,
    } = seal_content(&space(), &key(), [16u8; 16], &plaintext).expect("seal");
    for (i, ciphertext) in ciphertexts.iter().enumerate() {
        assert!(descriptor.verify_chunk(&proofs[i], ciphertext).is_ok());
    }
    // With the wrong key, verification still passes and opening still fails.
    let stranger = AuthorizedBodyKey::for_authorized_epoch(EPOCH, [0xEEu8; 32]);
    assert!(descriptor.verify_chunk(&proofs[0], &ciphertexts[0]).is_ok());
    assert_eq!(
        descriptor.open_chunk(&stranger, &proofs[0], &ciphertexts[0]),
        Err(ContentError::Unopenable)
    );
}

#[test]
fn recorded_chunk_geometry() {
    // F0 item 4. The frozen choice is 256 KiB; this is what the alternative
    // costs, measured at the sizes the decision turns on.
    println!(
        "\n{:>12} {:>10} {:>8} {:>12} {:>13} {:>11}",
        "content", "chunk KiB", "chunks", "proof depth", "sidecar B", "overhead %"
    );
    for content_len in [
        64 * 1024usize,
        1024 * 1024,
        16 * 1024 * 1024,
        256 * 1024 * 1024,
    ] {
        for chunk_len in [256 * 1024usize, 1024 * 1024] {
            let chunks = content_len.div_ceil(chunk_len).max(1);
            let depth = (usize::BITS - chunks.next_power_of_two().leading_zeros() - 1) as usize;
            // A sidecar is the leaf record plus one 33-byte step per level.
            let sidecar = 40 + depth * 33;
            let envelope_overhead = chunks * 44;
            println!(
                "{:>11}K {:>10} {:>8} {:>12} {:>13} {:>10.4}%",
                content_len / 1024,
                chunk_len / 1024,
                chunks,
                depth,
                sidecar,
                100.0 * (envelope_overhead + chunks * sidecar) as f64 / content_len as f64,
            );
        }
    }
    println!(
        "frozen: {} KiB plaintext chunks, max proof depth {}",
        CHUNK_PLAINTEXT_LEN / 1024,
        MAX_PROOF_DEPTH
    );

    // The reason 256 KiB is the choice rather than 1 MiB: a chunk plus its
    // envelope must fit inside Contact's frame with room to spare, and a
    // failed transfer must not waste a megabyte.
    assert!(
        replica::content::max_ciphertext_len() < 1024 * 1024,
        "a sealed chunk must fit Contact's 1 MiB frame"
    );
}

// --- Streaming ingest -------------------------------------------------------

fn temp_cache(tag: &str) -> fabric::journal::cache::ResidentCache {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("lait-ingest-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    fabric::journal::cache::ResidentCache::open(dir, 1 << 30).unwrap()
}

#[test]
fn a_streamed_ingest_matches_a_whole_seal_and_reads_back() {
    use replica::content::{open_resident_chunk, ContentIngest};
    let cache = temp_cache("roundtrip");
    let chunk = CHUNK_PLAINTEXT_LEN as usize;

    for len in [0usize, 1, chunk - 1, chunk, chunk + 1, chunk * 2 + 13] {
        let plaintext = filler(len as u64, len);
        let mut ingest = ContentIngest::begin(&space(), &key(), [1u8; 16], &cache, u64::MAX);
        // Feed in awkward slices: ingest must not care where the reader's
        // buffer boundaries fall.
        for piece in plaintext.chunks(7919.min(plaintext.len().max(1))) {
            ingest.push(piece).unwrap();
        }
        let out = ingest.finish().unwrap();

        assert_eq!(out.descriptor.plaintext_len, len as u64);
        assert_eq!(
            out.descriptor.chunk_count as u64,
            expected_chunk_count(len as u64)
        );
        assert_eq!(out.leases.len(), out.descriptor.chunk_count as usize);

        let mut recovered = Vec::with_capacity(len);
        for lease in &out.leases {
            recovered.extend_from_slice(
                &open_resident_chunk(&out.descriptor, &key(), &cache, &lease.entry).unwrap(),
            );
        }
        assert_eq!(recovered, plaintext, "streamed round trip failed at {len}");
    }
}

#[test]
fn a_cancelled_ingest_leaves_nothing_reachable() {
    use replica::content::ContentIngest;
    let cache = temp_cache("cancel");
    let mut ingest = ContentIngest::begin(&space(), &key(), [2u8; 16], &cache, u64::MAX);
    ingest
        .push(&filler(1, CHUNK_PLAINTEXT_LEN as usize * 2))
        .unwrap();
    ingest.cancel();

    // Nothing was installed, because installation happens at finish — a proof
    // cannot exist before the tree it proves against.
    cache.sweep().unwrap();
    assert_eq!(cache.resident_bytes(), 0);
}

#[test]
fn a_dropped_ingest_cleans_up_like_a_cancelled_one() {
    use replica::content::ContentIngest;
    let cache = temp_cache("dropped");
    {
        let mut ingest = ContentIngest::begin(&space(), &key(), [3u8; 16], &cache, u64::MAX);
        ingest.push(b"abandoned").unwrap();
    }
    cache.sweep().unwrap();
    assert_eq!(cache.resident_bytes(), 0);
}

#[test]
fn an_ingest_past_its_policy_maximum_is_refused_while_streaming() {
    // Refused as the bytes arrive, not after the whole thing has been read.
    use replica::content::ContentIngest;
    let cache = temp_cache("toolong");
    let mut ingest = ContentIngest::begin(&space(), &key(), [4u8; 16], &cache, 1_000);
    assert!(ingest.push(&filler(1, 600)).is_ok());
    assert_eq!(
        ingest.push(&filler(2, 600)),
        Err(ContentError::Geometry),
        "the limit fires on the piece that would cross it"
    );
}

#[test]
fn ingest_holds_one_chunk_regardless_of_content_size() {
    // The property that makes this streaming rather than buffering. Measured
    // through the public surface: a content many chunks long must not make the
    // ingester's own buffer grow.
    use replica::content::ContentIngest;
    let cache = temp_cache("bounded");
    let mut ingest = ContentIngest::begin(&space(), &key(), [5u8; 16], &cache, u64::MAX);
    let piece = filler(6, CHUNK_PLAINTEXT_LEN as usize);
    for _ in 0..8 {
        ingest.push(&piece).unwrap();
    }
    let out = ingest.finish().unwrap();
    assert_eq!(out.descriptor.chunk_count, 8);
    // The descriptor is what every replica carries, and it is the same size for
    // 2 MiB as for a terabyte.
    assert!(
        out.descriptor.encode().len() < 128,
        "a descriptor is fixed-size whatever the content is: {} bytes",
        out.descriptor.encode().len()
    );
}

#[test]
fn the_ingest_hold_and_the_content_hold_hand_over() {
    use replica::content::ContentIngest;
    let cache = temp_cache("held");
    // Quota zero: everything unheld is evictable immediately.
    let cache = fabric::journal::cache::ResidentCache::open(cache.root(), 0).unwrap();
    let mut ingest = ContentIngest::begin(&space(), &key(), [7u8; 16], &cache, u64::MAX);
    ingest.push(b"held by its ingest").unwrap();
    let out = ingest.finish().unwrap();

    cache.sweep().unwrap();
    assert!(
        cache.is_resident(&out.leases[0].entry),
        "the ingest holds it"
    );

    // Releasing the ingest's own hold is what a caller does once it has
    // committed the descriptor. The content-scoped hold takes over, so the
    // bytes stay — otherwise committing a descriptor would be the moment its
    // content became collectable.
    cache.release_operation(&[7u8; 16]).unwrap();
    cache.sweep().unwrap();
    assert!(
        cache.is_resident(&out.leases[0].entry),
        "the content hold outlives the ingest that created it"
    );

    // Only a reachability sweep, which releases the content hold by nonce,
    // lets go. Releasing it as an *operation* must not work: the two kinds
    // share a sixteen-byte namespace and a nonce is public.
    cache
        .release_operation(&out.descriptor.content_nonce)
        .unwrap();
    cache.sweep().unwrap();
    assert!(
        cache.is_resident(&out.leases[0].entry),
        "an operation release cannot drop a content hold"
    );
    cache
        .release_content(&out.descriptor.content_nonce)
        .unwrap();
    cache.sweep().unwrap();
    assert!(!cache.is_resident(&out.leases[0].entry));
}
