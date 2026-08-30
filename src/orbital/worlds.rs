//! Product-side hosts for the Worlds installed in one active Station.
//!
//! [`runtime::world::Catalog`] owns the immutable semantic implementations used
//! by a Station. This module owns the application-side half of that boundary:
//! a process-backed [`WorldPackage`] for each selected release, and one
//! [`WorldHost`] per package inside an active Space. A package carries the reviewed semantic
//! implementation plus its optional product-neutral call handler and its
//! Runtime-validated Exec package; orbital code never needs to name the
//! product behind any of them.
//!
//! A [`WorldRouter`] belongs to one active Station. It is not a process,
//! does not own a listener, and has no autonomous background loop.

use runtime::poison::LockRecovering;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use mechanics::ids::{DeviceId, SpaceId};
use replica::body::WorldId;
use runtime::world::Refusal as RegistrationRefusal;
use runtime::Error as RuntimeFailure;
use runtime::{
    world::Builder, world::Catalog, world::LocalIdentity, world::World, Session, Station,
};

use runtime::world::call::{Access, Call, Code, Failure, Handler};
#[cfg(test)]
use runtime::world::call::{Context, Reply};

/// One World's Observation projection, in that World's own vocabulary.
///
/// Defined by `runtime` so a World supplied out of tree can name it without
/// depending on this shell.
pub use runtime::world::{Invalidation, RoutedInvalidation};

pub use world_sdk::{
    BootstrapContext, FounderGrant, InitialScope, ReviewedImplementation, StatusProjection,
    WorldApplication as WorldLifecycle, WorldUpgradeAssessment, WorldUpgradeContext,
    WorldUpgradeProgress,
};

/// A World package's Observation projector. Implementations own their own
/// baselines; the Station host only fans generic observations out.
///
/// The invalidation vocabulary lives in `runtime`, so an out-of-tree World can
/// project without depending on this shell. The trait stays application-side
/// because it combines World projection with shell-facing status metadata and
/// projector registration.
pub trait ObservationProjector: Send + Sync {
    fn status(&self, session: &Session) -> Option<StatusProjection>;
    fn start(&self, session: &Session, space: &mechanics::ids::SpaceId);
    fn project(
        &self,
        session: &Session,
        space: &mechanics::ids::SpaceId,
        observation: &runtime::world::Observation,
    ) -> Invalidation;
}

/// Host-visible result of one daemon-owned lifecycle turn.
///
/// Product record bytes never cross this boundary: the Station host persists
/// them before returning one of these bounded progress facts to its daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorldUpgradeStep {
    Current,
    /// This Space never activated the World. An update of another Space must
    /// not turn that absence into a new binding.
    Unbound,
    Pending {
        completed: u64,
        remaining: Option<u64>,
    },
    /// Exact source reconstruction has been admitted and is progressing off
    /// the daemon reactor and Station locks.
    Building,
    /// Runtime's conscious read envelope refused this source reconstruction.
    Capacity,
    Verified,
    Unsupported {
        reason: String,
    },
}

impl<T: WorldLifecycle> ObservationProjector for T {
    fn status(&self, session: &Session) -> Option<StatusProjection> {
        WorldLifecycle::status(self, session)
    }

    fn start(&self, session: &Session, space: &SpaceId) {
        WorldLifecycle::start_projector(self, session, space);
    }

    fn project(
        &self,
        session: &Session,
        space: &SpaceId,
        observation: &runtime::world::Observation,
    ) -> Invalidation {
        WorldLifecycle::project(self, session, space, observation)
    }
}

/// One exact World implementation available to this daemon generation.
#[derive(Clone)]
pub struct WorldPackage {
    world: WorldId,
    implementation: Arc<dyn World>,
    reviewed_implementation: [u8; 32],
    control: Option<Arc<dyn Handler>>,
    exec: runtime::exec::Package,
    projector: Option<Arc<dyn ObservationProjector>>,
    lifecycle: Option<Arc<dyn WorldLifecycle>>,
    /// Immutable distribution release this package was launched from. Tests
    /// and Runtime-only embedders may construct packages without one.
    release_version: Option<String>,
    /// The package an unformed Space activates by default. Historical exact
    /// packages remain installed for retained publication reads and existing
    /// Spaces whose authority still selects them.
    preferred: bool,
}

impl std::fmt::Debug for WorldPackage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldPackage")
            .field("world", &self.world)
            .field(
                "reviewed_implementation",
                &data_encoding::HEXLOWER.encode(&self.reviewed_implementation[..8]),
            )
            .field("has_call_handler", &self.control.is_some())
            .field("exec", &self.exec)
            .finish()
    }
}

impl WorldPackage {
    pub fn new(implementation: Arc<dyn World>, reviewed_implementation: [u8; 32]) -> Self {
        let world = implementation.descriptor().id;
        Self {
            world,
            implementation,
            reviewed_implementation,
            control: None,
            exec: runtime::exec::Package::new(),
            projector: None,
            lifecycle: None,
            release_version: None,
            preferred: true,
        }
    }

    /// The version this package's World declares for itself.
    ///
    /// Read off the descriptor rather than taken as a second constructor
    /// argument, for the same reason `Implementation::from_registration` reads
    /// it off the registration: a version passed alongside the implementation
    /// is a second source of truth that can disagree with the one the id was
    /// hashed over, and the disagreement would be invisible.
    pub fn reviewed_version(&self) -> u32 {
        self.implementation.descriptor().implementation_version.0
    }

