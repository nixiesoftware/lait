//! Assemble the first-party World trees, for a release and for the tree you are
//! working in.
//!
//! # Why this is a workspace binary and not a shell script
//!
//! It was a shell script under `.github/scripts/`, which said two untrue things
//! at once. It said this is a CI concern — and it is the only way to produce
//! what the Library's **+ Add local World** consumes, so it is the inner loop.
//! And it said the loop is a shell — so the contract was two environment
//! variables you found by reading it, it could not run on Windows, and nothing
//! tested it.
//!
//! Being one program matters more than being Rust. A tree somebody is working
//! on and a tree that gets published are assembled by the same code, so the
//! thing you looked at all afternoon cannot differ from the thing that ships in
//! a way neither of you noticed. That is the property; the language is how it
//! is kept.
//!
//! # Convenient on purpose
//!
//! Every argument has a default that is right for the loop:
//!
//! ```sh
//! cargo stage-worlds                    # host triple, debug, target/local-worlds
//! cargo stage-worlds --profile release --out target/distrib
//! ```
//!
//! What it emits is `worlds/<id>/<version>/` — `world.json`, `bin/<runner>`, the
//! World's pages and its artwork. That is the shape a release has, minus the
//! seal, which is exactly what an unsealed local World is.
//!
//! # A re-stage re-admits what pointed at it
//!
//! A local World registration is consent to *bytes*: `world.json` and every
//! runner it declares are digested when the tree is added, and a tree whose
//! bytes moved reads as changed until somebody confirms it again. Staging
//! rewrites every one of those bytes — and a `cargo clean` first deletes the
//! whole tree, so the registration names a directory that is gone until the
//! next stage brings it back. Both are the loop working as designed, and both
//! used to end with somebody hand-computing blake3 digests into
//! `world-local-v1/<handle>.json`. So after staging, every registration whose
//! tree lies under the freshly staged root is re-admitted here, with the same
//! digests `src/world/local.rs` would take. One pointing anywhere else is
//! listed and left alone: it is not this tree, and consent to it is not ours
//! to renew.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// One first-party World and where its parts come from.
struct World {
    /// Reverse-domain id, and the directory it stages under.
    id: &'static str,
    /// The crate whose version names the release.
    version_from: &'static str,
    /// `world.json` with `${VERSION}` and `${EXE}` still in it.
    template: &'static str,
    /// The runner binary, by its built name.
    runner: &'static str,
    /// The built web payload, copied to the tree's root.
    web: &'static str,
    /// Artwork, copied to `art/`. Empty when the World ships none.
    art: &'static [(&'static str, &'static str)],
}

/// The reviewed first-party set. A World is here because somebody decided it
/// ships with the host, which is a different question from what a person may
/// add locally — that is a directory they pick, and this program never sees it.
const WORLDS: &[World] = &[
    World {
        id: "com.lait.issues",
        version_from: "products/issues/Cargo.toml",
        template: "products/issues-runner/world.json.template",
        runner: "lait-world-issues",
        web: "products/issues-app/assets/web",
        art: &[
            ("products/issues-app/assets/mark.png", "art/mark.png"),
            ("products/issues-app/assets/hero.png", "art/hero.png"),
        ],
    },
    World {
        id: "com.lait.signage",
        version_from: "products/signage/Cargo.toml",
        template: "products/signage-runner/world.json.template",
        runner: "lait-world-signage",
        // Signage declares a primary web launch target, so its release has to
        // carry the bytes that target resolves to. Without them Open reaches a
        // head with no document to answer with.
        web: "products/signage-app/assets/web",
        art: &[],
    },
];

struct Args {
    target: String,
    profile: String,
    out: PathBuf,
    /// Where built binaries are, when they are not where the profile implies.
    artifacts: Option<PathBuf>,
    /// The identity whose local-World registrations are re-admitted. Defaults
    /// to the daemon's own config directory.
    identity: Option<PathBuf>,
}

