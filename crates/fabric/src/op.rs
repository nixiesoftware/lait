//! Document-level commit configuration shared by every fabric constructor.
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
//! `FabricVersion` — the only causal value that is committed, framed, or
//! advertised — is a *head set*, which is sized by concurrency and stayed at
//! one entry across all 128 activations. Local memory grows slowly; nothing
//! replicated grows at all.

use loro::LoroDoc;

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
    getrandom::fill(&mut raw).expect("getrandom");
    // Loro reserves u64::MAX as a sentinel.
    u64::from_le_bytes(raw) & (u64::MAX >> 1)
}