    pub fn with_control(mut self, control: Arc<dyn Handler>) -> Self {
        self.control = Some(control);
        self
    }

    pub fn with_exec(mut self, exec: runtime::exec::Package) -> Self {
        self.exec = exec;
        self
    }

    pub fn with_projector(mut self, projector: Arc<dyn ObservationProjector>) -> Self {
        self.projector = Some(projector);
        self
    }

    pub fn with_lifecycle(mut self, lifecycle: Arc<dyn WorldLifecycle>) -> Self {
        self.lifecycle = Some(lifecycle);
        self
    }

    /// Pin the independently distributed release this process came from.
    pub fn with_release_version(mut self, version: impl Into<String>) -> Self {
        self.release_version = Some(version.into());
        self
    }

    /// Install this exact implementation for historical/authority-selected
    /// use without making it the formation default for its World.
    pub fn historical(mut self) -> Self {
        self.preferred = false;
        self
    }

    pub fn world_id(&self) -> &WorldId {
        &self.world
    }
}

/// One assignment a membership role expands to, and which role definition
/// said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MembershipAssignment {
    pub capability: mechanics::authorization::PolicyCapability,
    pub resource: mechanics::authorization::Resource,
    /// The World's opaque reference to the role definition — what
    /// [`mechanics::membership::GrantOrigin::Membership`] records.
    pub definition_ref: Vec<u8>,
}

/// The selected process generations installed for one daemon launch.
///
/// The package set is cloned down the Daemon → Station placement →
/// StationHost call stack. Each Space freezes its own Runtime registry and
/// host objects from that exact reviewed generation.
#[derive(Clone, Default)]
pub struct WorldPackages {
    packages: Vec<WorldPackage>,
}

impl WorldPackages {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one reviewed World package.
    ///
    /// Duplicate ids and registration/implementation mismatches are rejected by
    /// the existing [`Builder`] validation when [`Self::build`] freezes
    /// the set.
    pub fn with_package(mut self, package: WorldPackage) -> Self {
        self.packages.push(package);
        self
    }

    /// Add a semantic-only World. Kept for independent Runtime adopters that
    /// need no application control adapter.
    pub fn register(
        self,
        implementation: Arc<dyn World>,
        reviewed_implementation: [u8; 32],
    ) -> Self {
        self.with_package(WorldPackage::new(implementation, reviewed_implementation))
    }

    pub fn world_ids(&self) -> impl Iterator<Item = &WorldId> {
        self.packages
            .iter()
            .filter(|package| package.preferred)
            .map(WorldPackage::world_id)
    }

    pub fn contains(&self, world: &WorldId) -> bool {
        self.packages
            .iter()
            .any(|package| package.world_id() == world)
    }

    pub fn reviewed_implementation(&self, world: &WorldId) -> Option<[u8; 32]> {
        self.packages
            .iter()
            .find(|package| package.world_id() == world && package.preferred)
            .map(|package| package.reviewed_implementation)
    }

    /// Distribution release actually running in this daemon generation.
    pub fn release_version(&self, world: &WorldId) -> Option<&str> {
        self.packages
            .iter()
            .find(|package| package.world_id() == world && package.preferred)
            .and_then(|package| package.release_version.as_deref())
    }

    /// The reviewed id *and* the version its descriptor declares — the pair a
    /// catch-up needs to decide whether this build is ahead of the Space.
    pub fn reviewed_state(&self, world: &WorldId) -> Option<([u8; 32], u32)> {
        self.packages
            .iter()
            .find(|package| package.world_id() == world && package.preferred)
            .map(|package| (package.reviewed_implementation, package.reviewed_version()))
    }

    pub fn founder_policies(
        &self,
    ) -> anyhow::Result<Vec<(WorldId, [u8; 32], u32, Vec<FounderGrant>)>> {
        self.packages
            .iter()
            .filter(|package| package.preferred)
            .filter_map(|package| {
                package.lifecycle.as_deref().map(|lifecycle| {
                    lifecycle.founder_grants().map(|grants| {
                        (
                            package.world.clone(),
                            package.reviewed_implementation,
                            package.reviewed_version(),
                            grants,
                        )
                    })
                })
            })
            .collect()
    }

    pub fn initial_scopes(&self, display_name: &str) -> Vec<(WorldId, InitialScope)> {
        self.packages
            .iter()
            .filter(|package| package.preferred)
            .filter_map(|package| {
                package.lifecycle.as_deref().and_then(|lifecycle| {
                    lifecycle
                        .initial_scope(display_name)
                        .map(|scope| (package.world.clone(), scope))
                })
            })
            .collect()
    }

    pub fn lifecycle_world_ids(&self) -> impl Iterator<Item = &WorldId> {
        self.packages
            .iter()
            .filter(|package| package.preferred && package.lifecycle.is_some())
            .map(WorldPackage::world_id)
    }

