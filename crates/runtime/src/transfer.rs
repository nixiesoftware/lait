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

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    inner: Mutex<Registry>,
    updates: tokio::sync::watch::Sender<u64>,
}

#[derive(Debug, Default)]
struct Registry {
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
            inner: Mutex::new(Registry::default()),
            updates: tokio::sync::watch::Sender::new(0),
        }
    }

    /// A handle that bumps whenever anything changes. A watcher re-reads the
    /// snapshot rather than being handed a delta, because the snapshot is small
    /// and a delta stream would need its own overrun semantics.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.updates.subscribe()
    }

    /// Register a transfer, replacing any terminal entry for the same
    /// operation.
    pub fn begin(&self, operation: [u8; 16], content: ContentRef, now: Instant) {
        let mut guard = self.lock();
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
        self.bump(&mut guard);
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
            let finished = guard
                .active
                .remove(operation)
                .expect("present a line above")
                .progress;
            if guard.completed.len() >= MAX_COMPLETED {
                guard.completed.pop_front();
            }
            guard.completed.push_back(finished);
        }
        self.bump(&mut guard);
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

    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn bump(&self, guard: &mut Registry) {
        guard.version = guard.version.saturating_add(1);
        let _ = self.updates.send(guard.version);
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
    cache: Arc<replica::journal::cache::ResidentCache>,
    operation: [u8; 16],
    armed: bool,
}

impl TransferHandle {
    pub fn new(
        registry: Arc<TransferRegistry>,
        cache: Arc<replica::journal::cache::ResidentCache>,
        operation: [u8; 16],
        content: ContentRef,
        now: Instant,
    ) -> Self {
        registry.begin(operation, content, now);
        Self {
            registry,
            cache,
            operation,
            armed: true,
        }
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
        // The ingest-scoped hold goes; the content-scoped one stays, which is
        // what keeps the bytes after the transfer that fetched them is gone.
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
