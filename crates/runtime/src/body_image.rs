//! Governor-accounted lazy Body images for immutable World publications.
//!
//! Replica owns durable material and verifies/decrypts one exact Body image.
//! Runtime owns request-path reuse: one material-identity single-flight and a
//! bounded hot set shared by every human/agent access path at the Station.
//! Full Corpus extraction deliberately bypasses this cache so a sequential
//! scan cannot displace interactive Bodies; its outer publication-build
//! reservation accounts the one streamed image instead.

use crate::poison::LockRecovering as _;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// Cache-local form of Replica's authenticated Body material identity.
///
/// The supplying digest must bind BodyKey, binding/stamp, final causal
/// Version, the complete protected ArtifactRef closure including key epochs,
/// and authenticated size metadata. BodyKey or publication identity alone is
/// not sufficient: unchanged material may share across publications, while a
/// changed head must never alias its predecessor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct BodyImageKey([u8; 32]);

impl BodyImageKey {
    pub(crate) const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }
}

impl From<replica::BodyImageId> for BodyImageKey {
    fn from(identity: replica::BodyImageId) -> Self {
        Self::from_digest(identity.as_bytes())
    }
}

/// Authenticated upper bounds known before opening any protected artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BodyImageAdmission {
    pub(crate) key: BodyImageKey,
    /// Sum of the signed protected-envelope lengths which the resolver may
    /// read for this image.
    pub(crate) protected_bytes: u64,
    /// Maximum canonical Body export implied by those bounded envelopes.
    /// A plaintext-size field is validation metadata, not permission to
    /// reserve less than this safe decode bound.
    pub(crate) decoded_upper_bound: u64,
}

/// Hard corruption ceilings. Product/World limits should normally be tighter;
/// these only prevent a hostile signed length from becoming an allocation.
pub(crate) const MAX_PROTECTED_BODY_IMAGE_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const MAX_DECODED_BODY_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
/// Metadata bound independent of byte admission. Large Bodies will normally
/// hit the physical envelope first; tiny immutable records cannot create an
/// unbounded map merely because their payloads are cheap.
pub(crate) const MAX_HOT_BODY_IMAGES: usize = 4_096;
/// Byte ceiling on the READY set's summed governor leases. The entry bound
/// alone cannot hold the pool: a scan-shaped workload over many megabyte-sized
/// Bodies sums past the Station budget long before 4,096 entries — a real
/// store's migration walked ~1.6 GiB of hot-set leases into the governor and
/// starved its own commits.
const MAX_HOT_BODY_IMAGE_BYTES: u64 = 256 * 1024 * 1024;
const RETAINED_ENTRY_OVERHEAD: u64 = 256;

impl BodyImageAdmission {
    fn transient_bytes(self) -> Result<u64, BodyImageFailure> {
        if self.protected_bytes > MAX_PROTECTED_BODY_IMAGE_BYTES
            || self.decoded_upper_bound > MAX_DECODED_BODY_IMAGE_BYTES
        {
            return Err(BodyImageFailure::Corrupt);
        }
        self.protected_bytes
            .checked_add(self.decoded_upper_bound)
            .and_then(|bytes| bytes.checked_add(RETAINED_ENTRY_OVERHEAD))
            .ok_or(BodyImageFailure::Corrupt)
    }
}

/// Typed failure from the exact durable Body-image path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyImageFailure {
    /// The shared process/Station envelope refused the read before I/O.
    Capacity,
    /// The exact material closure is missing or has been reclaimed contrary
    /// to its publication lease.
    Unavailable,
    /// The publication names material the pinned authority deliberately does
    /// not expose. Keep this distinct from a missing artifact: syncing cannot
    /// turn the same materialization readable.
    Opaque,
    /// Its authenticated opening key is unavailable.
    KeyUnavailable,
    /// A length, digest, causal version, envelope, or decoded export failed
    /// verification.
    Corrupt,
    /// The exact Body is atomic rather than collaborative.
    NotCollaborative,
    /// The collaborative export binds a type this Runtime does not implement.
    SchemaAhead,
    /// The blocking resolution lane was cancelled or panicked.
    Interrupted,
}

impl From<replica::BodyImageFailure> for BodyImageFailure {
    fn from(failure: replica::BodyImageFailure) -> Self {
        match failure {
            replica::BodyImageFailure::MaterialUnavailable | replica::BodyImageFailure::Io => {
                Self::Unavailable
            }
            replica::BodyImageFailure::KeyUnavailable => Self::KeyUnavailable,
            replica::BodyImageFailure::Opaque => Self::Opaque,
            replica::BodyImageFailure::Capacity => Self::Capacity,
            replica::BodyImageFailure::Corrupt
            | replica::BodyImageFailure::ModelMismatch
            | replica::BodyImageFailure::ImmutableConflict => Self::Corrupt,
        }
    }
}

