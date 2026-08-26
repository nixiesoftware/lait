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
        // The tree keeps the id it declares, and its semantic package is
        // registered under that id — so a copy of a World this identity already
        // has installed would put two preferred packages under one id. The
        // Orbit's registry refuses that set as a whole
        // (`AmbiguousWorldDefault`), which is not a refusal of the local World
        // at all: it takes down every Space-plane read on every Orbit, and says
        // only that an id is ambiguous.
        //
        // So it is refused here, as one row, naming the release it collides
        // with. A local World runs beside a release it was copied from only
        // once its data is separable at the layer that stores data — the World
        // id is hashed into every Body id — and until then the honest thing is
        // to say so rather than to half-mount it.
        if let Some(declared) = replica::body::WorldId::parse(&manifest.id) {
            if packages.contains(&declared) {
                refused.push(format!(
                    "{}: {} declares World {declared}, which this identity already has installed \
— a local World cannot yet run beside the release it was copied from, because \
both would keep their data under the same id",
                    local.key,
                    local.dir.display()
                ));
                continue;
            }
        }
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
    let applicable: Vec<_> = manifest
        .runners
        .iter()
        .filter(|runner| runner.admits(std::env::consts::OS, std::env::consts::ARCH))
        .collect();
    if applicable.is_empty() {
        bail!("selected World {world} has no runner for this platform");
    }
    for runner in applicable {
        let release = Release::under(
            &root,
            manifest.id.clone(),
            manifest.version.clone(),
            admission.provenance,
            &runner.program,
            runner.args.clone(),
            runner.cwd.as_deref(),
        )?;
        let instance = Instance::launch(release)
            .with_context(|| format!("launch selected World {world} runner"))?;
        let remote = Arc::new(
            RemoteWorld::connect(instance)
                .with_context(|| format!("connect selected World {world} semantic service"))?,
        );
        let reviewed = remote.reviewed_implementation();
        if remote.descriptor().id.to_string() != world {
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
            // Built under the id the tree declares, so every check that asks
            // whether the tree is consistent with itself asks the right
            // question — a display surface commits its World id into a digest
            // its own runner computed. Re-keyed to the host's assignment
            // afterwards, which is what it is addressed by.
            let tree_id = replica::body::WorldId::parse(&world)
                .ok_or_else(|| anyhow!("World {world} declares an id that is not well formed"))?;
            let mut declared = client_package(tree_id, client, sealing)?;
            if admission.world.as_str() != world {
                declared = declared.registered_as(admission.world.clone());
            }
            if let Some(mount) = &admission.mount {
                declared = declared.mounted_at(mount.clone());
            }
            admitted.client = Some(declared);
        }
        admitted.packages.push(if runner.preferred {
            package
        } else {
            package.historical()
        });
    }
    Ok(admitted)
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
    /// The registration-time refusal cannot reach a row that is already
    /// recorded — one written before the check existed, or by hand. So the
    /// loader refuses it too, and refuses it as *one row*: the alternative is
    /// what this device actually did, which was to bring up an Orbit whose
    /// world registry would not build and answer every Space-plane read with
    /// `AmbiguousWorldDefault`.
    #[test]
    fn a_recorded_copy_of_an_installed_world_costs_only_its_own_row() {
        let identity = tempfile::tempdir().expect("an identity");
        let tree = tempfile::tempdir().expect("a copy of a World already installed");
        std::fs::write(
            tree.path().join("world.json"),
            br#"{"format":1,"id":"com.lait.issues","version":"0.0.0-local",
                 "mount":"issues","name":"Issues","runners":[]}"#,
        )
        .expect("a declaration");
        // Written straight to the registry: `register` refuses this now, and
        // the point of this test is the row that got in before it did.
        let root = crate::world::local::registrations_root(identity.path());
        std::fs::create_dir_all(&root).expect("the registry");
        std::fs::write(
            root.join("issues.json"),
            serde_json::json!({ "dir": tree.path(), "admitted": null }).to_string(),
        )
        .expect("a recorded registration");

        // What this device already serves, and must keep serving.
        let installed = crate::world::test::packages();
        let carried = crate::world::test::client_packages();
        let before: Vec<String> = carried
            .packages()
            .map(|package| package.mount().to_owned())
            .collect();

        let (packages, clients, refused) = load_local(identity.path(), installed, carried);

        assert_eq!(refused.len(), 1, "the collision is named: {refused:?}");
        assert!(
            refused[0].contains("com.lait.issues") && refused[0].contains("already has installed"),
            "and names the World it collides with: {:?}",
            refused
        );
        let issues = replica::body::WorldId::parse("com.lait.issues").expect("a World id");
        assert!(
            packages.contains(&issues),
            "the installed release is untouched"
        );
        let after: Vec<String> = clients
            .packages()
            .map(|package| package.mount().to_owned())
            .collect();
        assert_eq!(
            after, before,
            "and every World this device already served is still served"
        );
    }

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