fn main() -> Result<()> {
    let args = parse(std::env::args().skip(1).collect())?;
    let repo = repo_root()?;
    let artifacts = args.artifacts.clone().unwrap_or_else(|| {
        // `cargo build` lands in `target/<profile>`; `--target` adds a triple.
        // Defaulting to the plain path is right for the loop, because the loop
        // builds for the machine it is running on.
        repo.join("target").join(&args.profile)
    });
    let exe = if args.target.contains("-windows-") {
        ".exe"
    } else {
        ""
    };

    // Refused before anything is removed. This deletes a directory, and the
    // three that would take somebody's tree with them are the three worth
    // naming.
    let out = args.out.clone();
    if matches!(out.to_str(), Some("") | Some("/") | Some(".")) {
        bail!("refusing to stage into {}", out.display());
    }
    let worlds_root = out.join("worlds");
    if worlds_root.exists() {
        std::fs::remove_dir_all(&worlds_root)
            .with_context(|| format!("clear {}", worlds_root.display()))?;
    }

    for world in WORLDS {
        let version = version_of(&repo.join(world.version_from))?;
        let root = worlds_root.join(world.id).join(&version);
        std::fs::create_dir_all(root.join("bin")).context("create the World tree")?;

        let built = artifacts.join(format!("{}{exe}", world.runner));
        if !built.is_file() {
            bail!(
                "no built {}{exe} at {} — `cargo build{}` first",
                world.runner,
                built.display(),
                if args.profile == "release" {
                    " --release"
                } else {
                    ""
                }
            );
        }
        std::fs::copy(
            &built,
            root.join("bin").join(format!("{}{exe}", world.runner)),
        )
        .context("copy the World runner")?;

        let declared = std::fs::read_to_string(repo.join(world.template))
            .with_context(|| format!("read {}", world.template))?
            .replace("${VERSION}", &version)
            .replace("${EXE}", exe);
        std::fs::write(root.join("world.json"), declared).context("write world.json")?;

        copy_tree(&repo.join(world.web), &root).with_context(|| format!("copy {}", world.web))?;
        for (from, to) in world.art {
            let to = root.join(to);
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).context("create the artwork directory")?;
            }
            std::fs::copy(repo.join(from), &to).with_context(|| format!("copy {from}"))?;
        }
        println!("staged {} {version} at {}", world.id, root.display());
    }
    println!(
        "\nAdd one in Astrolabe: + Add local World, then pick a directory under {}",
        worlds_root.display()
    );

    match args.identity.or_else(default_identity) {
        Some(identity) => readmit(&identity, &worlds_root)?,
        None => println!("\nno identity directory could be resolved on this platform; registrations not re-admitted"),
    }
    Ok(())
}

/// Where this device's identity lives, the way the daemon resolves it:
/// `~/Library/Application Support/dev.nixi.lait` on macOS, `~/.config/lait` on
/// Linux — the product's config directory, and the parent of `world-local-v1`.
fn default_identity() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "nixi", "lait")
        .map(|dirs| dirs.config_dir().to_path_buf())
}

/// The registry of local Worlds, beside `world-bundles-v1`. The name is the
/// one `src/world/local.rs` writes under; a registration is `<handle>.json`.
fn registrations_root(identity: &Path) -> PathBuf {
    identity.join("world-local-v1")
}

/// Re-admit every registration whose tree lies under `worlds_root`.
///
/// A registration is rewritten in place, keeping every field but `admitted`,
/// and only when its tree exists again: one that points into the staged root
/// at a version this stage did not produce (the crate's version moved since it
/// was added) is named with the versions that are there, because repointing
/// it would be consenting to a tree on somebody's behalf.
fn readmit(identity: &Path, worlds_root: &Path) -> Result<()> {
    let root = registrations_root(identity);
    let Ok(entries) = std::fs::read_dir(&root) else {
        println!("\nno local World registrations at {}", root.display());
        return Ok(());
    };
    let worlds_root = std::fs::canonicalize(worlds_root)
        .with_context(|| format!("resolve {}", worlds_root.display()))?;
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    if files.is_empty() {
        println!("\nno local World registrations at {}", root.display());
        return Ok(());
    }
    println!("\nlocal World registrations under {}:", root.display());
    for file in files {
        let handle = file
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .unwrap_or_default();
        let bytes = std::fs::read(&file).with_context(|| format!("read {}", file.display()))?;
        let mut registration: serde_json::Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not a registration", file.display()))?;
        let Some(dir) = registration.get("dir").and_then(|dir| dir.as_str()) else {
            println!("  left alone {handle}: {} names no dir", file.display());
            continue;
        };
        let dir = PathBuf::from(dir);
        if !under(&dir, &worlds_root) {
            println!(
                "  left alone {handle}: {} is not under {}",
                dir.display(),
                worlds_root.display()
            );
            continue;
        }
        if !dir.is_dir() {
            println!(
                "  not re-admitted {handle}: {} is under the staged root but was not staged — \
                 staged: {}; add the tree again in Astrolabe (+ Add local World) or remove the \
                 registration",
                dir.display(),
                staged_beside(&dir, &worlds_root).join(", ")
            );
            continue;
        }
        let admitted = admitted(&dir).with_context(|| format!("digest {}", dir.display()))?;
        registration["admitted"] = admitted;
        let encoded = serde_json::to_vec_pretty(&registration)
            .context("encode the local World registration")?;
        std::fs::write(&file, encoded).with_context(|| format!("write {}", file.display()))?;
        println!("  re-admitted {handle} at {}", dir.display());
    }
    Ok(())
}