/// Transient memory acquired before the resolver opens its first artifact.
pub(crate) trait BodyImageMemoryReservation: Send {
    fn retain(
        self: Box<Self>,
        retained_bytes: u64,
    ) -> Result<Box<dyn BodyImageMemoryLease>, BodyImageFailure>;
}

/// Retained physical memory authority owned by one cached image. The lease is
/// intentionally held by the same Arc as the BodySnapshot, so removing an LRU
/// entry cannot release accounting while a concurrent reader still pins it.
pub(crate) trait BodyImageMemoryLease: Send + Sync {}

pub(crate) trait BodyImageMemory: Send + Sync {
    fn reserve(
        &self,
        transient_bytes: u64,
    ) -> Result<Box<dyn BodyImageMemoryReservation>, BodyImageFailure>;
}

struct ResidentBodyImage {
    image: Arc<fabric::BodySnapshot>,
    admission: BodyImageAdmission,
    memory: Arc<dyn BodyImageMemory>,
    collaborative: Mutex<CollaborativeState>,
    _image_memory: Box<dyn BodyImageMemoryLease>,
}

struct ResidentCollaborative {
    view: Arc<fabric::CollaborativeView>,
    _memory: Box<dyn BodyImageMemoryLease>,
}

#[derive(Clone)]
struct CollaborativeFlight {
    result: Arc<Mutex<Option<Result<Arc<ResidentCollaborative>, BodyImageFailure>>>>,
    wake: Arc<Condvar>,
}

impl CollaborativeFlight {
    fn new() -> Self {
        Self {
            result: Arc::new(Mutex::new(None)),
            wake: Arc::new(Condvar::new()),
        }
    }

    fn complete(&self, result: Result<Arc<ResidentCollaborative>, BodyImageFailure>) {
        *self.result.lock_recovering() = Some(result);
        self.wake.notify_all();
    }

    fn wait(&self) -> Result<Arc<ResidentCollaborative>, BodyImageFailure> {
        let mut result = self.result.lock_recovering();
        loop {
            if let Some(result) = result.clone() {
                return result;
            }
            result = self
                .wake
                .wait(result)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

enum CollaborativeState {
    Cold,
    Loading(CollaborativeFlight),
    Ready(Arc<ResidentCollaborative>),
    Failed(BodyImageFailure),
}

/// One exact Body image pinned independently of the cache's LRU ownership.
#[derive(Clone)]
pub(crate) struct PinnedBodyImage(Arc<ResidentBodyImage>);

impl std::fmt::Debug for PinnedBodyImage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PinnedBodyImage")
            .field("retained_bytes", &self.0.image.retained_bytes())
            .finish_non_exhaustive()
    }
}

impl PinnedBodyImage {
    /// The governor bytes this entry's leases hold while it stays cached.
    ///
    /// The projected collaborative view memoized on the image carries its own
    /// lease, charged at the conservative decoded upper bound — counting only
    /// the canonical image here let a cache "under" its byte ceiling hold
    /// gigabytes of projection leases in the governor.
    fn lease_bytes(&self) -> u64 {
        let projection = match &*self.0.collaborative.lock_recovering() {
            CollaborativeState::Ready(_) => self
                .0
                .admission
                .decoded_upper_bound
                .saturating_add(RETAINED_ENTRY_OVERHEAD),
            _ => 0,
        };
        self.0
            .image
            .retained_bytes()
            .saturating_add(RETAINED_ENTRY_OVERHEAD)
            .saturating_add(projection)
    }

    /// Borrow the exact canonical image while retaining this cache entry's
    /// image and memory leases. Callers must not let the returned reference
    /// escape the `PinnedBodyImage` guard.
    pub(crate) fn snapshot(&self) -> &fabric::BodySnapshot {
        &self.0.image
    }

    pub(crate) fn read_shared(&self) -> Option<Arc<[u8]>> {
        self.0.image.read_shared()
    }

