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
use world_runner::{Instance, Release};
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
pub fn load(worlds: &Path) -> Result<Installation> {
    let mut packages = WorldPackages::new();
    let mut clients = world_interface::WorldClientRegistry::new();
    for declaration in declarations(worlds)? {
        let root = declaration.root;
        let selected = declaration.release;
        let manifest = declaration.manifest;
        let world = manifest.id.clone();
        let digest = HEXLOWER
            .decode(selected.digest.as_bytes())
            .map_err(|error| anyhow!("World {world} has an invalid release digest: {error}"))?;
        let digest: [u8; 32] = digest
            .try_into()
            .map_err(|_| anyhow!("World {world} release digest is not 32 bytes"))?;

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
                digest,
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
                bail!(
                    "World runner for {world} does not match its declared implementation version"
                );
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
                let client =
                    Arc::new(RemoteClient::connect(remote).with_context(|| {
                        format!("load selected World {world} client declaration")
                    })?);
                if client.declaration().mount != manifest.mount() {
                    bail!("World runner for {world} declared a different mount than world.json");
                }
                clients = clients
                    .with_package(client_package(
                        replica::body::WorldId::parse(&world)
                            .ok_or_else(|| anyhow!("installed World id became invalid"))?,
                        client,
                    )?)
                    .map_err(|error| anyhow!(error))?;
            }
            packages = packages.with_package(if runner.preferred {
                package
            } else {
                package.historical()
            });
        }
    }
    Ok(Installation { packages, clients })
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
