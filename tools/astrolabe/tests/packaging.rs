//! What the installer must and must not do, asserted against the script itself.
//!
//! These are release criteria from CLIENT-13, and they are checked here rather
//! than by a human reading the `.nsi` because "no Flutter runtime in the
//! installer" is exactly the kind of thing that stays true for months and then
//! quietly stops being true in a hurry.
//!
//! Reading the script is a weaker check than installing it. It is also one that
//! runs on every push, on every platform, in under a millisecond — and it
//! catches the regressions that actually happen, which are edits to this file.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `<root>/tools/astrolabe`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate sits two levels below the workspace root")
        .to_path_buf()
}

fn installer() -> String {
    let path = repo_root().join("packaging/windows/astrolabe.nsi");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// The script with its comments removed.
///
/// The header deliberately *names* the technologies this installer does not
/// carry, because "no Flutter runtime" is worth stating where somebody editing
/// the file will read it. A check that scanned the prose would therefore fail on
/// the documentation of the very property it is checking — so it scans what the
/// installer actually does.
fn directives() -> String {
    installer()
        .lines()
        .map(|line| line.split_once(';').map_or(line, |(code, _)| code))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The pair ships together. An installer that placed only the client would
/// produce a machine where `sidecar::resolve` finds nothing, and the failure
/// would arrive at first launch rather than at install.
#[test]
fn the_installer_places_both_binaries_in_one_directory() {
    let script = installer();
    assert!(
        script.contains(r#"File "${STAGE}\astrolabe.exe""#),
        "the installer does not install astrolabe.exe"
    );
    assert!(
        script.contains(r#"File "${STAGE}\lait.exe""#),
        "the installer does not install the lait.exe sidecar"
    );
    // Both are written after one `SetOutPath "$INSTDIR"`, which is what makes
    // them siblings — the property `sidecar::resolve` depends on. Matched on
    // the whole directive, so the header comment naming the same file cannot
    // stand in for the line that installs it.
    let script = directives();
    let out = script
        .find(r#"SetOutPath "$INSTDIR""#)
        .expect("an out path");
    let client = script
        .find(r#"File "${STAGE}\astrolabe.exe""#)
        .expect("the client");
    let daemon = script
        .find(r#"File "${STAGE}\lait.exe""#)
        .expect("the sidecar");
    assert!(
        out < client && out < daemon,
        "the binaries are not installed into the same directory"
    );
}

/// Nothing from the old stack survives in the installer. Each of these names a
/// specific dead technology from a superseded revision, and finding any of them
/// would mean a shell nobody meant to ship had come back.
#[test]
fn the_installer_carries_nothing_from_the_retired_stacks() {
    let script = directives().to_ascii_lowercase();
    for corpse in [
        "flutter",
        "flutter_windows.dll",
        "webview2",
        "dart",
        "flutter_rust_bridge",
        "tauri",
        "\\data\\",
    ] {
        assert!(
            !script.contains(corpse),
            "the installer references '{corpse}', which no longer exists in this design"
        );
    }
}

/// An invite should be a link rather than a blob copied between two places,
/// and every hop is somewhere it can be truncated or mangled.
#[test]
fn the_installer_registers_the_lait_url_scheme() {
    let script = directives();
    assert!(
        script.contains(r#"WriteRegStr HKCU "Software\Classes\lait" "URL Protocol""#),
        "the lait: scheme is not registered, so an invite cannot be a link"
    );
    assert!(
        script.contains(r#"shell\open\command"#),
        "the scheme is registered with no command to open"
    );
    assert!(
        script.contains(r#"DeleteRegKey HKCU "Software\Classes\lait""#),
        "uninstalling leaves a scheme handler pointing at a deleted executable"
    );
}

/// Uninstalling removes the program. Removal and data deletion are separate
/// operations everywhere else in this design, and an uninstaller is the worst
/// possible place to conflate them: the person is not being asked, and what
/// would go is their Spaces.
#[test]
fn uninstalling_removes_the_program_and_never_the_persons_data() {
    let script = directives();
    let uninstall = script
        .split("Section \"Uninstall\"")
        .nth(1)
        .expect("an uninstall section");

    for kept in ["$APPDATA", "$LOCALAPPDATA\\lait", "$DOCUMENTS", "$PROFILE"] {
        assert!(
            !uninstall.contains(kept),
            "the uninstaller deletes {kept}, which holds the person's data"
        );
    }
    assert!(
        !uninstall.contains("RMDir /r"),
        "the uninstaller removes a directory tree recursively, which is how a \
         store gets destroyed by an uninstall nobody read"
    );
}

/// A per-user install needs no elevation, and elevation is the one prompt
/// people have been trained to click through.
#[test]
fn the_installer_asks_for_no_elevation() {
    assert!(
        directives().contains("RequestExecutionLevel user"),
        "the installer requests elevation it does not need"
    );
}

/// The release gate says no Flutter, Dart or generated FFI artifacts exist
/// anywhere in the tree. This walks the source directories and says so.
#[test]
fn no_retired_stack_artifacts_survive_anywhere_in_the_tree() {
    let root = repo_root();
    let mut found = Vec::new();
    for directory in ["tools", "packaging"] {
        walk(&root.join(directory), &mut |path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if name.ends_with(".dart")
                || name == "pubspec.yaml"
                || name == "pubspec.lock"
                || name == "flutter_windows.dll"
            {
                found.push(path.to_path_buf());
            }
        });
    }
    assert!(
        found.is_empty(),
        "artifacts from a retired stack are still in the tree: {found:?}"
    );
}

fn walk(directory: &Path, visit: &mut impl FnMut(&Path)) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // `target` is build output, and walking it is both slow and meaningless.
        if path.file_name().is_some_and(|name| name == "target") {
            continue;
        }
        if path.is_dir() {
            walk(&path, visit);
        } else {
            visit(&path);
        }
    }
}