    /// Project one immutable collaborative image exactly once. The canonical
    /// export and projected view hold distinct memory leases. Capacity and
    /// interruption are not cached: after followers receive the same typed
    /// result, the exact material remains retryable.
    pub(crate) fn read_collaborative(
        &self,
    ) -> Result<Arc<fabric::CollaborativeView>, BodyImageFailure> {
        enum Role {
            Lead(CollaborativeFlight),
            Follow(CollaborativeFlight),
        }

        let role = {
            let mut state = self.0.collaborative.lock_recovering();
            match &*state {
                CollaborativeState::Ready(view) => return Ok(view.view.clone()),
                CollaborativeState::Failed(failure) => return Err(failure.clone()),
                CollaborativeState::Loading(flight) => Role::Follow(flight.clone()),
                CollaborativeState::Cold => {
                    let flight = CollaborativeFlight::new();
                    *state = CollaborativeState::Loading(flight.clone());
                    Role::Lead(flight)
                }
            }
        };
        let flight = match role {
            Role::Lead(flight) => flight,
            Role::Follow(flight) => return flight.wait().map(|view| view.view.clone()),
        };

        let result = self.project_collaborative();
        {
            let mut state = self.0.collaborative.lock_recovering();
            let still_ours = matches!(
                &*state,
                CollaborativeState::Loading(current)
                    if Arc::ptr_eq(&current.result, &flight.result)
            );
            if still_ours {
                *state = match &result {
                    Ok(view) => CollaborativeState::Ready(view.clone()),
                    Err(
                        failure @ (BodyImageFailure::NotCollaborative
                        | BodyImageFailure::SchemaAhead
                        | BodyImageFailure::Corrupt),
                    ) => CollaborativeState::Failed(failure.clone()),
                    Err(_) => CollaborativeState::Cold,
                };
            }
        }
        flight.complete(result.clone());
        result.map(|view| view.view.clone())
    }

    fn project_collaborative(&self) -> Result<Arc<ResidentCollaborative>, BodyImageFailure> {
        let bytes = self
            .0
            .admission
            .decoded_upper_bound
            .checked_add(RETAINED_ENTRY_OVERHEAD)
            .ok_or(BodyImageFailure::Corrupt)?;
        let reservation = self.0.memory.reserve(bytes)?;
        let view = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.0.image.read_collaborative()
        }))
        .map_err(|_| BodyImageFailure::Interrupted)?
        .map_err(|failure| match failure {
            fabric::projection::Failure::NotCollaborative => BodyImageFailure::NotCollaborative,
            fabric::projection::Failure::SchemaAhead => BodyImageFailure::SchemaAhead,
            fabric::projection::Failure::Malformed => BodyImageFailure::Corrupt,
        })?;
        // The authenticated decoded bound includes the import/projection
        // working set. Retaining that full amount is conservative until
        // Fabric exposes an O(1) physical view-size estimate.
        let memory = reservation.retain(bytes)?;
        Ok(Arc::new(ResidentCollaborative {
            view: Arc::new(view),
            _memory: memory,
        }))
    }

    #[cfg(test)]
    fn shares_image_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Clone)]
struct LoadFlight {
    result: Arc<Mutex<Option<Result<PinnedBodyImage, BodyImageFailure>>>>,
    wake: Arc<Condvar>,
    waiters: Arc<AtomicUsize>,
}

