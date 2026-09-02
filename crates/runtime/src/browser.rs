//! The browser composition root: the daemon's own Station machinery —
//! [`StationCore`], [`Session`], the one shared dock path — composed by the
//! embedding Worker instead of the native lifecycle.
//!
//! There is no lifecycle here: no Orbit, no store custody chain, no drivers,
//! no planes. The Worker owns everything the composition stands on — the
//! Replica it opened on a browser medium, the ledger it pulled over Contact,
//! the runner it instantiated under the browser's own WebAssembly — and this
//! module wires those into the *same* query machinery the daemon runs, so a
//! browser read and a daemon read are one code path. A parallel composition
//! that re-derived publications or principal facts by hand would be a second
//! model disagreeing with the first exactly when an answer mattered.
//!
//! wasm32-only by design. On native, the lifecycle's custody chain (flock,
//! epoch bump, crash recovery) is the only door to a store; offering this
//! constructor there would be a second door around it.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use mechanics::ids::DeviceId;
use replica::body::WorldId;

pub use crate::session::DockRefusal;
use crate::session::{dock_session, DockPoint, ReadMemoryGovernor, Session, StationCore};
use crate::world::{AuthorityView, LocalIdentity, PrincipalResolution};

/// Why the composition was refused.
#[derive(Debug)]
pub enum ComposeRefusal {
    /// The read-memory governor refused the Station's resident snapshot.
    ReadCapacity,
}

/// A Station composed in the browser: the daemon's read/query machinery over
/// a Worker-owned Replica, with the Worker's own ledger as the authority.
pub struct Station {
    space: mechanics::ids::SpaceId,
    core: Arc<StationCore>,
    authority: Arc<dyn AuthorityView>,
    registry: crate::registry::Catalog,
    epoch: mechanics::station::Epoch,
    find_policy: crate::find::Policy,
    alive: Arc<AtomicBool>,
}

/// Declare the registry's schemas on a Replica BEFORE remote material
/// arrives. Convergence classifies each body at incorporation: an undeclared
/// `(world, schema, version)` takes the opaque-retention branch, and only
/// re-receipt upgrades it — a later declaration reinterprets nothing. The
/// native Station declares at activation, before its Contact driver ever
/// pulls; a browser composition that pulls before composing must make the
/// same declaration through this seam, or the pull lands unreadable and the
/// dock's corpus build refuses, loudly but avoidably.
pub fn declare_schemas(replica: &mut replica::Replica, registry: &crate::registry::Catalog) {
    replica.set_supported(registry.supported_schemas());
}

impl Station {
    /// Compose the query layer over an already-opened Replica. Declares the
    /// registry's schemas on the Replica exactly as the native activation
    /// does, so Convergence classifies remote material identically — though
    /// material that already arrived undeclared stays opaque; see
    /// [`declare_schemas`]. Corpus image acceleration is native-only (`None`
    /// here): the wasm store seam refuses `sync_dir` rather than pretending.
    pub fn compose(
        space: mechanics::ids::SpaceId,
        mut replica: replica::Replica,
        authority: Arc<dyn AuthorityView>,
        registry: crate::registry::Catalog,
        epoch: mechanics::station::Epoch,
    ) -> Result<Self, ComposeRefusal> {
        declare_schemas(&mut replica, &registry);
        let core = Arc::new(
            StationCore::new(
                epoch,
                crate::session::DEFAULT_OBSERVATION_CAPACITY,
                replica,
                ReadMemoryGovernor::process_default(),
                None,
            )
            .map_err(|()| ComposeRefusal::ReadCapacity)?,
        );
        Ok(Self {
            space,
            core,
            authority,
            registry,
            epoch,
            find_policy: crate::find::Policy::default(),
            alive: Arc::new(AtomicBool::new(true)),
        })
    }

    /// Attach a local caller to a hosted World — the same derivation of
    /// principal facts, activation, and publication the native Station docks
    /// through.
    pub fn dock(
        &self,
        world_id: &WorldId,
        identity: &LocalIdentity,
    ) -> Result<Session, DockRefusal> {
        dock_session(
            DockPoint {
                space: &self.space,
                core: &self.core,
                authority: &self.authority,
                registry: &self.registry,
                epoch: self.epoch,
                find_policy: self.find_policy,
                alive: &self.alive,
            },
            world_id,
            identity,
        )
    }