/// Whether `dir` lies under `root`, by canonical path when it exists and by
/// prefix when it does not — a registration whose tree is gone still has to be
/// recognised as one of ours, so it can be named rather than skipped.
fn under(dir: &Path, root: &Path) -> bool {
    // A tree that is gone still has an ancestor that is not, and on macOS the
    // temp root is a symlink: canonicalise the nearest existing ancestor and
    // put the missing tail back, so the two sides compare in the same form.
    let mut existing = dir.to_path_buf();
    let mut tail = Vec::new();
    loop {
        if existing.exists() {
            break;
        }
        let Some(name) = existing.file_name().map(std::ffi::OsStr::to_os_string) else {
            return dir.starts_with(root);
        };
        tail.push(name);
        if !existing.pop() {
            return dir.starts_with(root);
        }
    }
    let Ok(mut resolved) = std::fs::canonicalize(&existing) else {
        return dir.starts_with(root);
    };
    for name in tail.into_iter().rev() {
        resolved.push(name);
    }
    resolved.starts_with(root)
}

/// The version directories staged for the World `dir` names, for the message
/// that says a registration's version is not among them.
fn staged_beside(dir: &Path, worlds_root: &Path) -> Vec<String> {
    let Some(world_dir) = dir.parent() else {
        return vec!["absent".into()];
    };
    let mut found: Vec<String> = std::fs::read_dir(world_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| {
            entry.path().strip_prefix(worlds_root).map_or_else(
                |_| entry.path().display().to_string(),
                |relative| relative.display().to_string(),
            )
        })
        .collect();
    found.sort();
    if found.is_empty() {
        vec!["absent".into()]
    } else {
        found
    }
}

/// The digests `src/world/local.rs` takes when a tree is added — `world.json`
/// whole, and each runner program the manifest declares, by relative path,
/// where it can be read. Byte for byte the same computation, over the same
/// parser, so what this writes is what the daemon re-proves on every read.
fn admitted(dir: &Path) -> Result<serde_json::Value> {
    let declared = std::fs::read(dir.join("world.json")).context("read world.json")?;
    let manifest = world_interface::manifest::WorldManifest::parse(&declared)
        .map_err(|error| anyhow::anyhow!("read world.json: {error}"))?;
    let mut programs = std::collections::BTreeMap::new();
    for runner in &manifest.runners {
        if let Ok(bytes) = std::fs::read(dir.join(&runner.program)) {
            programs.insert(
                runner.program.clone(),
                blake3::hash(&bytes).to_hex().to_string(),
            );
        }
    }
    Ok(serde_json::json!({
        "declaration": blake3::hash(&declared).to_hex().to_string(),
        "programs": programs,
    }))
}

fn parse(args: Vec<String>) -> Result<Args> {
    let mut parsed = Args {
        target: host_triple()?,
        profile: "debug".into(),
        out: PathBuf::from("target/local-worlds"),
        artifacts: None,
        identity: None,
    };
    let mut rest = args.into_iter();
    while let Some(flag) = rest.next() {
        let mut value = || {
            rest.next()
                .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--target" => parsed.target = value()?,
            "--profile" => parsed.profile = value()?,
            "--out" => parsed.out = PathBuf::from(value()?),
            "--artifacts" => parsed.artifacts = Some(PathBuf::from(value()?)),
            "--identity" => parsed.identity = Some(PathBuf::from(value()?)),
            "--help" | "-h" => {
                println!(
                    "cargo stage-worlds [--target <triple>] [--profile debug|release] \\\n\
                     \x20                   [--out <dir>] [--artifacts <dir>] [--identity <dir>]"
                );
                std::process::exit(0);
            }
            other => bail!("unknown flag {other}"),
        }
    }
    Ok(parsed)
}

/// The repository root, from this crate's manifest rather than the working
/// directory — so it runs the same from anywhere in the tree.
fn repo_root() -> Result<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("locate the repository root")
}