impl LoadFlight {
    fn new() -> Self {
        Self {
            result: Arc::new(Mutex::new(None)),
            wake: Arc::new(Condvar::new()),
            waiters: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn complete(&self, result: Result<PinnedBodyImage, BodyImageFailure>) {
        *self.result.lock_recovering() = Some(result);
        self.wake.notify_all();
    }

    fn wait(&self) -> Result<PinnedBodyImage, BodyImageFailure> {
        self.waiters.fetch_add(1, Ordering::AcqRel);
        let mut result = self.result.lock_recovering();
        loop {
            if let Some(result) = result.clone() {
                return result;
            }
            result = self
                .wake
                .wait(result)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

enum CacheEntry {
    Loading {
        admission: BodyImageAdmission,
        flight: LoadFlight,
    },
    Ready {
        admission: BodyImageAdmission,
        image: PinnedBodyImage,
        used: u64,
    },
}

#[derive(Default)]
struct CacheState {
    entries: BTreeMap<BodyImageKey, CacheEntry>,
    clock: u64,
}

impl CacheState {
    fn tick(&mut self) -> u64 {
        self.clock = self.clock.saturating_add(1);
        self.clock
    }

    fn evict_one_ready(&mut self) -> bool {
        let oldest = self
            .entries
            .iter()
            .filter_map(|(key, entry)| match entry {
                CacheEntry::Ready { used, .. } => Some((*used, *key)),
                CacheEntry::Loading { .. } => None,
            })
            .min()
            .map(|(_, key)| key);
        oldest.is_some_and(|key| self.entries.remove(&key).is_some())
    }

    /// Summed governor leases of the READY set. Bounded by MAX_HOT_BODY_IMAGES
    /// entries, so recomputing on demand stays cheap.
    fn ready_lease_bytes(&self) -> u64 {
        self.entries
            .values()
            .fold(0u64, |total, entry| match entry {
                CacheEntry::Ready { image, .. } => total.saturating_add(image.lease_bytes()),
                CacheEntry::Loading { .. } => total,
            })
    }
}

/// Station-shared bounded interactive Body cache.
pub(crate) struct BodyImageCache {
    memory: Arc<dyn BodyImageMemory>,
    max_entries: usize,
    state: Mutex<CacheState>,
}

impl BodyImageCache {
    pub(crate) fn new(memory: Arc<dyn BodyImageMemory>, max_entries: usize) -> Self {
        Self {
            memory,
            max_entries,
            state: Mutex::new(CacheState::default()),
        }
    }

    /// Resolve one interactive Body. Exactly one caller reserves/reads/decodes
    /// a material identity; concurrent callers wait without holding cache,
    /// Station, or Replica locks and receive the same Arc or typed failure.
    pub(crate) fn resolve(
        &self,
        admission: BodyImageAdmission,
        load: impl FnOnce() -> Result<Arc<fabric::BodySnapshot>, BodyImageFailure>,
    ) -> Result<PinnedBodyImage, BodyImageFailure> {
        enum Role {
            Lead(LoadFlight),
            Follow(LoadFlight),
        }

        let role = {
            let mut state = self.state.lock_recovering();
            let next_used = state.tick();
            match state.entries.get_mut(&admission.key) {
                Some(CacheEntry::Ready {
                    admission: cached,
                    image,
                    used,
                }) => {
                    if *cached != admission {
                        return Err(BodyImageFailure::Corrupt);
                    }
                    *used = next_used;
                    return Ok(image.clone());
                }
                Some(CacheEntry::Loading {
                    admission: cached,
                    flight,
                }) => {
                    if *cached != admission {
                        return Err(BodyImageFailure::Corrupt);
                    }
                    Role::Follow(flight.clone())
                }
                None => {
                    if self.max_entries == 0 {
                        return Err(BodyImageFailure::Capacity);
                    }
                    while state.entries.len() >= self.max_entries {
                        if !state.evict_one_ready() {
                            return Err(BodyImageFailure::Capacity);
                        }
                    }
                    let flight = LoadFlight::new();
                    state.entries.insert(
                        admission.key,
                        CacheEntry::Loading {
                            admission,
                            flight: flight.clone(),
                        },
                    );
                    Role::Lead(flight)
                }
            }
        };

        let flight = match role {
            Role::Lead(flight) => flight,
            Role::Follow(flight) => return flight.wait(),
        };

        let result = self.load(admission, true, load);
        {
            let mut state = self.state.lock_recovering();
            let still_ours = matches!(
                state.entries.get(&admission.key),
                Some(CacheEntry::Loading { flight: current, .. })
                    if Arc::ptr_eq(&current.result, &flight.result)
            );
            if still_ours {
                match &result {
                    Ok(image) => {
                        let used = state.tick();
                        // Hold the READY set to its byte ceiling as well as
                        // its entry ceiling before this entry joins it.
                        let incoming = image.lease_bytes();
                        while state.ready_lease_bytes().saturating_add(incoming)
                            > MAX_HOT_BODY_IMAGE_BYTES
                            && state.evict_one_ready()
                        {}
                        state.entries.insert(
                            admission.key,
                            CacheEntry::Ready {
                                admission,
                                image: image.clone(),
                                used,
                            },
                        );
                    }
                    Err(_) => {
                        state.entries.remove(&admission.key);
                    }
                }
            }
        }
        flight.complete(result.clone());
        result
    }

    /// Resolve one Body without inserting a new LRU owner. Publication builds
    /// use this path so a full source scan cannot evict or fill the interactive
    /// hot set, while the returned guard still owns an exact governor lease if
    /// a World retains it beyond the extractor callback. An already-hot or
    /// in-flight interactive image is shared rather than opened twice.
    pub(crate) fn resolve_no_fill(
        &self,
        admission: BodyImageAdmission,
        load: impl FnOnce() -> Result<Arc<fabric::BodySnapshot>, BodyImageFailure>,
    ) -> Result<PinnedBodyImage, BodyImageFailure> {
        let shared = {
            let state = self.state.lock_recovering();
            match state.entries.get(&admission.key) {
                Some(CacheEntry::Ready {
                    admission: cached,
                    image,
                    ..
                }) => {
                    if *cached != admission {
                        return Err(BodyImageFailure::Corrupt);
                    }
                    return Ok(image.clone());
                }
                Some(CacheEntry::Loading {
                    admission: cached,
                    flight,
                }) => {
                    if *cached != admission {
                        return Err(BodyImageFailure::Corrupt);
                    }
                    Some(flight.clone())
                }
                None => None,
            }
        };
        if let Some(flight) = shared {
            return flight.wait();
        }
        // No-fill never inserts an owner, but byte pressure must still be
        // shed: with eviction forbidden here, the one reader routed around
        // the hot set — a full source scan — was also the one reader that
        // could never make room, and a migration died on a Capacity read the
        // cache could have absorbed by dropping a cold owner.
        self.load(admission, true, load)
    }

    fn load(
        &self,
        admission: BodyImageAdmission,
        evict_ready: bool,
        load: impl FnOnce() -> Result<Arc<fabric::BodySnapshot>, BodyImageFailure>,
    ) -> Result<PinnedBodyImage, BodyImageFailure> {
        let transient = admission.transient_bytes()?;
        let reservation = loop {
            match self.memory.reserve(transient) {
                Ok(reservation) => break reservation,
                Err(BodyImageFailure::Capacity) => {
                    // Byte pressure can arrive before the entry-count bound.
                    // Drop the least-recent cache owner and retry; an image
                    // still pinned by a concurrent BodyBytes guard keeps its
                    // lease, so this never pretends eviction freed memory.
                    if !evict_ready || !self.state.lock_recovering().evict_one_ready() {
                        return Err(BodyImageFailure::Capacity);
                    }
                }
                Err(failure) => return Err(failure),
            }
        };
        let image = std::panic::catch_unwind(std::panic::AssertUnwindSafe(load))
            .map_err(|_| BodyImageFailure::Interrupted)??;
        let retained = image.retained_bytes();
        if retained > admission.decoded_upper_bound {
            return Err(BodyImageFailure::Corrupt);
        }
        let retained = retained
            .checked_add(RETAINED_ENTRY_OVERHEAD)
            .ok_or(BodyImageFailure::Corrupt)?;
        let memory = reservation.retain(retained)?;
        Ok(PinnedBodyImage(Arc::new(ResidentBodyImage {
            image,
            admission,
            memory: self.memory.clone(),
            collaborative: Mutex::new(CollaborativeState::Cold),
            _image_memory: memory,
        })))
    }

    #[cfg(test)]
    fn ready_len(&self) -> usize {
        self.state
            .lock_recovering()
            .entries
            .values()
            .filter(|entry| matches!(entry, CacheEntry::Ready { .. }))
            .count()
    }

    #[cfg(test)]
    fn loading_waiters(&self, key: BodyImageKey) -> usize {
        self.state
            .lock_recovering()
            .entries
            .get(&key)
            .and_then(|entry| match entry {
                CacheEntry::Loading { flight, .. } => Some(flight.waiters.load(Ordering::Acquire)),
                CacheEntry::Ready { .. } => None,
            })
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    use std::sync::Barrier;

    #[derive(Default)]
    struct MemoryState {
        building: u64,
        retained: u64,
    }

    struct TestMemory {
        cap: u64,
        state: Arc<Mutex<MemoryState>>,
    }

    struct TestReservation {
        state: Arc<Mutex<MemoryState>>,
        bytes: u64,
        active: bool,
        cap: u64,
    }

    struct TestLease {
        state: Arc<Mutex<MemoryState>>,
        bytes: u64,
    }

    impl BodyImageMemoryLease for TestLease {}

    impl Drop for TestLease {
        fn drop(&mut self) {
            let mut state = self.state.lock_recovering();
            state.retained = state.retained.saturating_sub(self.bytes);
        }
    }

    impl Drop for TestReservation {
        fn drop(&mut self) {
            if self.active {
                let mut state = self.state.lock_recovering();
                state.building = state.building.saturating_sub(self.bytes);
            }
        }
    }

    impl BodyImageMemoryReservation for TestReservation {
        fn retain(
            mut self: Box<Self>,
            retained_bytes: u64,
        ) -> Result<Box<dyn BodyImageMemoryLease>, BodyImageFailure> {
            let mut state = self.state.lock_recovering();
            let next = state
                .retained
                .saturating_add(state.building.saturating_sub(self.bytes))
                .saturating_add(retained_bytes);
            if next > self.cap {
                return Err(BodyImageFailure::Capacity);
            }
            state.building = state.building.saturating_sub(self.bytes);
            state.retained = state.retained.saturating_add(retained_bytes);
            self.active = false;
            drop(state);
            Ok(Box::new(TestLease {
                state: self.state.clone(),
                bytes: retained_bytes,
            }))
        }
    }

    impl BodyImageMemory for TestMemory {
        fn reserve(
            &self,
            transient_bytes: u64,
        ) -> Result<Box<dyn BodyImageMemoryReservation>, BodyImageFailure> {
            let mut state = self.state.lock_recovering();
            if state
                .retained
                .saturating_add(state.building)
                .saturating_add(transient_bytes)
                > self.cap
            {
                return Err(BodyImageFailure::Capacity);
            }
            state.building = state.building.saturating_add(transient_bytes);
            drop(state);
            Ok(Box::new(TestReservation {
                state: self.state.clone(),
                bytes: transient_bytes,
                active: true,
                cap: self.cap,
            }))
        }
    }

    fn memory(cap: u64) -> (Arc<TestMemory>, Arc<Mutex<MemoryState>>) {
        let state = Arc::new(Mutex::new(MemoryState::default()));
        (
            Arc::new(TestMemory {
                cap,
                state: state.clone(),
            }),
            state,
        )
    }

    fn admission(byte: u8, protected: u64, decoded: u64) -> BodyImageAdmission {
        BodyImageAdmission {
            key: BodyImageKey::from_digest([byte; 32]),
            protected_bytes: protected,
            decoded_upper_bound: decoded,
        }
    }

    fn atomic(len: usize) -> Arc<fabric::BodySnapshot> {
        Arc::new(
            fabric::BodySnapshot::from_export(
                &fabric::Key::from_bytes(vec![7]),
                fabric::BodyExport::Atomic(vec![3; len]),
            )
            .expect("valid atomic snapshot"),
        )
    }

    fn collaborative() -> Arc<fabric::BodySnapshot> {
        let key = fabric::Key::from_bytes(vec![8]);
        let mut engine = fabric::Engine::new();
        engine
            .commit(fabric::Transaction::new(
                "collaborative-cache-fixture",
                vec![
                    fabric::Op::CreateBody { key: key.clone() },
                    fabric::Op::RegisterSet {
                        key: key.clone(),
                        path: "title".to_owned(),
                        value: b"shared".to_vec(),
                    },
                ],
            ))
            .expect("valid collaborative commit");
        Arc::new(
            engine
                .body_snapshot(&key)
                .expect("snapshot succeeds")
                .expect("collaborative Body exists"),
        )
    }

    #[test]
    fn startup_is_cold_and_repeated_read_reuses_one_inflation() {
        let (memory, state) = memory(16 * 1024);
        let cache = BodyImageCache::new(memory, 4);
        let loads = AtomicUsize::new(0);
        assert_eq!(cache.ready_len(), 0);
        assert_eq!(state.lock_recovering().retained, 0);

        let first = cache
            .resolve(admission(1, 64, 512), || {
                loads.fetch_add(1, Ordering::Relaxed);
                Ok(atomic(128))
            })
            .expect("cold read");
        let second = cache
            .resolve(admission(1, 64, 512), || {
                loads.fetch_add(1, Ordering::Relaxed);
                Ok(atomic(128))
            })
            .expect("warm read");
        assert_eq!(loads.load(Ordering::Relaxed), 1);
        assert!(first.shares_image_with(&second));
        assert_eq!(cache.ready_len(), 1);
    }

    #[test]
    fn concurrent_readers_share_one_singleflight_and_pin_one_image() {
        let (memory, _) = memory(16 * 1024);
        let cache = Arc::new(BodyImageCache::new(memory, 4));
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let loads = Arc::new(AtomicUsize::new(0));

        let leader = {
            let cache = cache.clone();
            let entered = entered.clone();
            let release = release.clone();
            let loads = loads.clone();
            std::thread::spawn(move || {
                cache.resolve(admission(2, 64, 512), || {
                    loads.fetch_add(1, Ordering::Relaxed);
                    entered.wait();
                    release.wait();
                    Ok(atomic(128))
                })
            })
        };
        entered.wait();
        let follower = {
            let cache = cache.clone();
            let loads = loads.clone();
            std::thread::spawn(move || {
                cache.resolve(admission(2, 64, 512), || {
                    loads.fetch_add(1, Ordering::Relaxed);
                    Ok(atomic(128))
                })
            })
        };
        while cache.loading_waiters(admission(2, 64, 512).key) == 0 {
            std::thread::yield_now();
        }
        release.wait();
        let leader = leader.join().expect("leader joins").expect("leader image");
        let follower = follower
            .join()
            .expect("follower joins")
            .expect("follower image");
        assert_eq!(loads.load(Ordering::Relaxed), 1);
        assert!(leader.shares_image_with(&follower));
    }

    #[test]
    fn collaborative_projection_is_shared_and_holds_its_own_memory_lease() {
        let (memory, state) = memory(64 * 1024);
        let cache = BodyImageCache::new(memory, 1);
        let image = cache
            .resolve(admission(11, 64, 4 * 1024), || Ok(collaborative()))
            .expect("collaborative export");
        let export_retained = state.lock_recovering().retained;

        let first = image.read_collaborative().expect("first projection");
        let projected_retained = state.lock_recovering().retained;
        assert!(projected_retained > export_retained);
        assert_eq!(
            first.registers.get("title").map(Vec::as_slice),
            Some(b"shared".as_slice())
        );
        let second = image.read_collaborative().expect("warm projection");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(state.lock_recovering().retained, projected_retained);

        let guard = crate::world::CollaborativeBody::cached(first, image);
        cache
            .resolve(admission(12, 64, 512), || Ok(atomic(128)))
            .expect("evict cache owner");
        assert_eq!(guard.registers["title"], b"shared");
        assert!(state.lock_recovering().retained >= projected_retained);
        drop(guard);
        assert!(state.lock_recovering().retained < projected_retained);
    }

    #[test]
    fn atomic_projection_is_typed_and_releases_projection_reservation() {
        let (memory, state) = memory(16 * 1024);
        let cache = BodyImageCache::new(memory, 4);
        let image = cache
            .resolve(admission(13, 64, 512), || Ok(atomic(128)))
            .expect("atomic export");
        let retained = state.lock_recovering().retained;
        assert!(matches!(
            image.read_collaborative(),
            Err(BodyImageFailure::NotCollaborative)
        ));
        assert_eq!(state.lock_recovering().building, 0);
        assert_eq!(state.lock_recovering().retained, retained);
    }

    #[test]
    fn eviction_does_not_release_a_concurrent_reader_pin() {
        let (memory, state) = memory(16 * 1024);
        let cache = BodyImageCache::new(memory, 1);
        let first_image = cache
            .resolve(admission(3, 64, 512), || Ok(atomic(128)))
            .expect("first image");
        let first = crate::world::BodyBytes::cached(
            first_image.read_shared().expect("atomic bytes"),
            first_image,
        );
        let first_retained = state.lock_recovering().retained;
        let second = cache
            .resolve(admission(4, 64, 512), || Ok(atomic(128)))
            .expect("second image");
        assert_eq!(cache.ready_len(), 1);
        assert_eq!(state.lock_recovering().retained, first_retained * 2);
        assert_eq!(first.as_ref(), &[3; 128]);
        drop(first);
        assert_eq!(state.lock_recovering().retained, first_retained);
        drop(second);
    }

    #[test]
    fn no_fill_atomic_guard_survives_publication_drop_and_releases_accounting() {
        let (memory, state) = memory(16 * 1024);
        let cache = BodyImageCache::new(memory, 4);
        let publication_image = atomic(128);
        let weak = Arc::downgrade(&publication_image);
        let pinned = cache
            .resolve_no_fill(admission(14, 0, 128), {
                let publication_image = publication_image.clone();
                move || Ok(publication_image)
            })
            .expect("resident publication image");
        let guard =
            crate::world::BodyBytes::cached(pinned.read_shared().expect("atomic bytes"), pinned);
        assert_eq!(cache.ready_len(), 0, "streaming read does not fill LRU");
        assert!(state.lock_recovering().retained > 0);

        drop(publication_image);
        assert!(weak.upgrade().is_some(), "guard pins exact image Arc");
        assert_eq!(guard.as_ref(), &[3; 128]);
        drop(guard);
        assert!(weak.upgrade().is_none());
        assert_eq!(state.lock_recovering().retained, 0);
    }

    #[test]
    fn no_fill_collaborative_guard_accounts_projection_after_publication_drop() {
        let (memory, state) = memory(64 * 1024);
        let cache = BodyImageCache::new(memory, 4);
        let publication_image = collaborative();
        let weak = Arc::downgrade(&publication_image);
        let pinned = cache
            .resolve_no_fill(admission(15, 0, 4 * 1024), {
                let publication_image = publication_image.clone();
                move || Ok(publication_image)
            })
            .expect("resident collaborative image");
        let retained_image = state.lock_recovering().retained;
        let view = pinned
            .read_collaborative()
            .expect("governed collaborative projection");
        let retained_projection = state.lock_recovering().retained;
        assert!(retained_projection > retained_image);
        let guard = crate::world::CollaborativeBody::cached(view, pinned);
        assert_eq!(cache.ready_len(), 0, "streaming read does not fill LRU");

        drop(publication_image);
        assert!(weak.upgrade().is_some(), "guard pins exact image Arc");
        assert_eq!(guard.registers["title"], b"shared");
        drop(guard);
        assert!(weak.upgrade().is_none());
        assert_eq!(state.lock_recovering().retained, 0);
    }

    #[test]
    fn pinned_eviction_can_refuse_then_release_capacity() {
        let one_transient = 64 + 512 + RETAINED_ENTRY_OVERHEAD;
        let (memory, state) = memory(one_transient + 128);
        let cache = BodyImageCache::new(memory, 1);
        let first = cache
            .resolve(admission(5, 64, 512), || Ok(atomic(128)))
            .expect("first image");
        assert!(matches!(
            cache.resolve(admission(6, 64, 512), || Ok(atomic(128))),
            Err(BodyImageFailure::Capacity)
        ));
        drop(first);
        assert_eq!(state.lock_recovering().retained, 0);
        cache
            .resolve(admission(6, 64, 512), || Ok(atomic(128)))
            .expect("capacity returns after last pin");
    }

    #[test]
    fn failure_and_panic_wake_followers_and_release_reservation() {
        let (memory, state) = memory(16 * 1024);
        let cache = Arc::new(BodyImageCache::new(memory, 4));
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let loads = Arc::new(AtomicUsize::new(0));
        let leader = {
            let cache = cache.clone();
            let entered = entered.clone();
            let release = release.clone();
            let loads = loads.clone();
            std::thread::spawn(move || {
                cache.resolve(admission(7, 64, 512), || {
                    loads.fetch_add(1, Ordering::Relaxed);
                    entered.wait();
                    release.wait();
                    Err(BodyImageFailure::KeyUnavailable)
                })
            })
        };
        entered.wait();
        let follower = {
            let cache = cache.clone();
            std::thread::spawn(move || cache.resolve(admission(7, 64, 512), || Ok(atomic(128))))
        };
        while cache.loading_waiters(admission(7, 64, 512).key) == 0 {
            std::thread::yield_now();
        }
        release.wait();
        assert!(matches!(
            leader.join().expect("leader joins"),
            Err(BodyImageFailure::KeyUnavailable)
        ));
        assert!(matches!(
            follower.join().expect("follower joins"),
            Err(BodyImageFailure::KeyUnavailable)
        ));
        assert_eq!(loads.load(Ordering::Relaxed), 1);
        assert_eq!(state.lock_recovering().building, 0);
        assert_eq!(state.lock_recovering().retained, 0);

        assert!(matches!(
            cache.resolve(admission(8, 64, 512), || -> Result<_, BodyImageFailure> {
                panic!("cancelled blocking load")
            }),
            Err(BodyImageFailure::Interrupted)
        ));
        assert_eq!(state.lock_recovering().building, 0);
    }

    #[test]
    fn admission_refusal_and_decoded_overrun_happen_atomically() {
        let (small_memory, _state) = memory(512);
        let cache = BodyImageCache::new(small_memory, 4);
        let calls = AtomicU64::new(0);
        assert!(matches!(
            cache.resolve(admission(9, 512, 512), || {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok(atomic(128))
            }),
            Err(BodyImageFailure::Capacity)
        ));
        assert_eq!(calls.load(Ordering::Relaxed), 0);

        let (memory, state) = memory(16 * 1024);
        let cache = BodyImageCache::new(memory, 4);
        assert!(matches!(
            cache.resolve(admission(10, 64, 64), || Ok(atomic(128))),
            Err(BodyImageFailure::Corrupt)
        ));
        assert_eq!(state.lock_recovering().building, 0);
        assert_eq!(state.lock_recovering().retained, 0);
    }
}