    pub fn bootstrap(&self, world: &WorldId, context: BootstrapContext<'_>) -> anyhow::Result<()> {
        let package = self
            .packages
            .iter()
            .find(|package| package.world_id() == world && package.preferred)
            .ok_or_else(|| anyhow::anyhow!("World '{world}' has no selected package"))?;
        let lifecycle = package
            .lifecycle
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("World '{world}' has no formation lifecycle"))?;
        lifecycle.bootstrap(context)
    }

    pub fn accepts_call(&self, call: &Call) -> bool {
        self.call_access(call).is_ok()
    }

    pub fn call_access(&self, call: &Call) -> Result<Access, Failure> {
        let control = self
            .packages
            .iter()
            .find(|package| package.world_id() == call.world() && package.preferred)
            .and_then(|package| package.control.as_deref())
            .ok_or_else(|| {
                Failure::new(
                    Code::UnsupportedOperation,
                    format!("World '{}' has no application call handler", call.world()),
                )
            })?;
        control.access(call)
    }

    /// Freeze the semantic registry and create one application host per
    /// registered World.
    pub fn build(&self) -> Result<(Catalog, WorldRouter), RegistrationRefusal> {
        let mut runtime = Builder::new();
        let mut hosts = Vec::with_capacity(self.packages.len());
        let mut preferred = BTreeMap::new();
        for package in &self.packages {
            let descriptor = package.implementation.descriptor();
            package
                .exec
                .validate(
                    &package.world,
                    &package.reviewed_implementation,
                    &descriptor,
                )
                .map_err(|reason| RegistrationRefusal::InvalidExecPackage {
                    world: package.world.clone(),
                    reason,
                })?;
            hosts.push((
                package.world.clone(),
                package.reviewed_implementation,
                package.reviewed_version(),
                package.control.clone(),
                package.exec.clone(),
                package.projector.clone(),
                package.lifecycle.clone(),
            ));
            if package.preferred
                && preferred
                    .insert(package.world.clone(), package.reviewed_implementation)
                    .is_some()
            {
                return Err(RegistrationRefusal::AmbiguousWorldDefault(
                    package.world.clone(),
                ));
            }
            runtime = runtime.register_reviewed(
                package.implementation.clone(),
                package.reviewed_implementation,
            );
        }
        for world in self
            .packages
            .iter()
            .map(WorldPackage::world_id)
            .collect::<std::collections::BTreeSet<_>>()
        {
            if !preferred.contains_key(world) {
                return Err(RegistrationRefusal::AmbiguousWorldDefault(world.clone()));
            }
        }
        let registry = runtime.build()?;
        Ok((
            registry,
            WorldRouter::new(
                hosts
                    .into_iter()
                    .map(
                        |(world, reviewed, version, control, exec, projector, lifecycle)| {
                            (
                                (world.clone(), reviewed),
                                WorldHost::new(
                                    world, reviewed, version, control, exec, projector, lifecycle,
                                ),
                            )
                        },
                    )
                    .collect(),
                preferred,
            ),
        ))
    }
}

/// The sole product-side entrance to one World in one active Space.
///
/// Primary and sponsored-agent Sessions cannot be reused across Worlds because
/// each host owns only the Sessions docked to its own [`WorldId`].
pub struct WorldHost {
    world: WorldId,
    reviewed_implementation: [u8; 32],
    /// The version the reviewed descriptor declares. Carried beside the id
    /// because a `WorldHost` never sees the `World` itself, so it cannot go
    /// back to the descriptor to ask.
    reviewed_version: u32,
    control: Option<Arc<dyn Handler>>,
    exec: runtime::exec::Package,
    projector: Option<Arc<dyn ObservationProjector>>,
    lifecycle: Option<Arc<dyn WorldLifecycle>>,
    primary_session: Mutex<Option<Session>>,
    agent_sessions: Mutex<HashMap<DeviceId, Session>>,
}

impl std::fmt::Debug for WorldHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldHost")
            .field("world", &self.world)
            .finish_non_exhaustive()
    }
}

