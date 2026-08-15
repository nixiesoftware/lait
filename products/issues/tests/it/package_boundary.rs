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
    // Moved deliberately when durable Exec control landed: the package now
    // declares `issues::contract::verify_spec()` in its Spec section, and
    // COMPATIBILITY.md's rule is that changing declared Spec meaning changes
    // the descriptor identity. Spaces on the previous implementation take the
    // ordinary World-upgrade path.
    assert_eq!(
        id.iter().map(|b| format!("{b:02x}")).collect::<String>(),
        "e405d9b52ba7a3aca4a1db28f802c4566890338ea2412fa0a70e832e80d04b56",
        "the Issues implementation id moved — see COMPATIBILITY.md before updating this"
    );
}
