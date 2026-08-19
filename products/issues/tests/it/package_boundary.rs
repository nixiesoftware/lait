//! The semantic Issues package must remain independently movable.

use std::path::{Path, PathBuf};

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    fn walk(path: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(path).expect("read package source") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }

    let mut files = Vec::new();
    walk(root, &mut files);
    files
}

#[test]
fn semantic_package_has_no_shell_or_process_dependency() {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(package.join("Cargo.toml")).expect("package manifest");
    assert!(
        !manifest.lines().any(|line| {
            let line = line.trim_start();
            line.starts_with("lait =") || line.starts_with("lait-")
        }),
        "the Issues product must not depend back on the lait application shell"
    );

    let forbidden = [
        "crate::control",
        "crate::orbital",
        "crate::serve",
        "crate::host_client",
        "crate::mcp",
        "std::fs",
        "std::process",
        "tokio::",
        "interprocess::",
    ];
    for source in rust_sources(&package.join("src")) {
        let text = std::fs::read_to_string(&source).expect("product source");
        for symbol in forbidden {
            assert!(
                !text.contains(symbol),
                "shell/process symbol `{symbol}` leaked into {}",
                source.display()
            );
        }
    }
}

/// The reviewed implementation identity this build ships.
///
/// Pinned because it is not a hash of convenience: the founder activates this id
/// and every product transaction pins it, so a Space running an older build sees
/// a descriptor it never approved. Moving it is a real event and has to be a
/// deliberate one — which is what this test makes it.
#[test]
fn the_implementation_id_is_pinned_and_moving_it_is_deliberate() {
    let descriptor = lait_issues::IssuesWorld::implementation_descriptor();
    // Version 2, because this World declares signal schemas. A World that
    // declared nothing would still encode as version 1, byte-identical to what
    // shipped before sections existed.
    assert_eq!(descriptor.version(), 2);
    let id = descriptor.id().expect("canonical descriptor");
    // Moved deliberately a second time by the query and publication rebuild.
    // The package's declared FIND surface is what changed: the schemas, the
    // extractors and their bounds are all part of the descriptor, and
    // COMPATIBILITY.md's rule is that changing declared meaning changes the
    // descriptor identity. (The first move was durable Exec control adding
    // `issues::contract::verify_spec()` to the Spec section.)
    //
    // That same rebuild then moved it once more, within this release, by
    // declaring `relation_target_kind` — the reverse of the membership
    // posting the rebuild had already introduced. One release, one move: the
    // two changes ship together and no Space ever ran the intermediate
    // surface.
    //
    // Then twice more, still inside this release. `alias_project_ordinal`,
    // because a human reference is a project AND a number and the number is
    // only unique within the project -- the ordinal alone answered with one
    // row per project once ordinals became small enough for a person to
    // read. And `kind_project_state_live`, which a roll-up counts directly:
    // the coordinate beside it counts tombstoned rows too, so excluding them
    // had meant resolving every member, which is what put a ceiling on
    // collections that never needed one.
    //
    // Spaces on the previous implementation take the ordinary World-upgrade
    // path, exactly as they did then. What must NOT happen is this constant
    // being refreshed to whatever the build now prints: that turns the pin
    // into a mirror and the gate stops meaning anything.
    assert_eq!(
        id.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        "f342ffcb0cc4b1fe8cc272c1f8de1830b56b15395af96b6d819818026faa1199",
        "the Issues implementation id moved — see COMPATIBILITY.md before updating this"
    );
}
