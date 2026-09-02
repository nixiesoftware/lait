//! Loading independently installed World processes from immutable releases.
//!
//! This module is deliberately product-blind. A `selected.json` record names
//! one release; its signed `world.json` names the applicable executables; and
//! each executable must describe the exact reviewed implementation it serves
//! before Runtime sees it. Directory order, process output, and executable
//! names are never authority.

use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use data_encoding::HEXLOWER;
use runtime::world::World as _;
use world_interface::manifest::WorldManifest;
use world_runner::{Instance, Provenance, Release};
use world_runner_wasm::WasmInstance;
use world_sdk::{remote_exec_package, RemoteClient, RemoteWorld};

use crate::orbital::{WorldPackage, WorldPackages};

pub struct Installation {
    pub packages: WorldPackages,
    pub clients: world_interface::WorldClientRegistry,
}

#[derive(Debug, Clone)]
pub struct Declaration {
    pub root: std::path::PathBuf,
    pub release: crate::update::world::InstalledBundle,
    pub manifest: WorldManifest,
}

/// Passively enumerate selected signed declarations without launching code.
pub fn declarations(worlds: &Path) -> Result<Vec<Declaration>> {
    let mut entries = match std::fs::read_dir(worlds) {
        Ok(entries) => entries
            .collect::<std::io::Result<Vec<_>>>()
            .with_context(|| format!("read World installations at {}", worlds.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read World installations at {}", worlds.display()));
        }
    };
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut declarations = Vec::new();
    for entry in entries {
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let world = entry.file_name().to_string_lossy().to_string();
        // A directory with nothing selected is not an installed World and is
        // not an error either. This used to fail the whole enumeration, and
        // the enumeration is what `installed::load` calls before a head or a
        // daemon will start — so one directory under this root with no
        // `selected.json` refused to bring the process up at all, naming a
        // World id nobody typed.
        //
        // Directories arrive here for reasons that have nothing to do with a
        // release: a World's own state now lives beside its releases, and
        // recording a link or a channel creates the parent. A single entry
        // that is not an installation must not be able to stop World loading,
        // however it got here.
        let Some(release) = crate::update::world::selected(worlds, &world) else {
            continue;
        };
        let Some(root) = crate::update::world::active_dir(worlds, &world) else {
            continue;
        };
        let manifest = WorldManifest::parse(
            &std::fs::read(root.join("world.json"))
                .with_context(|| format!("read {world} world.json"))?,
        )
        .map_err(anyhow::Error::msg)?;
        if manifest.id != world || manifest.version != release.version {
            bail!(
                "selected World {world} {} carries a declaration for {} {}",
                release.version,
                manifest.id,
                manifest.version
            );
        }
        declarations.push(Declaration {
            root,
            release,
            manifest,
        });
    }
    Ok(declarations)
}

/// Load every selected World release below `worlds`.
pub fn packages(worlds: &Path) -> Result<WorldPackages> {
    Ok(load(worlds)?.packages)
}

/// Load the semantic and client halves of every selected release, sharing one
/// supervised process per exact runner generation inside this host process.
/// What the host has decided about one tree, whatever the tree says about
/// itself.
///
/// A tree is checked for internal consistency — its runner must declare the id
/// and mount its `world.json` declares — and that check stays exactly as it
/// was. This is the separate question of what the *host* registers it as. For
/// a sealed release the two are the same and always will be: `MOUNT` is
/// published API and a World id is its address. For a tree somebody is working
/// on they are deliberately different, so a copy of a release can run beside
/// the release without either answering for the other.
struct Admission {
    /// The id this World answers to in the registry.
    world: replica::body::WorldId,
    /// Where it is mounted, when the host overrides the declaration.
    mount: Option<String>,
    /// What this host can honestly say about where the bytes came from.
    provenance: Provenance,
}

pub fn load(worlds: &Path) -> Result<Installation> {
    let mut packages = WorldPackages::new();
    let mut clients = world_interface::WorldClientRegistry::new();
    for declaration in declarations(worlds)? {
        let manifest = declaration.manifest;
        let world = manifest.id.clone();
        let digest = HEXLOWER
            .decode(declaration.release.digest.as_bytes())
            .map_err(|error| anyhow!("World {world} has an invalid release digest: {error}"))?;
        let digest: [u8; 32] = digest
            .try_into()
            .map_err(|_| anyhow!("World {world} release digest is not 32 bytes"))?;
        let admission = Admission {
            world: replica::body::WorldId::parse(&world)
                .ok_or_else(|| anyhow!("installed World id {world} is not well formed"))?,
            mount: None,
            provenance: Provenance::Sealed(digest),
        };
        let admitted = admit(&declaration.root, &manifest, &admission)?;
        for package in admitted.packages {
            packages = packages.with_package(package);
        }
        if let Some(client) = admitted.client {
            clients = clients
                .with_package(client)
                .map_err(|error| anyhow!(error))?;
        }
    }
    Ok(Installation { packages, clients })
}

/// Load the local Worlds registered on this device, beside the installed ones.
///
/// Each is admitted under the id and mount the host assigns it, and with
/// `Provenance::Local`, so nothing downstream — the registry, a routed call, an
/// MCP invocation, or the World's own process — can mistake it for a release.
///
/// A registration whose tree has gone, or whose tree cannot be loaded, is
/// skipped with its reason returned rather than failing the others. One
/// developer's broken working tree must not stop this device serving the Worlds
/// it has installed.
pub fn load_local(
    identity: &Path,
    mut packages: WorldPackages,
    mut clients: world_interface::WorldClientRegistry,
) -> (
    WorldPackages,
    world_interface::WorldClientRegistry,
    Vec<String>,
) {
    let mut refused = Vec::new();
    for local in crate::world::local::list(identity) {
        let Some(manifest) = local.manifest.clone() else {
            refused.push(format!(
                "{}: {} cannot be read",
                local.key,
                local.dir.display()
            ));
            continue;
        };
        let handle = local.key.trim_start_matches(crate::world::local::PREFIX);
        let admitted = crate::world::local::world_id_for(handle)
            .and_then(|world| Ok((world, crate::world::local::mount_for(handle)?)));
        let (world, mount) = match admitted {
            Ok(pair) => pair,
            Err(error) => {
                refused.push(format!("{}: {error:#}", local.key));
                continue;
            }
        };
        let admission = Admission {
            world,
            mount: Some(mount),
            provenance: Provenance::Local,
        };
        let admitted = match admit(&local.dir, &manifest, &admission) {
            Ok(admitted) => admitted,
            Err(error) => {
                refused.push(format!("{}: {error:#}", local.key));
                continue;
            }
        };
        // Merged only once the whole tree is up. A client package that
        // collides is this entry's failure and nobody else's.
        if let Some(client) = admitted.client {
            match clients.clone().with_package(client) {
                Ok(next) => clients = next,
                Err(error) => {
                    refused.push(format!("{}: {error}", local.key));
                    continue;
                }
            }
        }
        for package in admitted.packages {
            packages = packages.with_package(package);
        }
    }
    (packages, clients, refused)
}

/// What one tree contributed, before anything is merged.
///
/// Returned rather than merged in place so that a tree which fails to come up
/// takes nothing with it. The registries used to be handed *into* admission,
/// which meant a refusal consumed them — so one bad tree emptied the lot, and
/// an unsigned local World could stop this device serving every signed one.
/// Owning nothing is what makes skipping one entry possible at all.
struct Admitted {
    packages: Vec<WorldPackage>,
    client: Option<world_interface::WorldClientPackage>,
}

/// Bring one tree up and describe what it offers. Merging is the caller's.
fn admit(root: &Path, manifest: &WorldManifest, admission: &Admission) -> Result<Admitted> {
    let mut admitted = Admitted {
        packages: Vec::new(),
        client: None,
    };
    // The tree's own name for itself, used for every consistency check and
    // every message about it. What the host registers it as is `admission`.
    let world = manifest.id.clone();
    // The local namespace is reserved, and this is where the reservation is
    // kept rather than merely documented. A sealed World declaring a mount in
    // it would otherwise win the duplicate-mount refusal — `load` runs before
    // `load_local`, so the *working tree* was what got refused, the exact
    // inverse of what the reservation promises.
    if matches!(admission.provenance, Provenance::Sealed(_))
        && manifest
            .mount()
            .starts_with(crate::world::local::MOUNT_PREFIX)
    {
        bail!(
            "World {world} declares mount '{}', and '{}' is reserved for Worlds being worked on",
            manifest.mount(),
            crate::world::local::MOUNT_PREFIX
        );
    }
    // A World named by this host has no history here to migrate: its store
    // begins at the implementation it is being admitted with. A historical
    // runner would also be claiming a reviewed implementation minted for the
    // id its tree declares — a coordinate no Space has ever activated under
    // the name this one is being run as, which is a false entry in the record
    // rather than a useful one.
    let admits_history = !matches!(admission.provenance, Provenance::Local);
    // Two kinds, partitioned. A native runner applies iff this host's own
    // OS/arch admit it; a wasm runner applies on any host, because this build
    // carries the wasmtime host and runs the module in-process regardless of
    // its own architecture — which is why a wasm runner never keys on the
    // host arch.
    let native: Vec<_> = manifest
        .runners
        .iter()
        .filter(|runner| !runner.is_wasm())
        .filter(|runner| runner.admits(std::env::consts::OS, std::env::consts::ARCH))
        .filter(|runner| runner.preferred || admits_history)
        .collect();
    let wasm: Vec<_> = manifest
        .runners
        .iter()
        .filter(|runner| runner.is_wasm())
        .filter(|runner| runner.preferred || admits_history)
        .collect();
    // Prefer native when both apply: it is functionally complete, while the
    // wasm backend still drops a retained Find lease's detached callbacks
    // until a later slice. A wasm-only release selects wasm.
    let applicable = if native.is_empty() { wasm } else { native };
    if applicable.is_empty() {
        bail!("selected World {world} has no runner for this platform");
    }
    // The parse-time "exactly one preferred applicable" check keys on the real
    // host arch, so it never runs for a wasm-only release (nothing is
    // real-arch-applicable there). Enforce the invariant here, over the kind
    // actually selected, so a wasm release cannot smuggle in two preferred
    // runners the host would choose between by declaration order.
    let preferred = applicable.iter().filter(|runner| runner.preferred).count();
    if preferred != 1 {
        bail!(
            "selected World {world} must declare exactly one preferred runner for its kind, \
             found {preferred}"
        );
    }
    // What this host calls the World, which is what its runner is told and what
    // its data is keyed by. The same string as `world` for every installed
    // release; a name in the host's own namespace for a tree being worked on,
    // so it keeps its own Bodies rather than sharing the release's.
    let called = admission.world.as_str().to_owned();
    for runner in applicable {
        let release = Release::under(
            &root,
            called.clone(),
            manifest.version.clone(),
            admission.provenance,
            &runner.program,
            runner.args.clone(),
            runner.cwd.as_deref(),
        )?;
        let remote = Arc::new(if runner.is_wasm() {
            // The three facts a native runner reads from its environment
            // become the guest's instantiation, since a wasm module has no
            // environment: the host-chosen name, the release version, and the
            // provenance label (`local`, or the sealed digest).
            //
            // The wasm-runner ABI itself is proven end to end by
            // `world-runner-wasm`'s proof tests, and the selection that lands
            // here by the manifest/facts/staging unit tests. Admitting a real
            // wasm World all the way through `connect_runner` — the world-sdk
            // Descriptor, client declaration and mount — is proven once a
            // World answers that full surface in wasm, which arrives with the
            // real Issues wasm runner. Until then this branch is wired and
            // reviewed but not exercised end to end.
            let bytes = read_contained_wasm(&release)
                .with_context(|| format!("read selected World {world} wasm runner"))?;
            let wasm = WasmInstance::launch(
                &bytes,
                world_runner::wasm_abi::GuestInit {
                    world: called.clone(),
                    version: manifest.version.clone(),
                    release: admission.provenance.stated(),
                },
            )
            .with_context(|| format!("launch selected World {world} wasm runner"))?;
            RemoteWorld::connect_runner(Box::new(wasm))
                .with_context(|| format!("connect selected World {world} semantic service"))?
        } else {
            let instance = Instance::launch(release)
                .with_context(|| format!("launch selected World {world} runner"))?;
            RemoteWorld::connect(instance)
                .with_context(|| format!("connect selected World {world} semantic service"))?
        });
        let reviewed = remote.reviewed_implementation();
        // Against the name this host gave it, not the tree's. A runner that
        // takes its id from the host reports `called` back; one that compiled
        // its id in reports what the tree declares, and if those differ this is
        // a World that cannot be run under a name of the host's choosing. That
        // is a fact worth stating plainly here rather than discovering later as
        // a World whose data went to an id nobody addressed.
        let reported = remote.descriptor().id.to_string();
        if reported != called {
            if reported == world {
                bail!(
                    "World {world} does not support being run as '{called}': its runner \
reported the id its tree declares. A World takes its id from the host, so a copy \
of it can run beside the release it came from."
                );
            }
            bail!("World runner for {world} described another World");
        }
        if runner.preferred
            && manifest
                .implementation_version
                .is_some_and(|version| version != remote.descriptor().implementation_version.0)
        {
            bail!("World runner for {world} does not match its declared implementation version");
        }
        let exec = remote_exec_package(remote.clone())
            .with_context(|| format!("load selected World {world} Exec declaration"))?;
        let package = WorldPackage::new(remote.clone(), reviewed)
            .with_control(remote.clone())
            .with_exec(exec)
            .with_projector(remote.clone())
            .with_lifecycle(remote.clone())
            .with_release_version(manifest.version.clone());
        if runner.preferred {
            let client = Arc::new(
                RemoteClient::connect(remote)
                    .with_context(|| format!("load selected World {world} client declaration"))?,
            );
            if client.declaration().mount != manifest.mount() {
                bail!("World runner for {world} declared a different mount than world.json");
            }
            // Registered under the host's decision, not the tree's
            // declaration. The two checks just above compared the runner to
            // `world.json` — the tree against itself — which is a different
            // question and still the right one to ask.
            // The same fact the World's own process is told through
            // `LAIT_WORLD_RELEASE`, carried to the surface an agent reads —
            // and stated at construction, so no path can produce a package
            // that claims to be signed by forgetting to say otherwise.
            let sealing = match admission.provenance {
                Provenance::Sealed(_) => world_interface::Sealing::Sealed,
                Provenance::Local => world_interface::Sealing::Unsealed,
            };
            // Built under the name this host gave it, which is also the name
            // its own runner just reported and computed every digest under —
            // a display surface commits its World id into a contract digest,
            // and the runner is the one that computed it. There is no second
            // identity to re-key from any more: the tree declares what it is,
            // the host names it, and the runner serves under that name.
            let mut package = client_package(admission.world.clone(), client, sealing)?;
            if let Some(mount) = &admission.mount {
                package = package.mounted_at(mount.clone());
            }
            admitted.client = Some(package);
        }
        admitted.packages.push(if runner.preferred {
            package
        } else {
            package.historical()
        });
    }
    Ok(admitted)
}

/// Read a wasm runner's module out of its release tree, re-proving containment
/// exactly as `Instance::launch` does for a native executable: a tree that
/// changed on disk cannot turn a validated relative declaration into a path
/// that escapes the release root. `Release::under` proved the declaration was
/// a plain relative path; this proves the resolved path is still inside.
fn read_contained_wasm(release: &Release) -> Result<Vec<u8>> {
    let root = release
        .root
        .canonicalize()
        .with_context(|| format!("World release root {} is absent", release.root.display()))?;
    let module = root
        .join(&release.program)
        .canonicalize()
        .with_context(|| {
            format!(
                "World wasm runner {} is absent",
                root.join(&release.program).display()
            )
        })?;
    if !module.starts_with(&root) {
        bail!(
            "World wasm runner {} resolves outside release {}",
            module.display(),
            root.display()
        );
    }
    if !module.is_file() {
        bail!("World wasm runner {} is not a file", module.display());
    }
    std::fs::read(&module).with_context(|| format!("read World wasm runner {}", module.display()))
}

fn leaked(value: String) -> &'static str {
    Box::leak(value.into_boxed_str())
}

