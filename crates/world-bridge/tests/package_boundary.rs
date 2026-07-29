//! The generic application-call contract must remain movable with the substrate.

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
fn contract_has_no_shell_product_or_process_dependency() {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = std::fs::read_to_string(package.join("Cargo.toml")).expect("package manifest");
    for dependency in ["lait =", "lait-issues", "interprocess", "tokio"] {
        assert!(
            !manifest.contains(dependency),
            "generic World bridge depends on `{dependency}`"
        );
    }

    let forbidden = [
        "crate::control",
        "crate::orbital",
        "crate::serve",
        "issues::",
        "std::fs",
        "std::process",
        "tokio::",
        "interprocess::",
    ];
    for source in rust_sources(&package.join("src")) {
        let text = std::fs::read_to_string(&source).expect("bridge source");
        for symbol in forbidden {
            assert!(
                !text.contains(symbol),
                "shell/product/process symbol `{symbol}` leaked into {}",
                source.display()
            );
        }
    }
}
