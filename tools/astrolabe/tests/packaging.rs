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
    // Both are written after one `SetOutPath "$INSTDIR\current"`, which is
    // what makes them siblings — the property `sidecar::resolve` depends on,
    // and `update::custody_of` is its inverse. Matched on the whole directive,
    // so the header comment naming the same file cannot stand in for the line
    // that installs it.
    let script = directives();
    let out = script
        .find(r#"SetOutPath "$INSTDIR\current""#)
        .expect("the release out path");
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

    // And the stub is not between them: it takes the root name, so the
    // shortcut, the protocol command and any pin a person makes keep the path
    // they have always had while the release beneath moves.
    assert!(
        script.contains(r#"File "/oname=astrolabe.exe" "${STUB}""#),
        "the installer does not place the stub at the install root"
    );
    let stub = script
        .find(r#"File "/oname=astrolabe.exe" "${STUB}""#)
        .expect("the stub");
    assert!(
        stub < out,
        "the stub is installed after the release, so it would land inside it"
    );

    // Portable editor bindings invoke `lait mcp` from PATH. The installer must
    // register its own release after it exists and unregister it before the
    // helper is deleted, or a clean machine works only when Cargo happened to
    // install another copy first.
    let register = script
        .find(r#"ExecWait '"$INSTDIR\astrolabe.exe" --install-command-path'"#)
        .expect("the native install never registers its bundled lait command");
    let uninstall = script
        .find(r#"ExecWait '"$INSTDIR\astrolabe.exe" --uninstall-command-path'"#)
        .expect("uninstall leaves its bundled lait command registered");
    let delete = script
        .find(r#"Delete "$INSTDIR\astrolabe.exe""#)
        .expect("the uninstaller does not remove the stable stub");
    assert!(
        daemon < register && uninstall < delete,
        "PATH registration is not bounded by the installed helper's lifetime"
    );
}

/// Nothing outside the install may point into a release directory.
///
/// The most expensive mistake in this space, by evidence: Squirrel's
/// `app-1.0.0` / `app-1.0.1` layout broke firewall rules, antivirus
/// exclusions, GPU preferences and tray pinning, and it had to grow a routine
/// that rewrote users' pinned shortcuts on every update. Every shell artifact
/// here keys on `$INSTDIR\astrolabe.exe`, which no update ever moves.
#[test]
fn every_shell_artifact_points_at_the_stub_and_never_into_a_release() {
    let script = directives();
    for artifact in [
        r#"CreateShortcut "$SMPROGRAMS\Astrolabe.lnk" "$INSTDIR\astrolabe.exe""#,
        r#"WriteRegStr HKCU "Software\Classes\lait\DefaultIcon" "" "$INSTDIR\astrolabe.exe,0""#,
        r#"WriteRegStr HKCU "Software\Classes\lait\shell\open\command" "" '"$INSTDIR\astrolabe.exe" "%1"'"#,
    ] {
        assert!(
            script.contains(artifact),
            "a shell artifact does not point at the stub: {artifact}"
        );
    }
    for line in script.lines() {
        let line = line.trim();
        let points_outward = line.starts_with("CreateShortcut")
            || line.starts_with("WriteRegStr")
            || line.starts_with("WriteRegDWORD");
        assert!(
            !(points_outward && line.contains(r#"$INSTDIR\current"#)),
            "a shell artifact points into a release directory, which is what \
             breaks pins, firewall rules and antivirus exclusions on the first \
             update: {line}"
        );
    }
}

/// Nothing from a retired stack survives in the installer. The list has
/// inverted twice as interfaces came and went; today the Tauri host is the
/// client and the Flutter payload is the corpse.
#[test]
fn the_installer_carries_nothing_from_the_retired_stacks() {
    let script = directives().to_ascii_lowercase();
    for corpse in ["flutter", "dartjni", "icudtl", "msvcp140", "warpui", "egui"] {
        assert!(
            !script.contains(corpse),
            "the installer references '{corpse}', which no longer exists in this design"
        );
    }
}

/// WebView2 is the one runtime the pair does not carry: the host draws
/// through the system's Evergreen install, updated on the OS's schedule —
/// which is the evergreen design's whole point on Windows. A machine without
/// it must be given Microsoft's own bootstrapper, not a loader dialog.
#[test]
fn the_installer_ensures_webview2() {
    let script = directives();
    for required in [
        // The presence check, against Evergreen's registration.
        r#"EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"#,
        // The bootstrapper, staged by build-astrolabe.sh and never installed.
        r#"MicrosoftEdgeWebview2Setup.exe"#,
    ] {
        assert!(
            script.contains(required),
            "the installer omits {required}; a machine without WebView2 gets a loader dialog"
        );
    }
}

/// The pair carries its own C runtime.
///
/// The Flutter bundle staged msvcp/vcruntime DLLs app-locally; the Tauri pair
/// is built `+crt-static` by build-astrolabe.sh instead, so the installer
/// ships no runtime DLLs and a clean machine has nothing to be missing. Both
/// halves are pinned: the flag where the build sets it, and the absence of
/// any DLL enumeration that would quietly resurrect the old posture.
#[test]
fn the_installer_carries_the_c_runtime() {
    let build = std::fs::read_to_string(repo_root().join("packaging/build-astrolabe.sh"))
        .expect("build-astrolabe.sh");
    assert!(
        build.contains("crt-static"),
        "the Windows pair is not built with a static C runtime, and nothing ships one"
    );
    let script = directives();
    for corpse in [r#"msvcp140"#, r#"vcruntime140"#, r#"concrt140"#] {
        assert!(
            !script.to_ascii_lowercase().contains(corpse),
            "the installer enumerates {corpse}, which a static-CRT pair must not ship"
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

    // Recursive removal is allowed for the release trees, by name, and
    // nowhere else.
    //
    // The release trees hold whatever the *update path* put there — after the
    // first update, not what any installer shipped — so the rule is a bound
    // rather than a ban, and the bound is stated per directory.
    // `$INSTDIR` itself still comes off with plain `RMDir`, which refuses a
    // non-empty directory: an unknown file left behind fails the uninstall
    // visibly instead of being swept away with everything around it.
    let allowed = [
        r#""$INSTDIR\current""#,
        r#""$INSTDIR\previous""#,
        r#""$INSTDIR\staged""#,
    ];
    for line in uninstall.lines() {
        let line = line.trim();
        let Some(target) = line.strip_prefix("RMDir /r ") else {
            continue;
        };
        assert!(
            allowed.contains(&target.trim()),
            "the uninstaller recursively removes {target}, which is wider than \
             a release tree — that is how a store gets destroyed by an \
             uninstall nobody read"
        );
    }
    // And every release tree is actually removed: a tree left behind is an
    // install directory that survives its own uninstall.
    for tree in allowed {
        assert!(
            uninstall.contains(&format!("RMDir /r {tree}")),
            "the uninstaller leaves {tree} behind"
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

/// The installer lands where the caller told it to, not beside this script.
///
/// NSIS resolves a relative `OutFile` against the *script's* directory, not the
/// working directory. So `cd dist && makensis ..\packaging\windows\astrolabe.nsi`
/// compiled a perfectly good installer into `packaging\windows\` while the
/// release job looked in `dist\`, found nothing, and reported "makensis produced
/// no installer" — a success that read as a build failure. Three releases
/// (v0.8.0, v0.8.1, v0.8.2) shipped a macOS DMG and no Windows installer before
/// anyone read the compile log far enough to see `Output:` naming the wrong
/// directory.
///
/// The fix is that the caller passes `OUTDIR` absolute. This asserts the script
/// honours it, because a relative `OutFile` fails silently in exactly the
/// direction that looks like somebody else's bug.
#[test]
fn the_installer_is_written_where_the_caller_asked() {
    let script = directives();
    let out_file = script
        .lines()
        .find(|line| line.trim_start().starts_with("OutFile"))
        .expect("the script declares an OutFile");
    assert!(
        out_file.contains("${OUTDIR}"),
        "OutFile is relative to the script directory, so the caller's output \
         directory is ignored and the installer lands beside the .nsi: {out_file}"
    );
    assert!(
        script.contains("!define OUTDIR"),
        "OUTDIR has no default, so a standalone `makensis astrolabe.nsi` errors \
         on an undefined symbol instead of writing beside the script"
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
    // And it goes when the program goes. The notices ship inside the release,
    // so they leave with the tree rather than by a `Delete` of their own — a
    // file named individually here would be one the update path could move
    // out from under.
    let uninstall = directives()
        .split("Section \"Uninstall\"")
        .nth(1)
        .expect("an uninstall section")
        .to_owned();
    assert!(
        uninstall.contains(r#"RMDir /r "$INSTDIR\current""#),
        "uninstalling leaves the release, and the notices in it, behind"
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

fn client_build_script() -> String {
    let path = repo_root().join("packaging/build-astrolabe.sh");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// The pair ships together, macOS spelling: `sidecar::resolve` looks beside
/// the executable, and `Contents/MacOS` is beside. A bundle missing either
/// half — or carrying a sidecar that does not run — must be refused as a
/// packaging input, not discovered at someone's first launch.
#[test]
fn the_dmg_refuses_a_bundle_missing_either_half_of_the_pair() {
    let script = dmg_directives();
    for binary in ["astrolabe", "lait"] {
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
/// the bundle that seals it, and never *signed* with `--deep`. Notarization
/// requires the runtime flag on every executable; deep signing is the
/// deprecated way to half-do this sight unseen — an enumerated payload is the
/// point, same as the NSIS file list. Deep verification remains desirable.
#[test]
fn the_dmg_signs_inside_out_with_the_hardened_runtime_and_never_deep() {
    let script = dmg_directives();
    assert!(
        script.contains("--options runtime"),
        "signing without the hardened runtime notarizes nothing"
    );
    for command in script
        .lines()
        .filter(|line| line.trim_start().starts_with("codesign --force"))
    {
        assert!(
            !command.contains("--deep"),
            "--deep signs every nesting level sight unseen; enumerate the payload instead"
        );
    }
    for signed in [r#"sign "$STAGED/Contents/MacOS/lait""#, r#"sign "$STAGED""#] {
        assert!(script.contains(signed), "not signed explicitly: {signed}");
    }
    let runners = script
        .find(r#"find "$WORLD_ROOT" -type f -path '*/bin/*' -print0"#)
        .expect("the packager does not enumerate bundled World runners");
    let sidecar = script
        .find(r#"sign "$STAGED/Contents/MacOS/lait""#)
        .expect("the packager does not sign the lait sidecar");
    let bundle = script
        .find(r#"sign "$STAGED""#)
        .expect("the packager does not sign the outer app");
    assert!(
        script.contains(r#"file -b "$runner""#)
            && script.contains(r#"sign "$runner""#)
            && runners < sidecar
            && sidecar < bundle,
        "nested World runners, the sidecar, and the outer app are not signed inside-out"
    );
    assert!(
        script.contains(r#"codesign --verify --deep --strict --verbose=1 "$STAGED""#),
        "the preflight does not recursively verify the code graph Apple notarizes"
    );
    assert!(
        !script.contains("libastrolabe.dylib")
            && !script.contains("apps/astrolabe/macos/Runner/Release.entitlements"),
        "the Tauri packager still depends on the retired Flutter payload"
    );
}

/// A signed installer paired with an unsigned update tree would install once
/// and then replace itself with a bundle Gatekeeper cannot verify. The DMG
/// packager therefore exports its sealed staging app, and the tree consumes
/// that output rather than the raw Tauri build directory.
#[test]
fn the_macos_update_tree_is_built_from_the_app_sealed_for_the_dmg() {
    let dmg = dmg_directives();
    assert!(
        dmg.contains(r#"cp -R "$STAGED" "$SIGNED_APP_OUT""#),
        "the sealed DMG payload cannot be exported for the update tree"
    );

    let build = client_build_script();
    assert!(
        build.contains(r#"--signed-app-out "$SIGNED_APP""#),
        "the release build does not retain the app sealed by make-dmg"
    );
    assert!(
        build.contains(r#"TREE_APP="$SIGNED_APP""#)
            && build.contains(r#"make-tree.sh" --stage "$TREE_APP""#),
        "the macOS update tree is still packed from the unsigned build output"
    );
}

/// The feed's artifact keys are a closed platform vocabulary. Building on an
/// extra Rust host must fail before producing a target-named tree that no
/// manifest entry or installed client can ever select.
#[test]
fn the_client_builder_accepts_exactly_the_feed_supported_targets() {
    let build = client_build_script();
    for target in [
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
        "x86_64-unknown-linux-gnu",
    ] {
        assert!(
            build.contains(target),
            "the builder cannot emit the feed's {target} artifact"
        );
    }
    assert!(
        build.contains("unsupported client target '$TARGET'") && build.contains("exit 1"),
        "hosts outside the feed matrix are allowed to emit undiscoverable artifacts"
    );
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
        script.contains(r#"[ "$notary_status" != "Accepted" ]"#)
            && script.contains("notarytool log"),
        "an Invalid notarization can reach stapling without printing Apple's diagnostic log"
    );
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
    for required in ["astrolabe", "lait"] {
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

/// The whole stage travels: copying selected files would silently drop
/// whatever a future release adds beside the pair.
#[test]
fn the_linux_package_carries_the_pair_and_notices() {
    let script = linux_directives();
    assert!(
        script.contains(r#"cp -a "$BUNDLE/." "$STAGED/current/""#),
        "the Linux package copies selected files instead of the whole stage"
    );
    // The stub takes the root name, and the release sits beneath it — the
    // same shape as Windows, for the same reason: a path outside the install
    // must not move when a release does.
    assert!(
        script.contains(r#"cp "$STUB" "$STAGED/astrolabe""#),
        "the Linux package does not place the stub at the archive root"
    );
    assert!(
        script.contains(r#"mkdir "$STAGED/current""#),
        "the Linux package does not place the release under current/"
    );
    assert!(
        script.contains(r#"cp "$REPO/THIRD-PARTY-NOTICES.md""#),
        "the Linux package ships binaries without their notices"
    );
    assert!(
        script.contains(r#"find "$STAGED" -type f -exec chmod 0644 {} +"#)
            && script.contains(
                r#"chmod 0755 "$STAGED/astrolabe" "$STAGED/current/astrolabe" "$STAGED/current/lait""#
            ),
        "the Linux package inherits host-specific file modes, or leaves the \
         stub or a half of the pair unexecutable"
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
        notices.contains("PolyForm-Noncommercial-1.0.0"),
        "the notices do not state what lait itself is offered under"
    );
}

/// The client finds its sidecar, and the sidecar knows the client owns it.
///
/// These are two functions in two crates describing one layout, and they are
/// asserted together because that is the only way they cannot drift. If they
/// ever disagree the failure is invisible until the machine that matters: the
/// client would attach to a daemon that believes it may replace itself, and a
/// minor bump would leave a client unable to start one.
#[test]
fn the_client_and_its_sidecar_agree_about_the_layout() {
    let root = tempfile::tempdir().expect("a fake installation");
    let client = root.path().join(if cfg!(windows) {
        "astrolabe.exe"
    } else {
        "astrolabe"
    });
    std::fs::write(&client, b"the client").expect("stage the client");

    // The client's half: where it looks for the daemon it manages.
    let found = astrolabe::sidecar::beside_for_test(&client);
    std::fs::write(&found, b"the sidecar").expect("stage the sidecar");
    assert_eq!(
        found.file_name().and_then(|n| n.to_str()),
        Some(if cfg!(windows) { "lait.exe" } else { "lait" }),
        "the client looks for lait beside itself"
    );

    // The sidecar's half: who it believes owns replacing it.
    assert_eq!(
        lait::update::custody_of(&found),
        lait::update::Custody::Managed { by: client },
        "and that lait must know it is a component, not a self-managing install"
    );
}

/// The bundler's configuration is what makes the installed layout true, and
/// three of its fields are load-bearing in ways nothing else would catch.
///
/// The test above proves the two *functions* agree about the layout. This
/// proves the *bundle* produces it — which is a different claim, and the one
/// that broke when the client moved to Tauri: the bundler names the main
/// binary after `productName` unless told otherwise, so `Astrolabe` would
/// have shipped where `update::custody_of` looks for `astrolabe`. On macOS a
/// case-insensitive filesystem hides that; on Linux it does not, and either
/// way the symptom is a sidecar that believes it may replace itself.
///
/// Read from the config rather than from a built bundle so it runs anywhere,
/// including where no webview toolchain exists.
#[test]
fn the_bundle_is_configured_to_produce_the_layout_the_pair_rule_needs() {
    let path = repo_root().join("apps/astrolabe-web/src-tauri/tauri.conf.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let config: serde_json::Value = serde_json::from_str(&text).expect("tauri.conf.json parses");

    // 1. The entry binary's name, which `update::custody_of` looks for and
    //    `update::tree::entry_for` names in every tree.
    assert_eq!(
        config["mainBinaryName"].as_str(),
        Some("astrolabe"),
        "the bundle would ship a binary that custody_of cannot recognise"
    );

    // 2. The sidecar rides inside, so the pair ships together (CLIENT-12) and
    //    `sidecar::beside` finds it in the installed bundle.
    let external = config["bundle"]["externalBin"]
        .as_array()
        .expect("externalBin is declared");
    assert!(
        external
            .iter()
            .filter_map(serde_json::Value::as_str)
            .any(|entry| entry.ends_with("lait")),
        "no lait sidecar is bundled, so the installed client has no daemon to start: {external:?}"
    );

    // 3. The terms travel with the copy. PolyForm makes carrying them an
    //    obligation on whoever distributes one, and `make-tree.sh` refuses a
    //    tree without them — so an omission here fails a release rather than
    //    shipping quietly, but only if it is declared at all.
    let resources = &config["bundle"]["resources"];
    for required in ["LICENSE", "THIRD-PARTY-NOTICES.md"] {
        assert!(
            resources
                .as_object()
                .is_some_and(|map| map.values().any(|dest| dest.as_str() == Some(required))),
            "the bundle does not carry {required}"
        );
    }
}

// --- The terms --------------------------------------------------------------
//
// PolyForm's Notices section makes carrying the terms an obligation on whoever
// distributes a copy, not a courtesy. Each channel is asserted separately
// because each stages independently — and this is the failure mode that does
// not announce itself: nothing errors, the install simply lacks a file, and
// the omission is invisible until someone asks what terms they hold.

/// Windows ships the terms and shows them before anything is installed. A
/// person installing a noncommercial-licensed client on a work machine is
/// precisely who needs to read them at that moment rather than discover them
/// afterwards, which is why this is a page and not only a file.
#[test]
fn the_installer_ships_and_shows_the_licence() {
    let script = installer();
    assert!(
        script.contains(r#"File "${STAGE}\LICENSE""#),
        "the installer carries the binaries and not the terms they are offered under"
    );
    assert!(
        directives().contains(r#"MUI_PAGE_LICENSE "${STAGE}\LICENSE""#),
        "the installer never shows the terms it is installing under"
    );
    assert!(
        repo_root().join("LICENSE").is_file(),
        "there is nothing for the installer to carry"
    );
}

/// A drag-install copies the .app and nothing else — the same reasoning as the
/// notices, and so the same placement. Loose in the DMG, the terms stay behind
/// on an unmounted image and were never really shipped.
///
/// Placement alone is not the property. Signing the .app seals the state of
/// everything under it, so a file copied in *after* the seal does not ride
/// along quietly — it invalidates the signature. Both files must therefore be
/// staged before the bundle is signed, which is what is asserted here: the
/// Linux and Windows analogues check ordering too, and macOS is the platform
/// where getting it wrong is caught last and costs most.
#[test]
fn the_dmg_stages_the_terms_inside_the_bundle_before_it_is_sealed() {
    let script = dmg_directives();
    let at = |needle: &str| -> usize {
        script
            .find(needle)
            .unwrap_or_else(|| panic!("the DMG script no longer contains {needle:?}"))
    };

    // The bundle seal: the last `sign`, the one that takes `$STAGED` itself.
    let seal = at(r#"sign "$STAGED""#);

    assert!(
        at("Contents/Resources/LICENSE") < seal,
        "the terms are staged after the bundle is sealed, which breaks the signature"
    );
    assert!(
        at("Contents/Resources/THIRD-PARTY-NOTICES.md") < seal,
        "the notices are staged after the bundle is sealed, which breaks the signature"
    );
}

/// The tarball carries the terms into `current/`, beside the notices, and
/// refuses to build without them — the same guard the notices already had.
#[test]
fn the_linux_package_ships_the_licence() {
    let script = linux_directives();
    assert!(
        script.contains(r#"cp "$REPO/LICENSE""#),
        "the Linux package ships binaries without the terms they are offered under"
    );
    assert!(
        script.contains(r#"[ -f "$REPO/LICENSE" ]"#),
        "a missing LICENSE is not refused, so the package can ship without one"
    );
}

/// The one that actually regressed. The shared client builder now owns this
/// staging, so the assertion follows the packaging boundary rather than one CI
/// caller.
///
/// `make-tree.sh` packs the *bundle directory*, and the tree it produces is
/// what a self-update swaps into `current/`. So a file added afterwards by an
/// installer survives the install and not the first upgrade. Windows and macOS
/// staged into their bundle and were fine; Linux staged only inside
/// `make-tarball`, which writes to the tarball's own root — so its update tree
/// carried no notices at all, and would have carried no terms either.
///
/// Ordering is the property, so ordering is what is checked: the staging step
/// must appear before the `make-tree` call that consumes the directory.
#[test]
fn every_platform_stages_the_terms_where_the_update_tree_will_find_them() {
    let build = client_build_script();

    let at = |needle: &str| -> usize {
        build
            .find(needle)
            .unwrap_or_else(|| panic!("the client builder no longer contains {needle:?}"))
    };

    // Windows and Linux share this branch and pack the same `current/` tree.
    // The terms must be in that tree before it is sealed.
    assert!(
        at(r#"cp "$REPO/LICENSE" "$REPO/THIRD-PARTY-NOTICES.md" "$STAGE/$LIVE_DIR/""#)
            < at(r#"--stage "$STAGE/$LIVE_DIR""#),
        "the plain update tree is sealed before its terms are staged"
    );

    // macOS stages by a different route and is correct for a different reason:
    // `make-dmg` puts the terms inside the .app, and the tree is packed from
    // that staged copy rather than from the raw build output. Packing the build
    // output instead would reintroduce exactly the Linux defect.
    assert!(
        at(r#"bash "$REPO/packaging/macos/make-dmg.sh""#) < at(r#"--stage "$TREE_APP""#)
            && build.contains(r#"TREE_APP="$SIGNED_APP""#),
        "the macOS tree is not packed from the signed app carrying the terms"
    );
}