fn leaked_bytes(value: Vec<u8>) -> &'static [u8] {
    Box::leak(value.into_boxed_slice())
}

fn remote_decode(
    _call: &runtime::world::call::Call,
    _reply: runtime::world::call::Reply,
) -> Result<serde_json::Value, world_interface::Failure> {
    Err(world_interface::Failure::new(
        "remote World replies are decoded by their runner",
    ))
}

fn client_package(
    world: replica::body::WorldId,
    client: Arc<RemoteClient>,
    sealing: world_interface::Sealing,
) -> Result<world_interface::WorldClientPackage> {
    let declaration = client.declaration().clone();
    let adapter: Arc<dyn world_interface::ClientAdapter> = client.clone();
    let display_adapter: Arc<dyn world_interface::display::DisplayAdapter> = client;
    let tools = declaration
        .tools
        .into_iter()
        .map(|tool| {
            world_interface::McpTool::remote(
                leaked(tool.name),
                leaked(tool.description),
                tool.schema,
                Arc::clone(&adapter),
            )
        })
        .collect();
    let instructions = leaked(declaration.instructions);
    let without: Vec<&'static str> = declaration.without.into_iter().map(leaked).collect();
    let without = Box::leak(without.into_boxed_slice());
    let display = declaration.display;
    let routes: Vec<world_interface::Route> = display
        .routes
        .into_iter()
        .map(|(label, path)| world_interface::Route::new(leaked(label), leaked(path)))
        .collect();
    let routes = Box::leak(routes.into_boxed_slice());
    let mut package = world_interface::WorldClientPackage::new(
        world,
        leaked(declaration.mount),
        world_interface::AgentSurface::designed(tools, instructions, without),
        remote_decode,
        sealing,
    )?
    .with_client_adapter(adapter)
    .with_display(
        leaked(display.name),
        display.icon.map(leaked),
        display.entry_path.map(leaked),
    )?;
    if let Some(tagline) = display.tagline {
        package = package.with_tagline(leaked(tagline))?;
    }
    if let Some(accent) = display.accent {
        package = package.with_accent(accent)?;
    }
    package = package.with_artwork(
        display.mark.map(leaked_bytes),
        display.hero.map(leaked_bytes),
    )?;
    package = package.with_routes(routes)?;
    for descriptor in declaration.display_surfaces {
        package = package.with_display_surface(
            world_interface::display::DisplaySurface::remote(descriptor, display_adapter.clone()),
        )?;
    }
    Ok(package)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unsigned tree must not be able to stop this device serving signed
    /// ones.
    ///
    /// A local World that cannot come up — a tree with no runner for this
    /// platform, a binary that is not one, a directory somebody moved — costs
    /// its own row and nothing else. The first cut handed the registries into
    /// admission, so a refusal consumed them and returned empty ones: one bad
    /// local tree and the daemon, every head and every agent session served no
    /// World at all. That is a denial of service reachable by registering a
    /// directory.
    #[test]
    fn a_local_world_that_cannot_load_costs_only_its_own_row() {
        let identity = tempfile::tempdir().expect("an identity");
        let tree = tempfile::tempdir().expect("a tree that will not come up");
        // A declaration with no runner this platform admits: it parses, so it
        // registers, and it fails at admission — which is the case that used
        // to take everything with it.
        std::fs::write(
            tree.path().join("world.json"),
            br#"{"format":1,"id":"com.example.broken","version":"0.0.0",
                 "mount":"broken","name":"Broken","runners":[]}"#,
        )
        .expect("a declaration");
        crate::world::local::register(identity.path(), "broken", tree.path())
            .expect("it registers: the tree looks like a World");

        // Stand in for what a device already serves. The whole point is that
        // this survives; an empty registry would pass either way and prove
        // nothing.
        let carried = crate::world::test::client_packages();
        let before: Vec<String> = carried
            .packages()
            .map(|package| package.mount().to_owned())
            .collect();
        assert!(!before.is_empty(), "the fixture carries Worlds to lose");

        let (_packages, clients, refused) =
            load_local(identity.path(), WorldPackages::new(), carried);

        assert_eq!(refused.len(), 1, "the one that failed is named");
        assert!(
            refused[0].contains("local/broken"),
            "and named by its key: {:?}",
            refused
        );
        let after: Vec<String> = clients
            .packages()
            .map(|package| package.mount().to_owned())
            .collect();
        assert_eq!(
            after, before,
            "every World this device already served is still served"
        );
    }

    /// Recording a link or a channel for a World creates its state directory,
    /// and a head or a daemon calls `load` before it will start. One directory
    /// with nothing selected in it used to refuse to bring the process up at
    /// all, naming a World id nobody typed and recoverable only by deleting a
    /// directory by hand.
    #[test]
    fn a_directory_with_no_selected_release_is_skipped_rather_than_fatal() {
        let worlds = tempfile::tempdir().expect("an installations root");
        std::fs::create_dir_all(worlds.path().join("com.lait.issues"))
            .expect("the state directory a link creates");
        let found = declarations(worlds.path()).expect("enumeration survives it");
        assert!(
            found.is_empty(),
            "a directory that is not an installation is not an installed World"
        );
    }
}
