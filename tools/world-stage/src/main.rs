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
    Ok(())
}

fn parse(args: Vec<String>) -> Result<Args> {
    let mut parsed = Args {
        target: host_triple()?,
        profile: "debug".into(),
        out: PathBuf::from("target/local-worlds"),
        artifacts: None,
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
            "--help" | "-h" => {
                println!(
                    "cargo stage-worlds [--target <triple>] [--profile debug|release] \\\n\
                     \x20                   [--out <dir>] [--artifacts <dir>]"
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
