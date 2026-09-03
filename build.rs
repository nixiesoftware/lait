//! Build-time version stamping for the product-blind host binary.
//!
//! Exposes `LAIT_VERSION_LONG` — the string `lait --version` prints — as a
//! compile-time env the CLI reads via `env!`.
//!
//! Stable/release builds print a clean semver (`0.4.5`). A **dev-channel** build
//! (the `Dev Release` workflow, or any build that sets `LAIT_BUILD_SHA`) appends a
//! `-dev+<sha> (<date>)` suffix so a nightly binary is unmistakable from a tagged
//! release. We deliberately read **only** explicit env vars — never shell out to
//! git — so a tagged build from the repository-owned release workflow stays
//! clean and reproducible, and only the dev workflow opts into the suffix.

use std::env;
fn main() {
    // Re-run only when the stamping inputs change (not on every source edit).
    println!("cargo:rerun-if-env-changed=LAIT_BUILD_SHA");
    println!("cargo:rerun-if-env-changed=LAIT_BUILD_DATE");
    // Emitting any `rerun-if-*` replaces cargo's default "re-run when anything in
    // the package changed", so build.rs has to name itself or edits to this file
    // stop taking effect.
    println!("cargo:rerun-if-changed=build.rs");

    let base = env::var("CARGO_PKG_VERSION").unwrap_or_default();
    let explicit_sha = env::var("LAIT_BUILD_SHA").unwrap_or_default();
    let mut sha = explicit_sha.clone();
    let mut date = env::var("LAIT_BUILD_DATE").unwrap_or_default();

    // A debug-profile build is never a tagged release, so it may ask git who
    // it is. Without this a debug binary and a shipped one both said `0.9.9`,
    // and an evening was spent not knowing which of them held the display
    // port. Release builds keep the rule above: explicit env vars only.
    if sha.is_empty() && env::var("PROFILE").as_deref() == Ok("debug") {
        if let Some(described) = git_describe() {
            sha = described;
            if date.is_empty() {
                date = unix_now_utc();
            }
        }
    }

    // Human-facing version for `lait --version`.
    let long = if sha.is_empty() {
        base.clone()
    } else if date.is_empty() {
        format!("{base}-dev+{sha}")
    } else {
        format!("{base}-dev+{sha} ({date})")
    };

    // A VALID-semver form for the self-updater's version comparison. A dev build
    // uses a PRERELEASE identifier (`X.Y.Z-dev.<sha>`), which semver orders BELOW
    // the stable `X.Y.Z` — so a self-update on a dev node correctly sees the
    // stable release as newer and heals onto it. LAIT_VERSION_LONG can't be used
    // here: its ` (<date>)` suffix is not valid semver, and the bare
    // CARGO_PKG_VERSION would make a dev node report itself as the stable version
    // (so the updater saw "already up to date" and stranded it on the dev build).
    // Only an explicit dev-channel stamp changes the updater's view of this
    // binary; a git stamp on a debug build names it in `--version` and the
    // daemon log without making the self-updater treat it as a dev node.
    let semver = if explicit_sha.is_empty() {
        base
    } else {
        format!("{base}-dev.{explicit_sha}")
    };

    println!("cargo:rustc-env=LAIT_VERSION_LONG={long}");
    // A new commit re-stamps; a dirty tree is stamped as of this run. The
    // running daemon also logs its executable's path and mtime, which is the
    // fact that stays true when this one goes stale.
    println!("cargo:rerun-if-changed=.git/HEAD");
    if let Ok(head) = std::fs::read_to_string(".git/HEAD") {
        if let Some(reference) = head.trim().strip_prefix("ref: ") {
            println!("cargo:rerun-if-changed=.git/{reference}");
        }
    }
    println!("cargo:rustc-env=LAIT_VERSION_SEMVER={semver}");

    // The target triple this binary is FOR (cross-compile aware: TARGET, not
    // HOST). The self-updater addresses the release manifest's artifact table
    // by it, instead of a `#[cfg]` split that can only ever be tested on the
    // platform it selects.
    let target = env::var("TARGET").expect("cargo always sets TARGET for build scripts");
    println!("cargo:rustc-env=LAIT_TARGET={target}");
}

/// `<short sha>[-dirty]` from git, or `None` when git or the tree is absent.
fn git_describe() -> Option<String> {
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let sha = run(&["rev-parse", "--short=8", "HEAD"])?;
    if sha.is_empty() {
        return None;
    }
    let dirty = run(&["status", "--porcelain", "--untracked-files=no"])
        .map(|status| !status.is_empty())
        .unwrap_or(false);
    Some(if dirty { format!("{sha}-dirty") } else { sha })
}

/// The build instant as `YYYY-MM-DDTHH:MMZ`, without a dependency.
fn unix_now_utc() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    // Civil date from days since epoch (Howard Hinnant's algorithm).
    let days = seconds / 86_400;
    let rem = seconds % 86_400;
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60
    )
}
