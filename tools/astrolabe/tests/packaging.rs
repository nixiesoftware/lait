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

/// Nothing from a retired stack survives in the installer.
///
/// This list used to include Flutter and Dart, under revision 6, whose whole
/// point was that the client was a single self-contained executable. **Revision
/// 7 put the interface on Flutter over a Rust core**, so those two moved from
/// forbidden to required and the assertion below them inverted — see
/// [`the_installer_carries_the_whole_flutter_bundle`]. What remains here is the
/// genuinely dead: the browser shells this design never went back to.
#[test]
fn the_installer_carries_nothing_from_the_retired_stacks() {
    let script = directives().to_ascii_lowercase();
    for corpse in ["webview2", "tauri", "warpui", "egui"] {
        assert!(
            !script.contains(corpse),
            "the installer references '{corpse}', which no longer exists in this design"
        );
    }
}

/// The inverse of the test above, and the reason it had to change.
///
/// `astrolabe.exe` is a 92 KB runner. Installing it alone — which is exactly
/// what this script did for the whole of revision 7's first half — produces a
/// machine where the client cannot reach its first frame: no engine, no AOT
/// image, no ICU table. The failure arrives in the loader, before anything this
/// project wrote gets to run, which is the least diagnosable place it could.
#[test]
fn the_installer_carries_the_whole_flutter_bundle() {
    let script = directives();
    for required in [
        // The engine, and the Rust core reached across the bridge.
        r#"File "${STAGE}\flutter_windows.dll""#,
        r#"File "${STAGE}\astrolabe.dll""#,
        // The undecorated window and the tray icon are plugins, not framework.
        r#"File "${STAGE}\*_plugin.dll""#,
        // `data\` is resolved by path relative to the executable, so it has to
        // arrive under that name and no other.
        r#"SetOutPath "$INSTDIR\data""#,
        r#"File /r "${STAGE}\data\*.*""#,
    ] {
        assert!(
            script.contains(required),
            "the installer omits {required}, so the client cannot start"
        );
    }
}

