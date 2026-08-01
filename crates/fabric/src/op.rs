//! Document-level commit configuration shared by every Engine constructor.
//!
//! The crate pins Loro 1.13.6, whose configuration makes these details
//! load-bearing:
//! - `record_timestamp` defaults off, producing timestamp zero.
//! - with timestamp zero, the default merge interval check is always true:
//!   consecutive same-peer changes fuse into one. `set_change_merge_interval(0)`
//!   does not fix this because same-second stamps still compare equal; only
//!   `-1` disables fusion — the interval is the granularity guarantee.
//! - a fresh doc draws a **random peer id per session**, growing every doc's
//!   version vector by one dead entry per restart, forever.
//!
//! That last point used to carry the opposite mitigation: "callers that hold a
//! durable peer id pass it in so restart reuses it." It is superseded, and the
//! reason is measured rather than argued.
//!
//! Persisting a peer id is safe only when the exact local operation state is
//! inseparable from it — Loro's own guidance — and a copied store makes that
//! guarantee unreliable in a way nothing here can detect. So lait mints **one
//! fresh peer id per writable Station activation** and does not persist it.
//!
//! What that costs is a version vector that grows one entry per activation,
//! permanently: `crates/fabric/tests/causal_evidence.rs` measures 128 entries
//! after 128 activations, and a shallow snapshot does not shrink it, because
//! the document must keep knowing the trimmed operations are included.
//!
//! What makes the cost affordable is that the vector never leaves the process.
//! `Version` — the only causal value that is committed, framed, or
//! advertised — is a *head set*, which is sized by concurrency and stayed at
//! one entry across all 128 activations. Local memory grows slowly; nothing
//! replicated grows at all.

use loro::LoroDoc;
use std::sync::atomic::{AtomicU64, Ordering};

static FALLBACK_ENTROPY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Fill an internal identity buffer. OS entropy is preferred; the fallback
/// combines process-local monotonic state with time and hashes it so an entropy
/// outage cannot crash an activation or reuse the same value within a process.
pub(crate) fn fill_identity(raw: &mut [u8]) {
    if let Err(source) = getrandom::fill(raw) {
        tracing::error!(error = %source, "OS entropy unavailable while minting Fabric identity");
        let sequence = FALLBACK_ENTROPY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let elapsed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        fill_from_fallback(raw, std::process::id(), sequence, elapsed);
    }
}

/// The fallback itself, over its inputs rather than over the world.
///
/// Split out because otherwise it is unreachable: `getrandom::fill` fails only
/// on a real entropy outage, so no test has ever executed these lines. That is
/// a bad place for dead-by-default code — this is identity minting, and this
/// module explains why it matters that two activations never mint the same
/// value ("silent divergence rather than a detected equivocation"). The path
/// taken when entropy is ALREADY misbehaving is the last one that should be
/// untested.
///
/// Not a seam for injection: production still reads the real pid and clock two
/// lines up. This is the body, given its inputs, so its properties can be
/// asserted.
fn fill_from_fallback(raw: &mut [u8], pid: u32, sequence: u64, elapsed: std::time::Duration) {
    let mut hash = blake3::Hasher::new();
    hash.update(b"lait.fabric.identity-fallback.v1\0");
    hash.update(&pid.to_le_bytes());
    hash.update(&sequence.to_le_bytes());
    hash.update(&elapsed.as_secs().to_le_bytes());
    hash.update(&elapsed.subsec_nanos().to_le_bytes());
    hash.finalize_xof().fill(raw);
}

/// Engine configuration applied before any op is written or imported.
///
/// `peer` is the current writable activation's id, minted once per activation
/// and never persisted. `None` leaves Loro's random choice in place, which is
/// correct for a read-only or scratch document that will never author.
pub(crate) fn configure(doc: &LoroDoc, peer: Option<u64>) {
    doc.set_record_timestamp(true);
    doc.set_change_merge_interval(-1);
    if let Some(p) = peer {
        // Only fails with uncommitted pending ops; constructors call this first.
        let _ = doc.set_peer_id(p);
    }
}

/// Mint the peer id for one writable Station activation.
///
/// Fresh random per activation, deliberately. A derived-from-device id would
/// stop the version vector growing, and would also mean two processes that
/// somehow share a device key mint colliding operation ids — silent divergence
/// rather than a detected equivocation. Random is the choice that fails safely.
pub fn mint_activation_peer() -> u64 {
    let mut raw = [0u8; 8];
    fill_identity(&mut raw);
    // Loro reserves u64::MAX as a sentinel.
    u64::from_le_bytes(raw) & (u64::MAX >> 1)
}

#[cfg(test)]
mod fallback_tests {
    use super::fill_from_fallback;
    use std::collections::BTreeSet;
    use std::time::Duration;

    fn eight(pid: u32, sequence: u64, elapsed: Duration) -> [u8; 8] {
        let mut raw = [0u8; 8];
        fill_from_fallback(&mut raw, pid, sequence, elapsed);
        raw
    }

    /// The sequence counter alone must separate two activations, because the
    /// clock is exactly what cannot be relied on here: two activations inside
    /// one millisecond read the same `SystemTime`, and a machine whose entropy
    /// has failed is not a machine whose clock is above suspicion either.
    #[test]
    fn the_sequence_alone_separates_activations() {
        let clock = Duration::from_secs(1_700_000_000);
        let mut seen = BTreeSet::new();
        for sequence in 0..1_000u64 {
            assert!(
                seen.insert(eight(4242, sequence, clock)),
                "sequence {sequence} repeated a value at a frozen clock"
            );
        }
    }

    /// Two processes that lose entropy at the same instant must not agree.
    /// Colliding peer ids across processes is the failure this whole path
    /// exists to avoid.
    #[test]
    fn two_processes_do_not_collide() {
        let clock = Duration::from_secs(1_700_000_000);
        assert_ne!(eight(1, 0, clock), eight(2, 0, clock));
    }

    /// Never all-zero. A buffer the hash failed to fill would look like an
    /// ordinary peer id and collide with every other such failure.
    #[test]
    fn the_fallback_never_yields_zero() {
        for sequence in 0..256u64 {
            assert_ne!(
                eight(0, sequence, Duration::ZERO),
                [0u8; 8],
                "sequence {sequence} produced all zeros"
            );
        }
    }

    /// It fills whatever length it is given — callers ask for 8 and 16.
    #[test]
    fn it_fills_every_length_its_callers_use() {
        for len in [8usize, 16, 32] {
            let mut a = vec![0u8; len];
            let mut b = vec![0u8; len];
            fill_from_fallback(&mut a, 7, 1, Duration::ZERO);
            fill_from_fallback(&mut b, 7, 2, Duration::ZERO);
            assert_ne!(a, b, "length {len} did not vary with the sequence");
            assert!(
                a.iter().any(|byte| *byte != 0),
                "length {len} was all zeros"
            );
        }
    }
}