    /// Proof-of-possession of a device seed, minted here because the native
    /// minting door (`Runtime::identity_from_seed`) does not exist on wasm.
    pub fn identity_from_seed(seed: &[u8; 32]) -> LocalIdentity {
        LocalIdentity::from_seed(seed)
    }
}

/// The pulled ledger as the Session's authority: every answer delegates to
/// the same `mechanics::space::Authority` calls the daemon's `SpaceAuthority`
/// makes — one ledger, one evaluation, minus the daemon. In production the
/// browser never authors founder policy; activation and capability grants
/// arrive already-signed in the ledger it pulled over Contact.
pub struct LedgerAuthorityView(pub contact::authority::SharedLedgerAuthority);

impl LedgerAuthorityView {
    fn lock(&self) -> std::sync::MutexGuard<'_, contact::authority::LedgerAuthority> {
        self.0
             .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl AuthorityView for LedgerAuthorityView {
    fn resolve(&self, device: &DeviceId) -> Option<PrincipalResolution> {
        let mut guard = self.lock();
        let actor = guard
            .ledger
            .actor_plane()
            .actor_of_device(device)
            .cloned()?;
        // An unreplayable ledger refuses rather than resolving from a clean
        // slate — the same fail-closed stance the daemon takes.
        let acl = guard.ledger.acl_state().ok()?;
        if !acl.is_member(&actor) {
            return None;
        }
        Some(PrincipalResolution {
            actor,
            authority_frontier: guard.frontier(),
        })
    }

    fn active_implementation(
        &self,
        world: &WorldId,
        authority_frontier: &replica::frontier::AuthorityFrontier,
    ) -> Result<Option<[u8; 32]>, String> {
        // A ledger error is carried, not flattened into "no activation".
        let mut guard = self.lock();
        guard
            .ledger
            .active_implementation_at(authority_frontier.as_bytes(), world.as_str())
            .map_err(|e| format!("activation state at the pinned frontier: {e}"))
    }

    #[allow(clippy::too_many_arguments)]
    fn authorize_mutation(
        &self,
        _space: &mechanics::ids::SpaceId,
        world: &WorldId,
        actor: &mechanics::ids::ActorId,
        device: &DeviceId,
        authority_frontier: &replica::frontier::AuthorityFrontier,
        parent_manifest_root: [u8; 32],
        implementation_id: [u8; 32],
        intent_digest: [u8; 32],
        demand: &[u8],
        operations_digest: [u8; 32],
        core_digest: [u8; 32],
    ) -> Result<Vec<u8>, mechanics::authorization::Refusal> {
        let mut guard = self.lock();
        let receipt = guard
            .ledger
            .authorize(&mechanics::authorization::AuthorizationRequest {
                world: world.as_str(),
                actor: actor.as_str(),
                device: device
                    .key_bytes()
                    .ok_or(mechanics::authorization::Refusal::Denied(
                        mechanics::authorization::DenialReason::Internal(
                            "device key bytes unavailable",
                        ),
                    ))?,
                authority_frontier: authority_frontier.as_bytes(),
                parent_manifest_root,
                implementation_id,
                intent_digest,
                demand,
                effect_operations_digest: operations_digest,
                body_transaction_core_digest: core_digest,
            })?;
        Ok(receipt.encode())
    }

    fn evaluate_read(
        &self,
        actor: &mechanics::ids::ActorId,
        authority_frontier: &replica::frontier::AuthorityFrontier,
        demand: &[u8],
    ) -> Result<bool, String> {
        // Failure to evaluate is not a denial: a malformed demand is a World
        // bug and an unmaterializable frontier is a ledger problem.
        let parsed = mechanics::authorization::AuthorizationDemand::decode_canonical(demand)
            .map_err(|e| format!("the read demand does not decode (a World bug): {e}"))?;
        let mut guard = self.lock();
        let view = guard
            .ledger
            .state_at(authority_frontier.as_bytes())
            .map_err(|e| format!("authority state at the pinned frontier: {e}"))?;
        Ok(view.acl.evaluate_demand(actor, &parsed).is_some())
    }
}
