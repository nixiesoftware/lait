//! Product-side bridges into the Worlds hosted by one active Space.
//!
//! [`runtime::Registry`] owns the immutable semantic implementations used
//! by a Station. This module owns the application-side half of that boundary:
//! a compile-time [`WorldPackage`] for each product, and one [`WorldHost`] per
//! package inside an active Space. A package carries the reviewed semantic
//! implementation plus its optional product-neutral call handler; orbital code
//! never needs to name the product behind either one.
//!
//! A [`WorldRouter`] belongs to one Space bridge. It is not a process,
//! does not own a listener, and has no autonomous background loop.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use mechanics::ids::DeviceId;
use replica::ids::WorldId;
use runtime::lifecycle::Failure;
use runtime::registry::Refusal as RegistrationRefusal;
use runtime::{LocalIdentity, Registry, RuntimeBuilder, Session, Station, World};

pub use ::world_bridge::{
    CallFailure, CallFailureCode, WorldCall, WorldCallAccess, WorldCallContext, WorldCallHandler,
    WorldNudge, WorldReply,
};

/// One product package available to the application build.
#[derive(Clone)]
pub struct WorldPackage {
    world: WorldId,
    implementation: Arc<dyn World>,
    reviewed_implementation: [u8; 32],
    control: Option<Arc<dyn WorldCallHandler>>,
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
        }
    }

    pub fn with_control(mut self, control: Arc<dyn WorldCallHandler>) -> Self {
        self.control = Some(control);
        self
    }

    pub fn world_id(&self) -> &WorldId {
        &self.world
    }
}

/// Compile-time composition of the Worlds bundled by one application build.
///
/// The package set is cloned down the LaitDaemon → Station placement →
/// StationHost call stack. Each Space freezes its own Runtime registry and
/// bridge objects from the same reviewed set.
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
    /// the existing [`RuntimeBuilder`] validation when [`Self::build`] freezes
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
        self.packages.iter().map(WorldPackage::world_id)
    }

    pub fn contains(&self, world: &WorldId) -> bool {
        self.packages
            .iter()
            .any(|package| package.world_id() == world)
    }

    pub fn accepts_call(&self, call: &WorldCall) -> bool {
        self.call_access(call).is_ok()
    }

    pub fn call_access(&self, call: &WorldCall) -> Result<WorldCallAccess, CallFailure> {
        let control = self
            .packages
            .iter()
            .find(|package| package.world_id() == call.world())
            .and_then(|package| package.control.as_deref())
            .ok_or_else(|| {
                CallFailure::new(
                    CallFailureCode::UnsupportedOperation,
                    format!("World '{}' has no application call handler", call.world()),
                )
            })?;
        control.access(call)
    }

    /// Freeze the semantic registry and create one application bridge per
    /// registered World.
    pub fn build(&self) -> Result<(Registry, WorldRouter), RegistrationRefusal> {
        let mut runtime = RuntimeBuilder::new();
        let mut bridges = Vec::with_capacity(self.packages.len());
        for package in &self.packages {
            bridges.push((
                package.world.clone(),
                package.reviewed_implementation,
                package.control.clone(),
            ));
            runtime = runtime.register(package.implementation.clone());
        }
        let registry = runtime.build()?;
        Ok((
            registry,
            WorldRouter::new(
                bridges
                    .into_iter()
                    .map(|(world, reviewed, control)| {
                        (world.clone(), WorldHost::new(world, reviewed, control))
                    })
                    .collect(),
            ),
        ))
    }
}

