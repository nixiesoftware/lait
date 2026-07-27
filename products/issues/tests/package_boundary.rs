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
        "crate::cli",
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
