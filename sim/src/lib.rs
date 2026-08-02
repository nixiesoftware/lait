//! Whole-stack determinism: every source of entropy behind one seed.
//!
//! ## What this is for
//!
//! `runtime`'s convergence simulation replays its *schedule* — which peer
//! commits what, at which step, and which envelope slot is dropped. That is
//! enough to hand a colleague a seed and have them see the same assertion fail.
//! What it does **not** replay is the material underneath: two runs produce
//! different transaction commitments, because the stack draws from the OS in
//! several places at once — sealing nonces in `replica::content`, minted ids in
//! `replica::ids`, key material in `mechanics::crypto`, and FROST's own
//! `rand_core::OsRng`.
//!
//! Seaming those one at a time does not work; it was tried, on `fabric::op`,
//! and removed again. There is no single door *inside* the code.
//!
//! There is one underneath it. Every one of those paths — including FROST's,
//! since `rand_core::OsRng` is implemented on top of getrandom — ends at the
//! `getrandom` crate. Point that at a seeded generator and the whole stack
//! becomes a function of the seed: same commitments, same nonces, same ids,
//! byte for byte.
//!
//! ## Why it is safe
//!
//! `getrandom_backend="custom"` is a **rustc cfg**, set in this package's own
//! `.cargo/config.toml`. Cargo reads configuration from the current directory
//! upward and never downward, so a build at the repository root cannot see it.
//! A release binary does not contain this code and cannot be made to: there is
//! no runtime switch to flip, no feature to enable by accident, no `[patch]`
//! that survives into a published artifact.
//!
//! That distinction is the whole reason this lives here rather than in
//! `fabric`. An earlier attempt put a seed behind an atomic inside identity
//! minting — a switch in the middle of security-critical code, in every build,
//! that happened to be off. It also did not work, because it covered one source
//! of four. Both problems go away by moving the seam below the code entirely.
//!
//! ## Why a seeded nonce is not a vulnerability here
//!
//! AEAD requires nonce *uniqueness*, and the generator below never repeats
//! within a run. What it gives up is unpredictability — which matters for
//! material an adversary sees, and does not exist here: these stores are
//! temporary directories that outlive nothing. The property that would be
//! dangerous is a *production* build with predictable nonces, and that is
//! exactly what the compile-time scoping makes unrepresentable.

use std::sync::atomic::{AtomicU64, Ordering};

/// The simulation's single source of entropy.
///
/// A counter rather than a generator with hidden state: `fetch_add` is enough
/// to guarantee no two calls agree, and the hashing below is what turns a
/// counter into bytes. Anything with internal state would have to be reset in
/// exactly the right place, and forgetting is how a "deterministic" harness
/// quietly stops being one.
static DRAWN: AtomicU64 = AtomicU64::new(0);
static SEED: AtomicU64 = AtomicU64::new(0);

/// getrandom's opt-in hook. Every random byte the stack asks for arrives here.
///
/// # Safety
///
/// `dest` points to `len` writable bytes, possibly uninitialised — getrandom's
/// contract. Every byte is written before returning, so nothing uninitialised
/// is ever read.
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    let seed = SEED.load(Ordering::Relaxed);
    let mut written = 0usize;
    while written < len {
        // One hash per 32-byte block, keyed by (seed, draw). The draw counter
        // is global and monotonic, so two calls can never produce the same
        // block even if they ask for the same length at the same moment.
        let draw = DRAWN.fetch_add(1, Ordering::Relaxed);
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"lait.sim.entropy.v1\0");
        hasher.update(&seed.to_le_bytes());
        hasher.update(&draw.to_le_bytes());
        let block = hasher.finalize();
        let bytes = block.as_bytes();
        let take = (len - written).min(bytes.len());
        // SAFETY: `written + take <= len`, and the caller guarantees `len`
        // writable bytes at `dest`.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dest.add(written), take);
        }
        written += take;
    }
    Ok(())
}

/// Point the whole stack at `seed`, from the beginning.
///
/// Resets the draw counter as well as the seed, so a run is a function of the
/// seed alone rather than of whatever ran before it in the same process.
pub fn seed(seed: u64) {
    DRAWN.store(0, Ordering::Relaxed);
    SEED.store(seed, Ordering::Relaxed);
}

/// How many random bytes the stack has asked for since the last [`seed`].
///
/// Worth asserting on: a run that draws a different NUMBER of times from the
/// same seed has diverged somewhere the outputs do not show yet, and this
/// catches it before the divergence reaches something visible.
pub fn draws() -> u64 {
    DRAWN.load(Ordering::Relaxed)
}