/// The sole product-side entrance to one World in one active Space.
///
/// Primary and sponsored-agent Sessions cannot be reused across Worlds because
/// each bridge owns only the Sessions docked to its own [`WorldId`].
pub struct WorldHost {
    world: WorldId,
    reviewed_implementation: [u8; 32],
    control: Option<Arc<dyn WorldCallHandler>>,
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
        control: Option<Arc<dyn WorldCallHandler>>,
    ) -> Self {
        Self {
            world,
            reviewed_implementation,
            control,
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

    pub fn control(&self) -> Option<&dyn WorldCallHandler> {
        self.control.as_deref()
    }

    /// Ensure the Space's primary identity has a Session for this World.
    ///
    /// Docking remains lazy: an unadmitted joiner can keep its StationHost
    /// active to drive Contact before Mechanics grants it standing.
    pub fn ensure_primary(
        &self,
        station: &Station,
        identity: &LocalIdentity,
    ) -> Result<(), Failure> {
        let mut session = self.primary_session.lock().expect("World Session lock");
        if session.is_none() {
            *session = Some(station.dock(&self.world, identity)?);
        }
        Ok(())
    }

    pub fn with_primary<R>(&self, f: impl FnOnce(&Session) -> R) -> Option<R> {
        let session = self.primary_session.lock().expect("World Session lock");
        session.as_ref().map(f)
    }

    /// Dock or reuse a sponsored local agent's Session, then run `f` with it.
    pub fn with_agent<R>(
        &self,
        station: &Station,
        identity: &LocalIdentity,
        f: impl FnOnce(&Session) -> R,
    ) -> Result<R, Failure> {
        let mut sessions = self
            .agent_sessions
            .lock()
            .expect("agent World Sessions lock");
        let device = identity.device().clone();
        if !sessions.contains_key(&device) {
            sessions.insert(device.clone(), station.dock(&self.world, identity)?);
        }
        Ok(f(sessions.get(&device).expect("agent Session just docked")))
    }
}

/// The World bridges enabled inside one active StationHost.
pub struct WorldRouter {
    bridges: BTreeMap<WorldId, WorldHost>,
}

impl std::fmt::Debug for WorldRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorldRouter")
            .field("worlds", &self.bridges.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl WorldRouter {
    fn new(bridges: BTreeMap<WorldId, WorldHost>) -> Self {
        Self { bridges }
    }

    pub fn world_ids(&self) -> impl Iterator<Item = &WorldId> {
        self.bridges.keys()
    }

    pub fn contains(&self, world: &WorldId) -> bool {
        self.bridges.contains_key(world)
    }

    pub fn bridge(&self, world: &WorldId) -> Option<&WorldHost> {
        self.bridges.get(world)
    }

    pub fn reviewed_implementations(&self) -> impl Iterator<Item = (&WorldId, &[u8; 32])> {
        self.bridges
            .iter()
            .map(|(world, bridge)| (world, bridge.reviewed_implementation()))
    }

    pub fn ensure_primary(
        &self,
        station: &Station,
        world: &WorldId,
        identity: &LocalIdentity,
    ) -> Result<(), Failure> {
        self.bridge(world)
            .ok_or_else(|| Failure::UnknownWorld(world.clone()))?
            .ensure_primary(station, identity)
    }

    pub fn with_primary<R>(&self, world: &WorldId, f: impl FnOnce(&Session) -> R) -> Option<R> {
        self.bridge(world)?.with_primary(f)
    }

    /// Run `f` with any docked primary Session.
    ///
    /// Station observations and authority doorbells are shared across Worlds,
    /// so Space-level adapters need exactly one Session to publish that plane.
    pub fn with_any_primary<R>(&self, f: impl FnOnce(&Session) -> R) -> Option<R> {
        for bridge in self.bridges.values() {
            let session = bridge.primary_session.lock().expect("World Session lock");
            if let Some(session) = session.as_ref() {
                return Some(f(session));
            }
        }
        None
    }

    pub fn with_agent<R>(
        &self,
        station: &Station,
        world: &WorldId,
        identity: &LocalIdentity,
        f: impl FnOnce(&Session) -> R,
    ) -> Result<R, Failure> {
        self.bridge(world)
            .ok_or_else(|| Failure::UnknownWorld(world.clone()))?
            .with_agent(station, identity, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use replica::{Schema, WorldId};
    use runtime::{Context, Effect, Intent, Projection, Query};

    struct NoopWorld {
        id: WorldId,
        schemas: Vec<Schema>,
    }

    struct ProjectControl;

    impl WorldCallHandler for ProjectControl {
        fn access(&self, call: &WorldCall) -> Result<WorldCallAccess, CallFailure> {
            if call.operation() == "projects.control" && call.version() == 1 {
                Ok(WorldCallAccess::Query)
            } else {
                Err(CallFailure::new(
                    CallFailureCode::UnsupportedOperation,
                    "unsupported project call",
                ))
            }
        }

        fn call(&self, call: &WorldCall, _context: &WorldCallContext<'_>) -> WorldReply {
            WorldReply::ok(call, b"{}".to_vec())
        }
    }

    impl World for NoopWorld {
        fn id(&self) -> WorldId {
            self.id.clone()
        }

        fn schemas(&self) -> &[Schema] {
            &self.schemas
        }

        fn submit(
            &self,
            _ctx: &mut Context<'_>,
            _intent: Intent,
        ) -> Result<Effect, runtime::Rejection> {
            unreachable!("registry tests never execute the World")
        }

        fn query(
            &self,
            _ctx: &Context<'_>,
            _query: Query,
        ) -> Result<Projection, runtime::Rejection> {
            unreachable!("registry tests never execute the World")
        }
    }

    fn package(id: &str, marker: u8) -> (Arc<dyn World>, [u8; 32]) {
        let id = WorldId::parse(id).expect("test World id");
        let schemas = Vec::new();
        (Arc::new(NoopWorld { id, schemas }), [marker; 32])
    }

    #[test]
    fn one_space_has_one_bridge_per_registered_world() {
        let a = package("com.example.files", 1);
        let b = package("com.example.notes", 2);
        let (registry, bridges) = WorldPackages::new()
            .with_package(WorldPackage::new(a.0, a.1))
            .with_package(WorldPackage::new(b.0, b.1))
            .build()
            .unwrap();

        let ids: Vec<_> = bridges.world_ids().map(WorldId::as_str).collect();
        assert_eq!(ids, ["com.example.files", "com.example.notes"]);
        assert_eq!(registry.len(), 2);
        assert_eq!(
            bridges
                .reviewed_implementations()
                .map(|(id, implementation)| (id.as_str(), implementation[0]))
                .collect::<Vec<_>>(),
            [("com.example.files", 1), ("com.example.notes", 2)]
        );
        assert_ne!(
            bridges
                .bridge(&WorldId::parse("com.example.files").unwrap())
                .unwrap() as *const WorldHost,
            bridges
                .bridge(&WorldId::parse("com.example.notes").unwrap())
                .unwrap() as *const WorldHost,
            "each World must have a distinct bridge object"
        );
    }

    #[test]
    fn duplicate_worlds_still_fail_through_the_runtime_registry_contract() {
        let a = package("com.example.files", 1);
        let b = package("com.example.files", 2);
        let err = WorldPackages::new()
            .with_package(WorldPackage::new(a.0, a.1))
            .with_package(WorldPackage::new(b.0, b.1))
            .build()
            .unwrap_err();
        assert_eq!(
            err,
            RegistrationRefusal::DuplicateWorld(
                WorldId::parse("com.example.files").expect("test World id")
            )
        );
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

        let call = WorldCall::new(notes.clone(), "projects.control", 1, Vec::new()).unwrap();
        assert!(packages.accepts_call(&call));
        let (_, bridges) = packages.build().unwrap();
        assert!(bridges.bridge(&files).unwrap().control().is_none());
        assert!(bridges.bridge(&notes).unwrap().control().is_some());
    }
}