impl WorldHost {
    fn new(
        world: WorldId,
        reviewed_implementation: [u8; 32],
        reviewed_version: u32,
        control: Option<Arc<dyn Handler>>,
        exec: runtime::exec::Package,
        projector: Option<Arc<dyn ObservationProjector>>,
        lifecycle: Option<Arc<dyn WorldLifecycle>>,
    ) -> Self {
        Self {
            world,
            reviewed_version,
            reviewed_implementation,
            control,
            exec,
            projector,
            lifecycle,
            primary_session: Mutex::new(None),
            agent_sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn world_id(&self) -> &WorldId {
        &self.world
    }

    pub fn reviewed_implementation(&self) -> &[u8; 32] {
        &self.reviewed_implementation
    }

    /// The implementation version declared by this selected World runner.
    pub fn reviewed_version(&self) -> u32 {
        self.reviewed_version
    }

    pub fn reviewed_state(&self) -> ReviewedImplementation {
        ReviewedImplementation {
            id: self.reviewed_implementation,
            version: self.reviewed_version,
        }
    }

    pub fn lifecycle(&self) -> Option<&dyn WorldLifecycle> {
        self.lifecycle.as_deref()
    }

    fn clear_sessions(&self) {
        // Move guards out before dropping Sessions: deregistration may touch
        // Runtime state and must not happen while either host-local mutex is
        // held.
        let primary = self.primary_session.lock_recovering().take();
        let agents = std::mem::take(&mut *self.agent_sessions.lock_recovering());
        drop(primary);
        drop(agents);
    }

    pub fn control(&self) -> Option<&dyn Handler> {
        self.control.as_deref()
    }

    pub fn exec(&self) -> &runtime::exec::Package {
        &self.exec
    }

    /// Observe committed unresolved Runs and perform one local Attempt pass.
    pub fn perform(
        &self,
        session: &Session,
        put_output: impl FnMut(&[u8]) -> Result<replica::content::ContentRef, runtime::world::Failure>,
    ) -> Result<runtime::exec::PerformReport, runtime::world::Failure> {
        session.perform(&self.exec, put_output)
    }

    /// Dispose of resolved, uncited Runs past the activation's retention
    /// grace. A no-op unless the operator set a window.
    pub fn sweep_runs(
        &self,
        session: &Session,
    ) -> Result<Vec<runtime::exec::RunId>, runtime::world::Failure> {
        session.sweep_resolved_runs()
    }

    /// Presume foreign in-flight Attempts dead once their deadline has elapsed
    /// on this Station's own clock. Non-terminal: the executor can still
    /// return. Requires the Spec's `control` demand, enforced at commit.
    pub fn sweep_liveness(
        &self,
        session: &Session,
        now_millis: u64,
        grace_millis: u64,
    ) -> Result<Vec<(runtime::exec::RunId, runtime::exec::AttemptId)>, runtime::world::Failure>
    {
        session.sweep_liveness(now_millis, grace_millis)
    }

    /// Ensure the Space's primary identity has a Session for this World.
    ///
    /// Docking remains lazy: an unadmitted joiner can keep its StationHost
    /// active to drive Contact before Mechanics grants it standing.
    pub fn ensure_primary(
        &self,
        station: &Station,
        identity: &LocalIdentity,
    ) -> Result<(), RuntimeFailure> {
        let mut session = self.primary_session.lock_recovering();
        if session.is_none() {
            *session = Some(station.dock(&self.world, identity)?);
        }
        Ok(())
    }

    pub fn with_primary<R>(&self, f: impl FnOnce(&Session) -> R) -> Option<R> {
        let session = self.primary_session.lock_recovering();
        session.as_ref().map(f)
    }

    /// Dock or reuse a sponsored local agent's Session, then run `f` with it.
    pub fn with_agent<R>(
        &self,
        station: &Station,
        identity: &LocalIdentity,
        f: impl FnOnce(&Session) -> R,
    ) -> Result<R, RuntimeFailure> {
        let mut sessions = self.agent_sessions.lock_recovering();
        let device = identity.device().clone();
        if !sessions.contains_key(&device) {
            sessions.insert(device.clone(), station.dock(&self.world, identity)?);
        }
        sessions
            .get(&device)
            .map(f)
            .ok_or_else(|| RuntimeFailure::UnknownWorld(self.world.clone()))
    }
}

/// The World hosts enabled inside one active StationHost.
pub struct WorldRouter {
    hosts: BTreeMap<(WorldId, [u8; 32]), WorldHost>,
    preferred: BTreeMap<WorldId, [u8; 32]>,
    /// The sole ordinary-routing selector. Docking a replacement happens
    /// before this short swap; prior Sessions are retired after it, so readers
    /// see either exact old or exact new and never BTreeMap order.
    active: Mutex<BTreeMap<WorldId, [u8; 32]>>,
}

impl std::fmt::Debug for WorldRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldRouter")
            .field("worlds", &self.preferred.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl WorldRouter {
    fn new(
        hosts: BTreeMap<(WorldId, [u8; 32]), WorldHost>,
        preferred: BTreeMap<WorldId, [u8; 32]>,
    ) -> Self {
        Self {
            hosts,
            preferred,
            active: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn world_ids(&self) -> impl Iterator<Item = &WorldId> {
        self.preferred.keys()
    }

    pub fn contains(&self, world: &WorldId) -> bool {
        self.preferred.contains_key(world)
    }

    pub fn host(&self, world: &WorldId) -> Option<&WorldHost> {
        let active = self.active.lock_recovering().get(world).copied();
        active
            .and_then(|implementation| self.host_for(world, implementation))
            .or_else(|| self.preferred_host(world))
    }

    pub fn host_for(&self, world: &WorldId, implementation: [u8; 32]) -> Option<&WorldHost> {
        self.hosts.get(&(world.clone(), implementation))
    }

    pub fn preferred_host(&self, world: &WorldId) -> Option<&WorldHost> {
        self.host_for(world, *self.preferred.get(world)?)
    }

    pub fn reviewed_implementations(&self) -> impl Iterator<Item = (&WorldId, &[u8; 32])> {
        self.preferred
            .iter()
            .map(|(world, implementation)| (world, implementation))
    }

    /// Every hosted World's reviewed id with the version beside it.
    pub fn reviewed_states(&self) -> impl Iterator<Item = (&WorldId, [u8; 32], u32)> {
        self.preferred.iter().filter_map(|(world, implementation)| {
            let host = self.host_for(world, *implementation)?;
            Some((
                world,
                *host.reviewed_implementation(),
                host.reviewed_version(),
            ))
        })
    }

    pub fn upgrade_assessment(
        &self,
        world: &WorldId,
        active: Option<ReviewedImplementation>,
    ) -> anyhow::Result<WorldUpgradeAssessment> {
        let preferred = self
            .preferred_host(world)
            .ok_or_else(|| anyhow::anyhow!("World '{world}' has no preferred package"))?;
        let preferred_state = preferred.reviewed_state();
        match preferred.lifecycle() {
            Some(lifecycle) => lifecycle.assess_upgrade(active, preferred_state),
            None if active.is_some_and(|active| active.id == preferred_state.id) => {
                Ok(WorldUpgradeAssessment::Current)
            }
            None => Ok(WorldUpgradeAssessment::Direct),
        }
    }

    pub fn verification_migrator(&self, world: &WorldId) -> Option<ReviewedImplementation> {
        let preferred = self.preferred_host(world)?;
        preferred
            .lifecycle()?
            .verification_migrator(preferred.reviewed_state())
    }

    /// Ask one exact preferred World generation to expand a role selector.
    ///
    /// With no explicit World, exactly one installed generation must claim the
    /// selector. This preserves a convenient default without making install
    /// order or a host-compiled product name into policy.
    pub fn admission_evidence(
        &self,
        world: Option<&WorldId>,
        role: &str,
        parent_manifest_root: [u8; 32],
    ) -> anyhow::Result<mechanics::authorization::WorldAssignmentEvidence> {
        if let Some(world) = world {
            let lifecycle = self
                .preferred_host(world)
                .and_then(WorldHost::lifecycle)
                .ok_or_else(|| anyhow::anyhow!("World '{world}' defines no admission roles"))?;
            return lifecycle
                .admission_evidence(role, parent_manifest_root)?
                .ok_or_else(|| anyhow::anyhow!("World '{world}' defines no admission roles"));
        }

        let mut claimed = Vec::new();
        for world in self.world_ids() {
            let Some(lifecycle) = self.preferred_host(world).and_then(WorldHost::lifecycle) else {
                continue;
            };
            if let Some(evidence) = lifecycle.admission_evidence(role, parent_manifest_root)? {
                claimed.push((world.clone(), evidence));
            }
        }
        match claimed.len() {
            0 => anyhow::bail!("unknown role '{role}': no installed World defines it"),
            1 => Ok(claimed.remove(0).1),
            _ => {
                // Name the claimants: the usual pair is a release and a local
                // copy of it, and "ambiguous" alone reads as a broken role
                // rather than a second Issues on the machine.
                let claimants = claimed
                    .iter()
                    .map(|(world, _)| world.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                anyhow::bail!(
                    "admission role '{role}' is ambiguous across installed Worlds \
                     ({claimants}); name a World"
                )
            }
        }
    }

    /// Expand a conventional membership role across every installed World
    /// that claims it. Root membership remains product-neutral; each World
    /// supplies the exact scoped assignments that make that membership useful
    /// in its own namespace.
    /// Every installed World's exact expansion of one membership role
    /// selector, each assignment beside the opaque reference to the role
    /// definition it came from — the same reference an admission carries, so
    /// a role change records the same provenance a redemption would.
    pub fn membership_assignments(
        &self,
        role: &str,
        parent_manifest_root: [u8; 32],
    ) -> anyhow::Result<Vec<MembershipAssignment>> {
        let mut assignments = Vec::new();
        for world in self.world_ids() {
            let Some(lifecycle) = self.preferred_host(world).and_then(WorldHost::lifecycle) else {
                continue;
            };
            let Some(evidence) = lifecycle.admission_evidence(role, parent_manifest_root)? else {
                continue;
            };
            evidence.validate().map_err(|error| {
                anyhow::anyhow!("World '{world}' returned invalid role evidence: {error}")
            })?;
            if evidence.world != world.as_str() {
                anyhow::bail!(
                    "World '{world}' returned membership evidence for '{}'",
                    evidence.world
                );
            }
            let mut expansion = evidence.assignments;
            expansion.sort();
            expansion.dedup();
            assignments.extend(expansion.into_iter().map(|(capability, resource)| {
                MembershipAssignment {
                    capability,
                    resource,
                    definition_ref: evidence.opaque_definition_ref.clone(),
                }
            }));
        }
        Ok(assignments)
    }

    pub fn has_reviewed_implementation(
        &self,
        world: &WorldId,
        implementation: ReviewedImplementation,
    ) -> bool {
        self.host_for(world, implementation.id)
            .is_some_and(|host| host.reviewed_version() == implementation.version)
    }

    pub fn with_primary_for<R>(
        &self,
        world: &WorldId,
        implementation: [u8; 32],
        f: impl FnOnce(&Session) -> R,
    ) -> Option<R> {
        self.host_for(world, implementation)?.with_primary(f)
    }

    pub fn ensure_primary(
        &self,
        station: &Station,
        world: &WorldId,
        identity: &LocalIdentity,
    ) -> Result<(), RuntimeFailure> {
        let implementation = station.active_implementation(world, identity)?;
        let target = self
            .host_for(world, implementation)
            .ok_or_else(|| RuntimeFailure::UnknownWorld(world.clone()))?;
        target.ensure_primary(station, identity)?;
        self.active
            .lock_recovering()
            .insert(world.clone(), implementation);
        // Activation is a single exact implementation coordinate. Once the
        // new Session is ready, retire every prior package Session for this
        // World so BTreeMap order can never route ordinary work to a migrator.
        for ((candidate, reviewed), host) in &self.hosts {
            if candidate == world && *reviewed != implementation {
                host.clear_sessions();
            }
        }
        Ok(())
    }

    pub fn with_primary<R>(&self, world: &WorldId, f: impl FnOnce(&Session) -> R) -> Option<R> {
        self.host(world)?.with_primary(f)
    }

    /// Run `f` with any docked primary Session.
    ///
    /// Station observations and authority doorbells are shared across Worlds,
    /// so Space-level adapters need exactly one Session to publish that plane.
    pub fn with_any_primary<R>(&self, f: impl FnOnce(&Session) -> R) -> Option<R> {
        let active = self.active.lock_recovering().clone();
        for (world, implementation) in active {
            let Some(host) = self.host_for(&world, implementation) else {
                continue;
            };
            let session = host.primary_session.lock_recovering();
            if let Some(session) = session.as_ref() {
                return Some(f(session));
            }
        }
        None
    }

    pub fn start_projectors(&self, space: &mechanics::ids::SpaceId) {
        let active = self.active.lock_recovering().clone();
        for (world, implementation) in active {
            let Some(host) = self.host_for(&world, implementation) else {
                continue;
            };
            let Some(projector) = host.projector.as_deref() else {
                continue;
            };
            let session = host.primary_session.lock_recovering();
            if let Some(session) = session.as_ref() {
                projector.start(session, space);
            }
        }
    }

    pub fn status(&self) -> Option<StatusProjection> {
        let mut combined: Option<StatusProjection> = None;
        let active = self.active.lock_recovering().clone();
        for (world, implementation) in active {
            let Some(host) = self.host_for(&world, implementation) else {
                continue;
            };
            let Some(projector) = host.projector.as_deref() else {
                continue;
            };
            let session = host.primary_session.lock_recovering();
            // A host with nothing to say is skipped, never propagated: with a
            // second selected World this loop visits hosts that hold no session
            // on this Space at all (Signage on a board-only Space), and an
            // early `?` here let that silence erase the answer of the World
            // that had one — the joiner's board synced and its status still
            // read "no board data", which the join diagnosis renders as a
            // sync that never completes.
            let Some(status) = session
                .as_ref()
                .and_then(|session| projector.status(session))
            else {
                continue;
            };
            match &mut combined {
                Some(total) => {
                    total.items = total.items.saturating_add(status.items);
                    total.scopes = total.scopes.saturating_add(status.scopes);
                    if total.name.is_empty() {
                        total.name = status.name;
                    }
                    if total.description.is_empty() {
                        total.description = status.description;
                    }
                }
                None => combined = Some(status),
            }
        }
        combined
    }

    /// Fan one Observation out to every hosted World, preserving the World
    /// boundary around each package's vocabulary.
    pub fn project(
        &self,
        space: &mechanics::ids::SpaceId,
        observation: &runtime::world::Observation,
    ) -> Vec<RoutedInvalidation> {
        let mut projected = Vec::new();
        let active = self.active.lock_recovering().clone();
        for (world, implementation) in active {
            let Some(host) = self.host_for(&world, implementation) else {
                continue;
            };
            let Some(projector) = host.projector.as_deref() else {
                continue;
            };
            let session = host.primary_session.lock_recovering();
            let Some(session) = session.as_ref() else {
                continue;
            };
            let next = projector.project(session, space, observation);
            if !next.dirty.is_empty() || !next.planes.is_empty() {
                projected.push(RoutedInvalidation {
                    world: host.world.clone(),
                    dirty: next.dirty,
                    planes: next.planes,
                });
            }
        }
        projected
    }

    pub fn with_agent<R>(
        &self,
        station: &Station,
        world: &WorldId,
        identity: &LocalIdentity,
        f: impl FnOnce(&Session) -> R,
    ) -> Result<R, RuntimeFailure> {
        let implementation = station.active_implementation(world, identity)?;
        self.host_for(world, implementation)
            .ok_or_else(|| RuntimeFailure::UnknownWorld(world.clone()))?
            .with_agent(station, identity, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use replica::body::Schema;
    use replica::body::WorldId;
    use runtime::{
        world::Context as WorldContext, world::Effect, world::Intent, world::Projection,
        world::Query,
    };

    struct NoopWorld {
        id: WorldId,
        schemas: Vec<Schema>,
        exec_specs: Vec<runtime::exec::Spec>,
    }

    struct ProjectControl;

    impl Handler for ProjectControl {
        fn access(&self, call: &Call) -> Result<Access, Failure> {
            if call.operation() == "projects.control" && call.version() == 1 {
                Ok(Access::Query)
            } else {
                Err(Failure::new(
                    Code::UnsupportedOperation,
                    "unsupported project call",
                ))
            }
        }

        fn call(&self, call: &Call, _context: &super::Context<'_>) -> Reply {
            Reply::ok(call, b"{}".to_vec())
        }
    }

    impl World for NoopWorld {
        fn id(&self) -> WorldId {
            self.id.clone()
        }

        fn schemas(&self) -> &[Schema] {
            &self.schemas
        }

        fn exec_specs(&self) -> &[runtime::exec::Spec] {
            &self.exec_specs
        }

        fn submit(
            &self,
            _ctx: &mut WorldContext<'_>,
            _intent: Intent,
        ) -> Result<Effect, runtime::world::Rejection> {
            unreachable!("registry tests never execute the World")
        }

        fn query(
            &self,
            _ctx: &WorldContext<'_>,
            _query: Query,
        ) -> Result<Projection, runtime::world::Rejection> {
            unreachable!("registry tests never execute the World")
        }
    }

    fn package(id: &str, marker: u8) -> (Arc<dyn World>, [u8; 32]) {
        let id = WorldId::parse(id).expect("test World id");
        let schemas = Vec::new();
        (
            Arc::new(NoopWorld {
                id,
                schemas,
                exec_specs: Vec::new(),
            }),
            [marker; 32],
        )
    }

    fn exec_demand(capability: &str) -> Vec<u8> {
        mechanics::authorization::AuthorizationDemand::require(
            mechanics::authorization::PolicyCapability::new("com.example.jobs", capability),
            mechanics::authorization::Resource::root("com.example.jobs"),
        )
        .encode_canonical()
        .unwrap()
    }

    fn exec_spec() -> runtime::exec::Spec {
        let payload = |name: &str| runtime::exec::PayloadSpec {
            schema: runtime::exec::SchemaRef {
                name: replica::body::SchemaId::parse(name).unwrap(),
                version: 1,
            },
            max_inline_bytes: 1_024,
            max_content_refs: 0,
            max_content_bytes: 0,
            read: exec_demand("payload.read"),
            max_additional_input_bytes: 0,
        };
        runtime::exec::Spec {
            name: replica::body::SchemaId::parse("job").unwrap(),
            version: 1,
            access: runtime::exec::Access {
                start: exec_demand("start"),
                offer: exec_demand("offer"),
                control: exec_demand("control"),
                accept: exec_demand("accept"),
            },
            input: payload("job.input"),
            output: payload("job.output"),
            mode: runtime::exec::Mode::Unary,
            resume: runtime::exec::Resume::Restart,
            effects: runtime::exec::Effects::Pure,
            accept: runtime::exec::AcceptRule::World,
            queries: Vec::new(),
            service: None,
            links: Vec::new(),
            limits: runtime::exec::Limits {
                attempts: 2,
                events: 32,
                checkpoints: 0,
                child_runs: 1,
                progress_bytes: 1_024,
                checkpoint_bytes: 0,
                wall_millis: 30_000,
            },
        }
    }

    struct CheckHandler {
        binding: runtime::exec::HandlerBinding,
    }

    impl runtime::exec::Handler for CheckHandler {
        fn binding(&self) -> &runtime::exec::HandlerBinding {
            &self.binding
        }

        fn handle(
            &self,
            _context: &mut dyn runtime::exec::HandlerContext,
        ) -> Result<runtime::exec::Candidate, runtime::exec::Failure> {
            Ok(runtime::exec::Candidate {
                output: runtime::exec::SchemaRef {
                    name: replica::body::SchemaId::parse("job.output").unwrap(),
                    version: 1,
                },
                inline: Vec::new(),
                content: Vec::new(),
                content_bytes: 0,
                terminal: runtime::exec::TerminalClass::Succeeded,
                usage: Vec::new(),
                evidence: Vec::new(),
            })
        }
    }

    fn executable_package() -> (Arc<dyn World>, [u8; 32], runtime::exec::Package) {
        let world = WorldId::parse("com.example.jobs").unwrap();
        let reviewed = [0x77; 32];
        let spec = exec_spec();
        let seed = [0x78; 32];
        let build = runtime::exec::Build {
            id: runtime::exec::BuildId::from_bytes([0; 32]),
            world: world.clone(),
            world_build: reviewed,
            spec: runtime::exec::SchemaRef {
                name: spec.name.clone(),
                version: spec.version,
            },
            handler: replica::content::ContentRef {
                content_id: [0x79; 32],
            },
            dependencies: None,
            environment: [0x7a; 32],
            config: Vec::new(),
            checkpoint: None,
            replay_commands: None,
            compatible_from: Vec::new(),
            publisher: mechanics::ids::ActorId::from_incept_hash(&"a".repeat(64)),
            signature: runtime::exec::Signature {
                signer: mechanics::actor::device_from_seed(&seed),
                algorithm: 1,
                bytes: [0; 64],
            },
        }
        .sign(&seed)
        .unwrap();
        let handler = Arc::new(CheckHandler {
            binding: runtime::exec::HandlerBinding {
                spec: build.spec.clone(),
                build: build.id,
                artifact: build.handler,
                role: None,
                links: Vec::new(),
            },
        });
        let exec = runtime::exec::Package::new()
            .with_spec(spec.clone())
            .with_build(build)
            .with_handler(handler);
        (
            Arc::new(NoopWorld {
                id: world,
                schemas: Vec::new(),
                exec_specs: vec![spec],
            }),
            reviewed,
            exec,
        )
    }

    #[test]
    fn one_space_has_one_host_per_registered_world() {
        let a = package("com.example.files", 1);
        let b = package("com.example.notes", 2);
        let (registry, hosts) = WorldPackages::new()
            .with_package(WorldPackage::new(a.0, a.1))
            .with_package(WorldPackage::new(b.0, b.1))
            .build()
            .unwrap();

        let ids: Vec<_> = hosts.world_ids().map(WorldId::as_str).collect();
        assert_eq!(ids, ["com.example.files", "com.example.notes"]);
        assert_eq!(registry.len(), 2);
        assert_eq!(
            hosts
                .reviewed_implementations()
                .map(|(id, implementation)| (id.as_str(), implementation[0]))
                .collect::<Vec<_>>(),
            [("com.example.files", 1), ("com.example.notes", 2)]
        );
        assert_ne!(
            hosts
                .host(&WorldId::parse("com.example.files").unwrap())
                .unwrap() as *const WorldHost,
            hosts
                .host(&WorldId::parse("com.example.notes").unwrap())
                .unwrap() as *const WorldHost,
            "each World must have a distinct host object"
        );
    }

    #[test]
    fn multiple_current_defaults_for_one_world_are_rejected() {
        let a = package("com.example.files", 1);
        let b = package("com.example.files", 2);
        let err = WorldPackages::new()
            .with_package(WorldPackage::new(a.0, a.1))
            .with_package(WorldPackage::new(b.0, b.1))
            .build()
            .unwrap_err();
        assert_eq!(
            err,
            RegistrationRefusal::AmbiguousWorldDefault(
                WorldId::parse("com.example.files").expect("test World id")
            )
        );
    }

    #[test]
    fn historical_and_preferred_packages_are_exact_and_default_is_explicit() {
        let old = package("com.example.files", 1);
        let current = package("com.example.files", 2);
        let (registry, hosts) = WorldPackages::new()
            .with_package(WorldPackage::new(old.0, old.1).historical())
            .with_package(WorldPackage::new(current.0, current.1))
            .build()
            .unwrap();
        let world = WorldId::parse("com.example.files").unwrap();

        assert!(registry.world_for(&world, [1; 32]).is_some());
        assert!(registry.world_for(&world, [2; 32]).is_some());
        assert_eq!(
            hosts
                .preferred_host(&world)
                .unwrap()
                .reviewed_implementation(),
            &[2; 32]
        );
        assert!(hosts.host_for(&world, [1; 32]).is_some());
        assert!(hosts.host_for(&world, [2; 32]).is_some());
    }

    #[test]
    fn call_handlers_are_owned_by_the_registered_package() {
        let files = package("com.example.files", 1);
        let notes = package("com.example.notes", 2);
        let control = Arc::new(ProjectControl);
        let packages = WorldPackages::new()
            .with_package(WorldPackage::new(files.0, files.1))
            .with_package(WorldPackage::new(notes.0, notes.1).with_control(control));
        let files = WorldId::parse("com.example.files").unwrap();
        let notes = WorldId::parse("com.example.notes").unwrap();

        let call = Call::new(notes.clone(), "projects.control", 1, Vec::new()).unwrap();
        assert!(packages.accepts_call(&call));
        let (_, hosts) = packages.build().unwrap();
        assert!(hosts.host(&files).unwrap().control().is_none());
        assert!(hosts.host(&notes).unwrap().control().is_some());
    }

    #[test]
    fn executable_packages_are_validated_and_retained_by_the_world_host() {
        let (world, reviewed, exec) = executable_package();
        let world_id = world.id();
        let (_, hosts) = WorldPackages::new()
            .with_package(WorldPackage::new(world, reviewed).with_exec(exec))
            .build()
            .unwrap();

        let installed = hosts.host(&world_id).unwrap().exec();
        assert_eq!(installed.specs().len(), 1);
        assert_eq!(installed.builds().len(), 1);
        assert_eq!(installed.handlers().len(), 1);
    }

    #[test]
    fn a_world_exec_declaration_without_its_application_package_is_refused() {
        let (world, reviewed, _) = executable_package();
        let err = WorldPackages::new()
            .with_package(WorldPackage::new(world, reviewed))
            .build()
            .unwrap_err();

        assert_eq!(
            err,
            RegistrationRefusal::InvalidExecPackage {
                world: WorldId::parse("com.example.jobs").unwrap(),
                reason: runtime::exec::PackageInvalid::SpecRegistrationMismatch,
            }
        );
    }

    /// A lifecycle that claims exactly one role, in its own namespace — the
    /// shape a release and a local copy of it present side by side.
    struct ClaimsAdministrator(&'static str);

    impl WorldLifecycle for ClaimsAdministrator {
        fn admission_evidence(
            &self,
            role: &str,
            parent_manifest_root: [u8; 32],
        ) -> anyhow::Result<Option<mechanics::authorization::WorldAssignmentEvidence>> {
            Ok((role == "administrator").then(|| {
                mechanics::authorization::WorldAssignmentEvidence {
                    world: self.0.to_owned(),
                    opaque_definition_ref: Vec::new(),
                    definition_digest: [0; 32],
                    parent_manifest_root,
                    assignments: Vec::new(),
                }
            }))
        }
    }

    /// The release and a local copy of it are two Worlds with the same roles.
    /// An unqualified selector is refused rather than guessed — install order
    /// is not policy — and the refusal names both claimants, because
    /// "ambiguous" alone reads as a broken role. Naming either World resolves
    /// it to that World's evidence.
    #[test]
    fn a_role_two_installed_worlds_claim_is_resolved_by_naming_one() {
        let release = package("com.example.issues", 1);
        let local = package("local.issues", 2);
        let (_, hosts) = WorldPackages::new()
            .with_package(
                WorldPackage::new(release.0, release.1)
                    .with_lifecycle(Arc::new(ClaimsAdministrator("com.example.issues"))),
            )
            .with_package(
                WorldPackage::new(local.0, local.1)
                    .with_lifecycle(Arc::new(ClaimsAdministrator("local.issues"))),
            )
            .build()
            .unwrap();

        let err = hosts
            .admission_evidence(None, "administrator", [0; 32])
            .unwrap_err()
            .to_string();
        assert!(err.contains("ambiguous"), "{err}");
        assert!(err.contains("com.example.issues"), "{err}");
        assert!(err.contains("local.issues"), "{err}");

        let named = hosts
            .admission_evidence(
                Some(&WorldId::parse("local.issues").unwrap()),
                "administrator",
                [0; 32],
            )
            .unwrap();
        assert_eq!(named.world, "local.issues");

        // A role only one of them claims still needs no name.
        let err = hosts
            .admission_evidence(None, "viewer", [0; 32])
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown role"), "{err}");
    }
}