/// Ask the toolchain rather than mapping platform names here. A wrong guess
/// stages a tree whose runner cannot run, and says nothing until it is opened.
fn host_triple() -> Result<String> {
    let out = std::process::Command::new("rustc")
        .arg("-vV")
        .output()
        .context("run rustc -vV")?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .context("rustc -vV did not report a host triple")
}

/// The first `version = "…"` in a manifest, which is the package's own.
fn version_of(manifest: &Path) -> Result<String> {
    let text = std::fs::read_to_string(manifest)
        .with_context(|| format!("read {}", manifest.display()))?;
    text.lines()
        .find_map(|line| {
            let rest = line.trim().strip_prefix("version")?.trim_start();
            let rest = rest.strip_prefix('=')?.trim();
            rest.strip_prefix('"')?.split('"').next().map(str::to_owned)
        })
        .filter(|version| !version.is_empty())
        .with_context(|| format!("{} declares no version", manifest.display()))
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    for entry in std::fs::read_dir(from).with_context(|| format!("read {}", from.display()))? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_is_the_packages_own_and_not_a_dependencys() {
        let dir = tempfile::tempdir().expect("a directory");
        let manifest = dir.path().join("Cargo.toml");
        std::fs::write(
            &manifest,
            "[package]\nname = \"thing\"\nversion = \"0.9.3\"\n\n\
             [dependencies]\nother = { version = \"1.2.3\" }\n",
        )
        .expect("a manifest");
        assert_eq!(version_of(&manifest).expect("a version"), "0.9.3");
    }

    #[test]
    fn a_manifest_with_no_version_is_named_rather_than_guessed() {
        let dir = tempfile::tempdir().expect("a directory");
        let manifest = dir.path().join("Cargo.toml");
        std::fs::write(&manifest, "[package]\nname = \"thing\"\n").expect("a manifest");
        assert!(version_of(&manifest).is_err());
    }

    /// The three that would take somebody's tree with them.
    #[test]
    fn an_unsafe_output_is_refused_before_anything_is_removed() {
        for unsafe_out in ["", "/", "."] {
            let args = parse(vec!["--out".into(), unsafe_out.into()]);
            let out = args.expect("parses").out;
            assert!(
                matches!(out.to_str(), Some("") | Some("/") | Some(".")),
                "the guard in main covers {unsafe_out:?}"
            );
        }
    }

    #[test]
    fn defaults_are_the_loop_and_need_no_arguments() {
        let args = parse(Vec::new()).expect("no arguments is valid");
        assert_eq!(args.profile, "debug");
        assert_eq!(args.out, PathBuf::from("target/local-worlds"));
        assert!(!args.target.is_empty(), "the host triple is asked for");
    }

    #[test]
    fn an_unknown_flag_is_refused_rather_than_ignored() {
        assert!(parse(vec!["--wat".into()]).is_err());
        assert!(
            parse(vec!["--out".into()]).is_err(),
            "a flag needs its value"
        );
    }

    /// A staged tree: a declaration naming one runner, and the runner's bytes.
    fn staged_tree(dir: &Path, program: &str, runner_bytes: &[u8]) -> String {
        std::fs::create_dir_all(dir.join("bin")).expect("a World tree");
        let declaration = format!(
            r#"{{"format":1,"id":"com.example.atlas","version":"0.0.0-local","mount":"atlas",
                 "name":"Atlas","runners":[{{"preferred":true,"program":"{program}"}}]}}"#
        );
        std::fs::write(dir.join("world.json"), &declaration).expect("a declaration");
        std::fs::write(dir.join(program), runner_bytes).expect("a runner");
        declaration
    }

    /// The digests are the ones `src/world/local.rs` takes: blake3 hex of the
    /// declaration's bytes, and of each declared program's bytes by its
    /// relative path. Computed here against known bytes rather than against
    /// that module, because the point is that a registration rewritten by
    /// this tool reads as unchanged to the daemon.
    #[test]
    fn admitted_digests_the_declaration_and_each_declared_program() {
        let dir = tempfile::tempdir().expect("a tree");
        let declaration = staged_tree(dir.path(), "bin/atlas", b"the runner bytes");

        let admitted = admitted(dir.path()).expect("a valid tree digests");
        assert_eq!(
            admitted["declaration"],
            blake3::hash(declaration.as_bytes()).to_hex().to_string()
        );
        assert_eq!(
            admitted["programs"]["bin/atlas"],
            blake3::hash(b"the runner bytes").to_hex().to_string()
        );
        assert_eq!(
            admitted["programs"].as_object().map(serde_json::Map::len),
            Some(1)
        );
    }

    /// A program the manifest declares but the tree does not carry is left out
    /// rather than failing the digest — the daemon pins what it can read.
    #[test]
    fn a_declared_program_that_is_absent_is_not_pinned() {
        let dir = tempfile::tempdir().expect("a tree");
        staged_tree(dir.path(), "bin/atlas", b"bytes");
        std::fs::remove_file(dir.path().join("bin/atlas")).expect("remove the runner");

        let admitted = admitted(dir.path()).expect("digests without the program");
        assert!(admitted["programs"]
            .as_object()
            .is_some_and(serde_json::Map::is_empty));
    }

    #[test]
    fn a_tree_with_no_declaration_is_refused() {
        let dir = tempfile::tempdir().expect("a tree");
        assert!(admitted(dir.path()).is_err());
    }

    /// The whole pass: a registration under the staged root is rewritten with
    /// fresh digests and its other fields kept; one elsewhere is untouched; one
    /// whose version directory the stage did not produce is untouched too.
    #[test]
    fn readmit_rewrites_registrations_under_the_staged_root_and_no_others() {
        let identity = tempfile::tempdir().expect("an identity");
        let out = tempfile::tempdir().expect("an output root");
        let worlds_root = out.path().join("worlds");
        let staged = worlds_root.join("com.example.atlas").join("0.1.0");
        staged_tree(&staged, "bin/atlas", b"new bytes");
        let elsewhere = tempfile::tempdir().expect("another tree");
        staged_tree(elsewhere.path(), "bin/atlas", b"other bytes");

        let root = registrations_root(identity.path());
        std::fs::create_dir_all(&root).expect("a registry");
        let stale = serde_json::json!({
            "dir": staged,
            "admitted": {"declaration": "0".repeat(64), "programs": {"bin/atlas": "0".repeat(64)}},
            "note": "kept",
        });
        std::fs::write(root.join("atlas.json"), stale.to_string()).expect("a registration");
        let other = serde_json::json!({
            "dir": elsewhere.path(),
            "admitted": {"declaration": "1".repeat(64), "programs": {}},
        });
        std::fs::write(root.join("other.json"), other.to_string()).expect("a registration");
        let gone = serde_json::json!({
            "dir": worlds_root.join("com.example.atlas").join("0.0.9"),
            "admitted": {"declaration": "2".repeat(64), "programs": {}},
        });
        std::fs::write(root.join("gone.json"), gone.to_string()).expect("a registration");

        readmit(identity.path(), &worlds_root).expect("the pass runs");

        let rewritten: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("atlas.json")).expect("still there"))
                .expect("still JSON");
        assert_eq!(rewritten["note"], "kept", "other fields are preserved");
        assert_eq!(rewritten["dir"], serde_json::json!(staged));
        assert_eq!(
            rewritten["admitted"],
            admitted(&staged).expect("digests"),
            "the digests are the staged tree's"
        );
        let untouched: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("other.json")).expect("still there"))
                .expect("still JSON");
        assert_eq!(untouched, other, "a registration elsewhere is left alone");
        let untouched: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("gone.json")).expect("still there"))
                .expect("still JSON");
        assert_eq!(
            untouched, gone,
            "a version the stage did not produce is not repointed"
        );
    }

    #[test]
    fn readmit_with_no_registry_is_not_a_failure() {
        let identity = tempfile::tempdir().expect("an identity");
        let out = tempfile::tempdir().expect("an output root");
        readmit(identity.path(), out.path()).expect("nothing to do is fine");
    }

    /// Every World here has to be assemblable: a template that exists, a
    /// version that parses, and pages to copy. A World added to the list with a
    /// path that does not exist fails at staging time, on somebody's machine,
    /// with a message about a file rather than about the list.
    #[test]
    fn every_first_party_world_names_paths_that_exist() {
        let repo = repo_root().expect("a repository root");
        for world in WORLDS {
            assert!(
                repo.join(world.template).is_file(),
                "{}: template {} is missing",
                world.id,
                world.template
            );
            assert!(
                version_of(&repo.join(world.version_from)).is_ok(),
                "{}: {} declares no version",
                world.id,
                world.version_from
            );
            for (from, _) in world.art {
                assert!(repo.join(from).is_file(), "{}: {from} is missing", world.id);
            }
        }
    }
}