/// The C runtime ships beside the client.
///
/// `astrolabe.exe` imports MSVCP140.dll and VCRUNTIME140*.dll. A development
/// machine has them because Visual Studio installed them, which is precisely
/// why leaving them out is invisible until somebody installs on a clean
/// machine — the one test nothing in CI substitutes for.
#[test]
fn the_installer_carries_the_c_runtime() {
    let script = directives();
    for required in [
        r#"File "${STAGE}\msvcp140*.dll""#,
        r#"File "${STAGE}\vcruntime140*.dll""#,
    ] {
        assert!(
            script.contains(required),
            "the installer omits {required}; a clean machine cannot start the client"
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

    // Recursive removal is allowed in exactly one place: the engine's own
    // payload directory.
    //
    // Under revision 6 this test forbade `RMDir /r` outright, and that was the
    // right rule for an installer that placed two files. Revision 7's bundle
    // nests `data\flutter_assets\<package>\...`, which cannot be enumerated —
    // so the rule becomes a bound rather than a ban. `$INSTDIR` itself still
    // comes off with plain `RMDir`, which refuses a non-empty directory: an
    // unknown file left behind fails the uninstall visibly instead of being
    // swept away with everything around it.
    for line in uninstall.lines() {
        let line = line.trim();
        let Some(target) = line.strip_prefix("RMDir /r ") else {
            continue;
        };
        assert_eq!(
            target.trim(),
            r#""$INSTDIR\data""#,
            "the uninstaller recursively removes {target}, which is wider than \
             the engine payload — that is how a store gets destroyed by an \
             uninstall nobody read"
        );
    }
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

/// The interface stays in `apps/astrolabe`, and the core stays out of it.
///
/// This test used to assert that no Flutter or Dart artifact existed *anywhere*
/// in the tree, which was revision 6's rule and is now false — the interface is
/// Dart. What survives is the boundary underneath it: `tools/astrolabe` is the
/// Rust core, `packaging/` is the installer, and a `.dart` file or a pubspec
/// appearing in either means the two halves have started to merge. The
/// directory list below is the whole assertion; it is deliberately not `apps/`.
#[test]
fn no_interface_artifacts_leak_into_the_core_or_the_packaging() {
    let root = repo_root();
    let mut found = Vec::new();
    for directory in ["tools", "packaging"] {
        walk(&root.join(directory), &mut |path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            // `name` is already lowercased above, so these comparisons are
            // case-insensitive in effect.
            if std::path::Path::new(&name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("dart"))
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
        "interface artifacts have leaked out of apps/astrolabe: {found:?}"
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

/// A person who receives these binaries is owed the list of what they are built
/// from. It is generated from the lockfile and held current by CI, so the only
/// way it fails to arrive is if the installer forgets to carry it.
#[test]
fn the_installer_ships_the_third_party_notices() {
    let script = installer();
    assert!(
        script.contains(r#"File "${STAGE}\THIRD-PARTY-NOTICES.md""#),
        "the installer places both binaries and no account of what is in them"
    );
    assert!(
        repo_root().join("THIRD-PARTY-NOTICES.md").is_file(),
        "there is nothing for the installer to carry"
    );
    // And it goes when the program goes. A notices file left behind names a
    // program that is no longer there.
    let uninstall = directives()
        .split("Section \"Uninstall\"")
        .nth(1)
        .expect("an uninstall section")
        .to_owned();
    assert!(
        uninstall.contains(r#"Delete "$INSTDIR\THIRD-PARTY-NOTICES.md""#),
        "uninstalling leaves the notices behind"
    );
}

// --- The macOS disk image ---------------------------------------------------
//
// packaging/macos/make-dmg.sh is the DMG counterpart of the NSIS script, and
// gets the same treatment: release criteria asserted against the script
// itself, on every push, on every platform. Reading the script is weaker than
// notarizing it — and the regressions that actually happen are edits to the
// script, which is exactly what a text scan catches.

fn dmg_script() -> String {
    let path = repo_root().join("packaging/macos/make-dmg.sh");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// The script with its comment lines removed — the same move as
/// [`directives`], for the same reason: the header prose *names* the
/// properties being checked, so a scan that read it would pass on the
/// documentation of the thing instead of the thing. Bash comments are
/// stripped per whole line (not at any `#`) because `$#` is code.
fn dmg_directives() -> String {
    dmg_script()
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The pair ships together, macOS spelling: `sidecar::resolve` looks beside
/// the executable, and `Contents/MacOS` is beside. A bundle missing either
/// half — or carrying a sidecar that does not run — must be refused as a
/// packaging input, not discovered at someone's first launch.
#[test]
fn the_dmg_refuses_a_bundle_missing_either_half_of_the_pair() {
    let script = dmg_directives();
    for binary in ["astrolabe", "lait", "libastrolabe.dylib"] {
        assert!(
            script.contains(binary),
            "the packaging never checks for {binary}"
        );
    }
    assert!(
        script.contains(r#""$STAGED/Contents/MacOS/lait" --version"#),
        "the staged sidecar is never run — presence is checked, execution is the claim"
    );
}

/// Every executable is signed with the hardened runtime, nested code before
/// the bundle that seals it, and never with `--deep`. Notarization requires
/// the runtime flag on every executable; `--deep` is the deprecated way to
/// half-do this sight unseen — an enumerated payload is the point, same as
/// the NSIS file list.
#[test]
fn the_dmg_signs_inside_out_with_the_hardened_runtime_and_never_deep() {
    let script = dmg_directives();
    assert!(
        script.contains("--options runtime"),
        "signing without the hardened runtime notarizes nothing"
    );
    assert!(
        !script.contains("--deep"),
        "--deep signs every nesting level sight unseen; enumerate the payload instead"
    );
    for signed in [
        r#"sign "$STAGED/Contents/MacOS/libastrolabe.dylib""#,
        r#"sign "$STAGED/Contents/MacOS/lait""#,
    ] {
        assert!(script.contains(signed), "not signed explicitly: {signed}");
    }
}

/// A signature from an "Apple Development" certificate succeeds locally and
/// fails notarization minutes later, naming neither the certificate nor the
/// script. The identity type is a precondition, checked where the mistake is
/// made.
#[test]
fn the_dmg_refuses_a_non_distribution_signing_identity() {
    assert!(
        dmg_directives().contains(r#""Developer ID Application"*)"#),
        "any codesigning identity is accepted; only Developer ID Application can be notarized"
    );
}

/// A drag-install copies the .app and nothing else, so the notices ride
/// inside the bundle — loose in the DMG they stay behind on an unmounted
/// image, which is shipping the binaries and not the account of what is in
/// them.
#[test]
fn the_dmg_ships_the_notices_inside_the_bundle() {
    assert!(
        dmg_directives().contains("Contents/Resources/THIRD-PARTY-NOTICES.md"),
        "the notices are not placed inside the app bundle"
    );
}

/// Notarization ends with the ticket stapled and Gatekeeper's own assessment
/// run — the check a customer's machine makes, made first on the machine that
/// can still do something about it. And the Xcode project stays out of it:
/// distribution identity lives at package time, never in the repository.
#[test]
fn the_dmg_staples_assesses_and_keeps_identity_out_of_the_project() {
    let script = dmg_directives();
    assert!(
        script.contains("stapler staple"),
        "an unstapled DMG needs Apple reachable at first launch"
    );
    assert!(
        script.contains("spctl --assess"),
        "the customer's Gatekeeper assessment is never rehearsed here"
    );
    let project = std::fs::read_to_string(
        repo_root().join("apps/astrolabe/macos/Runner.xcodeproj/project.pbxproj"),
    )
    .expect("the Runner project");
    assert!(
        !project.contains("DEVELOPMENT_TEAM"),
        "a personal team identity is pinned into the repository's Xcode project"
    );
}

// --- The Linux bundle ------------------------------------------------------

fn linux_script() -> String {
    let path = repo_root().join("packaging/linux/make-tarball.sh");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn linux_directives() -> String {
    linux_script()
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Linux has the same pair rule as Windows and macOS. The package boundary is
/// the last place both presence and execution can be proved together.
#[test]
fn the_linux_bundle_refuses_a_missing_or_mismatched_pair() {
    let script = linux_directives();
    for required in ["astrolabe", "lait", "libastrolabe.so", "data", "lib"] {
        assert!(
            script.contains(required),
            "the Linux package never checks for {required}"
        );
    }
    assert!(
        script.contains(r#"[ "$reported" != "lait $VERSION" ]"#),
        "the Linux sidecar version is not compared exactly to the package version"
    );
}

/// Flutter documents the whole bundle directory as its Linux distribution
/// unit. Copying selected files would silently drop a future engine artifact.
#[test]
fn the_linux_package_carries_the_whole_flutter_bundle_and_notices() {
    let script = linux_directives();
    assert!(
        script.contains(r#"cp -a "$BUNDLE/." "$STAGED/""#),
        "the Linux package enumerates a partial Flutter bundle"
    );
    assert!(
        script.contains(r#"cp "$REPO/THIRD-PARTY-NOTICES.md""#),
        "the Linux package ships binaries without their notices"
    );
    assert!(
        script.contains(r#"find "$STAGED" -type f -exec chmod 0644 {} +"#)
            && script.contains(r#"chmod 0755 "$STAGED/astrolabe" "$STAGED/lait""#),
        "the Linux package inherits host-specific file modes"
    );
}

/// A target-specific name keeps x64 and arm64 from overwriting one another in
/// a release, while the ldd gate catches a development-only library before it
/// becomes a clean-machine loader failure.
#[test]
fn the_linux_package_names_its_target_and_refuses_unresolved_libraries() {
    let script = linux_directives();
    assert!(script.contains("ldd \"$BUNDLE/$native\""));
    assert!(script.contains("not found"));
    assert!(script.contains("astrolabe-$VERSION-$TARGET"));
}

/// The notices are generated, and the file says so where somebody about to edit
/// it will read it. A hand-edited generated file is a file that silently stops
/// matching the thing it describes.
#[test]
fn the_notices_say_they_are_generated_and_name_what_generates_them() {
    let notices =
        std::fs::read_to_string(repo_root().join("THIRD-PARTY-NOTICES.md")).expect("the notices");
    assert!(
        notices.contains("ci/third-party-notices.sh"),
        "the notices do not name the generator that owns them"
    );
    assert!(
        notices.contains("do not edit it"),
        "the notices do not warn against being edited by hand"
    );
    // The claim the whole file exists to support.
    assert!(
        notices.contains("MIT OR Apache-2.0"),
        "the notices do not state what lait itself is offered under"
    );
}
