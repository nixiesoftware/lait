//! Where a transfer is up to, as local state and nothing else.
//!
//! **Not an Observation, not a Body, and not any product-level change hint.**
//! Progress is a fact about this machine's disk and this machine's network. It is not agreed with
//! anyone, it does not survive a restart, and a peer's opinion about it is not
//! evidence. Putting it on the Observation ring would give it a sequence number
//! in a stream whose whole contract is that entries correspond to durable
//! commits — and it would let a chatty transfer push real commits out of a
//! consumer's window.
//!
//! So it has its own channel, its own bound, and its own coalescing. A watcher
//! that stalls falls behind on *progress* and nothing else.

use crate::poison::LockRecovering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
// `tokio::time::Instant`, not `tokio::time::Instant`. Without the `test-util`
// feature it IS `tokio::time::Instant::now()` — same call, same value, no
// indirection — so production pays nothing. With it, `tokio::time::pause()`
// stops the clock for every site at once, which is what lets a test drive a
// sweep interval or a probation window without waiting for one.
use std::time::Duration;
use tokio::time::Instant;

use replica::content::ContentRef;

/// The ladder a transfer climbs, and the three ways off it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferState {
    /// Admitted and waiting for a worker slot.
    Queued,
    /// Choosing and reaching providers.
    Connecting,
    /// Bytes are moving. `total` is absent until a descriptor is resolved.
    Transferring { bytes: u64, total: Option<u64> },
    /// Every chunk is here; hashes and proofs are being checked.
    Verifying,
    /// Complete and readable locally.
    Available,
    /// The caller asked for it to stop.
    Cancelled,
    /// It will not complete. The reason is local diagnostics, not a peer's.
    Failed,
    /// It completed once and the bytes have since been reclaimed.
    Evicted,
}

impl TransferState {
    /// Whether the transfer is over, whichever way it went.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TransferState::Available
                | TransferState::Cancelled
                | TransferState::Failed
                | TransferState::Evicted
        )
    }
}

/// One transfer, as a watcher sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferProgress {
    pub operation: [u8; 16],
    pub content: ContentRef,
    pub state: TransferState,
}

/// Why a transfer could not be registered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// An operation by that id is already in flight. Two handles over one id
    /// would release each other's leases.
    DuplicateOperation,
    /// This Station is already moving as much as it will move at once.
    TooManyActive,
}

/// How many transfers may be in flight at once.
///
/// The active set is what a cache sweep reads as "still live", so an unbounded
/// one is not just memory — it is disk that is never reclaimed.
pub const MAX_ACTIVE: usize = 32;

/// How many transfers are remembered after they finish.
///
/// Small on purpose. Completed entries exist so a caller that asked a moment
/// ago can find out how it went; they are not a history, and a registry that
/// grew one entry per completed transfer would be a memory leak with a
/// respectable name.
pub const MAX_COMPLETED: usize = 64;

/// How often a moving transfer publishes.
///
/// Progress is monotone, so coalescing loses nothing: a watcher that reads
/// twice a second learns the same thing as one that reads every chunk, minus
/// the wakeups. Two frames per second per transfer can never be what overwhelms
/// a consumer.
pub const PROGRESS_TICK: Duration = Duration::from_millis(500);

#[derive(Debug)]
struct Entry {
    progress: TransferProgress,
    published_at: Instant,
    /// The last state actually sent, so a coalesced update that changes the
    /// *state* is never held back by the tick.
    published_state: TransferState,
}

/// Every transfer this Station knows about.
///
/// Bounded in both directions: active transfers are capped by the fetch permit
/// that admits them, and completed ones by [`MAX_COMPLETED`].
#[derive(Debug)]
pub struct TransferRegistry {
    inner: Mutex<Catalog>,
    updates: tokio::sync::watch::Sender<u64>,
}

#[derive(Debug, Default)]
struct Catalog {
    active: BTreeMap<[u8; 16], Entry>,
    completed: VecDeque<TransferProgress>,
    version: u64,
}

impl Default for TransferRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TransferRegistry {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Catalog::default()),
            updates: tokio::sync::watch::Sender::new(0),
        }
    }

    /// A handle that bumps whenever anything changes. A watcher re-reads the
    /// snapshot rather than being handed a delta, because the snapshot is small
    /// and a delta stream would need its own overrun semantics.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.updates.subscribe()
    }

    /// Register a transfer.
    ///
    /// Refuses a duplicate rather than replacing one. Replacing looks harmless
    /// and is not: the displaced handle's `Drop` still runs, and it releases
    /// the *new* transfer's leases and deletes its staged bytes, because both
    /// are keyed by the same operation id. Silent data loss with no failure
    /// anywhere to point at.
    ///
    /// Also refuses past a ceiling, because the active set is what a cache
    /// sweep reads as "still live" — an unbounded one would keep every dead
    /// operation's disk forever.
    pub fn begin(
        &self,
        operation: [u8; 16],
        content: ContentRef,
        now: Instant,
    ) -> Result<(), Refusal> {
        let mut guard = self.lock();
        if guard.active.contains_key(&operation) {
            return Err(Refusal::DuplicateOperation);
        }
        if guard.active.len() >= MAX_ACTIVE {
            return Err(Refusal::TooManyActive);
        }
        guard.active.insert(
            operation,
            Entry {
                progress: TransferProgress {
                    operation,
                    content,
                    state: TransferState::Queued,
                },
                published_at: now,
                published_state: TransferState::Queued,
            },
        );
        self.bump(guard);
        Ok(())
    }

    /// Advance a transfer. Coalesced while only the byte count moves; a state
    /// change publishes immediately, because a state change is the thing a
    /// caller is usually waiting for.
    pub fn advance(&self, operation: &[u8; 16], state: TransferState, now: Instant) {
        let mut guard = self.lock();
        let Some(entry) = guard.active.get_mut(operation) else {
            return;
        };
        entry.progress.state = state;
        let changed_state =
            std::mem::discriminant(&state) != std::mem::discriminant(&entry.published_state);
        let due = now.duration_since(entry.published_at) >= PROGRESS_TICK;
        if !changed_state && !due {
            return;
        }
        entry.published_at = now;
        entry.published_state = state;

        if state.is_terminal() {
            let Some(finished) = guard.active.remove(operation).map(|entry| entry.progress) else {
                return;
            };
            if guard.completed.len() >= MAX_COMPLETED {
                guard.completed.pop_front();
            }
            guard.completed.push_back(finished);
        }
        self.bump(guard);
    }

    /// Everything currently in flight.
    pub fn active(&self) -> Vec<TransferProgress> {
        self.lock()
            .active
            .values()
            .map(|e| e.progress.clone())
            .collect()
    }

    /// The bounded tail of finished transfers, oldest first.
    pub fn completed(&self) -> Vec<TransferProgress> {
        self.lock().completed.iter().cloned().collect()
    }

    /// What one operation is doing, active or recently finished.
    pub fn state_of(&self, operation: &[u8; 16]) -> Option<TransferState> {
        let guard = self.lock();
        guard
            .active
            .get(operation)
            .map(|e| e.progress.state)
            .or_else(|| {
                guard
                    .completed
                    .iter()
                    .rev()
                    .find(|p| &p.operation == operation)
                    .map(|p| p.state)
            })
    }

    /// The operations that still hold staging and leases.
    ///
    /// This is the live set a cache sweep needs. Anything not in it is the
    /// wreckage of a run that is over, and the sweep is what says so.
    pub fn live_operations(&self) -> BTreeSet<[u8; 16]> {
        self.lock().active.keys().copied().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Catalog> {
        self.inner.lock_recovering()
    }

    /// Publish, having let go of the lock.
    ///
    /// Sending on a watch channel wakes subscribers, and a subscriber holding
    /// its `Ref` across an await would then be blocking a lock this registry
    /// takes on every update. Computing under the lock and sending after it is
    /// what keeps this a leaf.
    fn bump(&self, guard: std::sync::MutexGuard<'_, Catalog>) {
        let version = guard.version.saturating_add(1);
        let mut guard = guard;
        guard.version = version;
        drop(guard);
        let _ = self.updates.send(version);
    }
}

/// A transfer's lifetime, tied to a value.
///
/// Dropping one fails the transfer and releases everything it held. That is the
/// point: a fetch task that panics, is cancelled, or returns early through a
/// `?` would otherwise leave an operation lease pinning its chunks and a
/// staging slot holding its bytes, and nothing would ever say the operation was
/// over. Success has to be declared explicitly, which is the right way round —
/// forgetting to declare failure is common, forgetting to declare success is
/// immediately visible.
pub struct TransferHandle {
    registry: Arc<TransferRegistry>,
    cache: Arc<replica::content::Residency>,
    operation: [u8; 16],
    armed: bool,
}

impl TransferHandle {
    /// Register and take ownership, or refuse.
    ///
    /// A refusal is the whole point: if a handle could exist for an operation
    /// that is already in flight, the two would release each other's leases and
    /// delete each other's staged bytes with nothing anywhere reporting a
    /// failure.
    pub fn new(
        registry: Arc<TransferRegistry>,
        cache: Arc<replica::content::Residency>,
        operation: [u8; 16],
        content: ContentRef,
        now: Instant,
    ) -> Result<Self, Refusal> {
        registry.begin(operation, content, now)?;
        Ok(Self {
            registry,
            cache,
            operation,
            armed: true,
        })
    }

    pub fn operation(&self) -> [u8; 16] {
        self.operation
    }

    pub fn advance(&self, state: TransferState, now: Instant) {
        self.registry.advance(&self.operation, state, now);
    }

    /// Disarm the drop guard: the transfer completed and its holds have handed
    /// over to the content's own lease.
    pub fn succeed(mut self, now: Instant) {
        self.registry
            .advance(&self.operation, TransferState::Available, now);
        // The transfer-scoped hold goes. Under `Acquisition::Keep` the
        // content-scoped one stays and keeps the bytes; under `Stream` there is
        // none, and whoever is reading holds its own.
        let _ = self.cache.release_operation(&self.operation);
        let _ = self.cache.discard_staged(&self.operation);
        self.armed = false;
    }

    /// End it deliberately.
    pub fn finish(mut self, state: TransferState, now: Instant) {
        self.registry.advance(&self.operation, state, now);
        let _ = self.cache.release_operation(&self.operation);
        let _ = self.cache.discard_staged(&self.operation);
        self.armed = false;
    }
}

impl Drop for TransferHandle {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.registry
            .advance(&self.operation, TransferState::Failed, Instant::now());
        let _ = self.cache.release_operation(&self.operation);
        let _ = self.cache.discard_staged(&self.operation);
    }
}
