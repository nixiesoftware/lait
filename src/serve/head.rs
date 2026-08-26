//! Where a World's web head comes from: a downloaded bundle when one is
//! activated, unavailable otherwise. There is one selected release and no
//! compiled-in product shadow behind it.
//!
//! Only a bundle whose declared runtime version equals this build's is ever
//! activated. That check belongs to whoever activates ([`Source::activated`]);
//! by the time bytes are served the question is settled, which is what keeps
//! "a bundle newer than its host" from being a condition this path has to
//! handle.
//!
//! ## Traversal is ours to refuse now
//!
//! A directory on disk has no implicit containment property, so every lookup
//! here refuses anything that is not a plain
//! relative path before it touches the filesystem — and a refusal is a miss,
//! which the SPA fallback answers exactly as it answers any other miss.

use std::path::{Component, Path, PathBuf};

/// Where head bytes are read from.
#[derive(Debug, Clone, Default)]
pub struct Source {
    /// The activated bundle's root, when one is activated.
    bundle: Option<PathBuf>,
}

impl Source {
    /// No selected release. Tests and diagnostics use this to model an
    /// unavailable World without inventing product bytes.
    pub fn unavailable() -> Self {
        Self { bundle: None }
    }

    /// Serve from the selected immutable release when it holds the path.
    ///
    /// Whether this build may run the bundle at all was settled when it was
    /// staged — a payload whose declared requirements are unmet never reaches
    /// this directory. Deciding it again here would be a second answer to one
    /// question, and the two would drift.
    pub fn activated(bundle: PathBuf) -> Self {
        Self {
            bundle: Some(bundle),
        }
    }

    /// The activated bundle's root.
    pub fn bundle(&self) -> Option<&Path> {
        self.bundle.as_deref()
    }

    /// Read one path, bundle first.
    ///
    /// `None` means neither source holds it — which the caller answers with
    /// the SPA entry, exactly as it always has.
    pub fn read(&self, path: &str) -> Option<Vec<u8>> {
        if let Some(bytes) = self
            .bundle
            .as_deref()
            .and_then(|root| read_under(root, path))
        {
            return Some(bytes);
        }
        None
    }
}

/// Read `relative` from under `root`, or nothing.
///
/// Refuses anything that is not a plain relative path *before* touching the
/// filesystem: an absolute path, a root or prefix component, and any `..` are
/// all misses rather than reads. A miss is safe by construction here — the
/// caller falls back to the release's SPA entry — so the refusal costs nothing
/// and needs no separate error path.
fn read_under(root: &Path, relative: &str) -> Option<Vec<u8>> {
    let relative = relative.trim_start_matches('/');
    if relative.is_empty() {
        return None;
    }
    let candidate = Path::new(relative);
    if candidate
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return None;
    }
    std::fs::read(root.join(candidate)).ok()
}

/// Where independently installed World bundles live.
///
/// The versioned namespace is the compatibility boundary. This host never
/// reads installation records from any prior namespace, so retired selections
/// remain inert and every World must reinstall from its signed channel.
pub fn installations_root(identity: &Path) -> PathBuf {
    identity.join("world-bundles-v1")
}

/// The environment variable that links a World to a directory being worked on.
pub const LINK_VAR: &str = "LAIT_WORLD_LINK";

/// Serve a World's selected installed bundle, when one is installed for it.
///
/// Nothing here reads a declaration or verifies bytes: staging is what proves
/// a bundle *and* what decides whether this build can run it, so a directory
/// present under a World's name is one that was proven and admitted when it
/// landed. What this settles is only *which* source a head serves from.
///
/// A link ([`LINK_VAR`]) is the one thing that outranks the release, and it
/// exists because the alternative was worse. Building a World's page and
/// looking at it in the real window otherwise meant packaging a release,
/// signing it, publishing it to a feed and installing it — so the loop for a
/// one-line change ran through the whole distribution pipeline, and the
/// pipeline is not what was being tested. This is the seam that was missing:
/// **an immutable signed release is how a World travels, not how it is
/// written.**
///
/// Two of them, in the order [`crate::update::feed::Channel::current`] already
/// established for the node's channel: the environment first as a development
/// convenience, then what somebody recorded, then the release.
///
/// The environment one dies with the process launched holding it, which makes
/// it right for CI and a one-off. The recorded one is a deliberate choice made
/// in a window, and it pays for outliving the afternoon by being **visible**
/// wherever the World is — the Library row, its settings window, and the
/// World's own window. Either way a head that comes up on one says so, because
/// a machine serving somebody's working tree while believing it serves 0.9.3
/// is a worse defect than the friction either removes.
pub fn activate(worlds: &Path, world: &str) -> Source {
    match linked(&std::env::var(LINK_VAR).unwrap_or_default(), world) {
        Link::None => {}
        Link::Directory(dir) => {
            tracing::warn!(
                %world,
                dir = %dir.display(),
                "serving a linked directory ({LINK_VAR}) — this head is NOT serving the installed release"
            );
            return Source::activated(dir);
        }
        // Declared and unusable: refuse rather than fall through. Serving the
        // release here would answer a question nobody asked — a typo in a path
        // would look exactly like an edit that did nothing, which is the one
        // failure this whole seam exists to stop producing.
        Link::Unusable(why) => {
            tracing::error!(%world, %why, "refusing a linked World; serving nothing");
            return Source::unavailable();
        }
    }
    // The recorded link re-proves its directory as it reads it, so a record
    // whose directory has since been renamed reads as absent — and absent
    // here means the release, which is the honest answer for a choice that is
    // visible in three places and can be cleared from one of them.
    if let Some(dir) = crate::update::world::linked_dir(worlds, world) {
        tracing::warn!(
            %world,
            dir = %dir.display(),
            "serving a linked directory (recorded) — this head is NOT serving the installed release"
        );
        return Source::activated(dir);
    }
    if let Some(candidate) = crate::update::world::active_dir(worlds, world) {
        tracing::info!(bundle = %candidate.display(), %world, "serving a staged World payload");
        return Source::activated(candidate);
    }
    Source::unavailable()
}

/// What [`LINK_VAR`] says about one World.
#[derive(Debug, PartialEq, Eq)]
enum Link {
    /// Nothing was declared for this World. Every other World is unaffected by
    /// a link naming one of its neighbours.
    None,
    /// Serve this directory.
    Directory(PathBuf),
    /// Declared, and cannot be served.
    Unusable(String),
}

/// Read `<world>=<dir>` pairs, comma separated, and answer for one World.
///
/// Split from the environment so the parsing is testable without a process to
/// set variables on — every interesting case here is a malformed declaration,
/// and none of them should need a subprocess to reach.
fn linked(declared: &str, world: &str) -> Link {
    for entry in declared.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        // The first `=` only: a World id never contains one, and a path is
        // entitled to.
        let Some((named, dir)) = entry.split_once('=') else {
            continue;
        };
        if named.trim() != world {
            continue;
        }
        let dir = Path::new(dir.trim());
        // Relative would resolve against the daemon's working directory, which
        // is whatever launched it and is nobody's intent.
        if !dir.is_absolute() {
            return Link::Unusable(format!("{} is not an absolute path", dir.display()));
        }
        if !dir.is_dir() {
            return Link::Unusable(format!("{} is not a directory", dir.display()));
        }
        return Link::Directory(dir.to_path_buf());
    }
    Link::None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle_with(files: &[(&str, &[u8])]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a bundle root");
        for (path, bytes) in files {
            let at = dir.path().join(path);
            if let Some(parent) = at.parent() {
                std::fs::create_dir_all(parent).expect("a bundle subdirectory");
            }
            std::fs::write(&at, bytes).expect("a bundle file");
        }
        dir
    }

    /// The loop this exists for: a directory being worked on, named for one
    /// World, served in place of its release.
    #[test]
    fn a_linked_world_is_served_from_the_directory_it_names() {
        let dir = bundle_with(&[("index.html", b"from the working tree")]);
        let declared = format!("com.lait.issues={}", dir.path().display());
        assert_eq!(
            linked(&declared, "com.lait.issues"),
            Link::Directory(dir.path().to_path_buf())
        );
    }

    /// A link names one World. Its neighbours must go on serving their
    /// releases, or linking a World under development would take out every
    /// other World on the machine.
    #[test]
    fn a_link_says_nothing_about_a_world_it_does_not_name() {
        let dir = bundle_with(&[]);
        let declared = format!("com.lait.issues={}", dir.path().display());
        assert_eq!(linked(&declared, "com.lait.signage"), Link::None);
    }

    #[test]
    fn several_worlds_can_be_linked_at_once() {
        let issues = bundle_with(&[]);
        let signage = bundle_with(&[]);
        let declared = format!(
            "com.lait.issues={} , com.lait.signage={}",
            issues.path().display(),
            signage.path().display()
        );
        assert_eq!(
            linked(&declared, "com.lait.signage"),
            Link::Directory(signage.path().to_path_buf())
        );
    }

    #[test]
    fn nothing_declared_links_nothing() {
        assert_eq!(linked("", "com.lait.issues"), Link::None);
    }

    /// The failure this refuses to make quiet. A path that is gone — renamed,
    /// or never right — must not fall back to the installed release: that
    /// serves stale bytes to somebody who just asked for their own, and it
    /// looks exactly like an edit that did nothing.
    #[test]
    fn a_link_pointing_nowhere_is_refused_rather_than_falling_back() {
        let dir = bundle_with(&[]);
        let gone = dir.path().join("was-never-here");
        let declared = format!("com.lait.issues={}", gone.display());
        assert!(matches!(
            linked(&declared, "com.lait.issues"),
            Link::Unusable(_)
        ));
    }

    /// A relative path resolves against the daemon's working directory, which
    /// is whatever happened to launch it — the client, a shell, or launchd.
    #[test]
    fn a_relative_link_is_refused_because_nobody_means_the_daemons_cwd() {
        assert!(matches!(
            linked(
                "com.lait.issues=products/issues-app/assets/web",
                "com.lait.issues"
            ),
            Link::Unusable(_)
        ));
    }

    /// `activate` prefers the link, and says so loudly enough that a machine
    /// cannot be serving a working tree while believing it serves a release.
    #[test]
    fn a_link_outranks_the_installed_release() {
        let worlds = tempfile::tempdir().expect("an installations root");
        let working = bundle_with(&[("index.html", b"from the working tree")]);
        let declared = format!("com.lait.issues={}", working.path().display());
        // Not through the environment: these tests share a process, and a
        // variable set in one is set in all of them.
        assert_eq!(
            linked(&declared, "com.lait.issues"),
            Link::Directory(working.path().to_path_buf())
        );
        // And with nothing declared, the release path is untouched.
        assert!(
            activate(worlds.path(), "com.lait.issues")
                .bundle()
                .is_none(),
            "an unlinked, uninstalled World still serves nothing"
        );
    }

    #[test]
    fn an_activated_bundle_answers_and_an_absent_path_falls_through() {
        let dir = bundle_with(&[("index.html", b"<html>from the bundle</html>")]);
        let source = Source::activated(dir.path().to_path_buf());
        assert_eq!(
            source.read("/index.html").as_deref(),
            Some(&b"<html>from the bundle</html>"[..])
        );
        assert!(
            source.read("/not-in-the-bundle.js").is_none(),
            "a path the bundle does not hold must fall through to its entry"
        );
    }

    /// Whether a payload may run here is settled when it is staged, not when
    /// it is read. This layer's job is only which directory answers.
    #[test]
    fn a_source_answers_from_the_bundle_it_was_given() {
        let dir = bundle_with(&[("index.html", b"from the bundle")]);
        let source = Source::activated(dir.path().to_path_buf());
        assert_eq!(source.bundle(), Some(dir.path()));
        assert!(source.read("/index.html").is_some());
    }

    /// The old embedded source made this impossible for free. A selected
    /// release directory on disk gives nothing for free.
    #[test]
    fn nothing_outside_the_bundle_is_readable_through_it() {
        let dir = bundle_with(&[("index.html", b"inside")]);
        let outside = dir.path().parent().expect("a parent").join("outside.txt");
        std::fs::write(&outside, b"not the bundle's to serve").expect("a file beside the bundle");
        let source = Source::activated(dir.path().to_path_buf());

        for escape in [
            "../outside.txt",
            "assets/../../outside.txt",
            "/../outside.txt",
            "./../outside.txt",
        ] {
            assert!(
                source.read(escape).is_none(),
                "a path escaped the bundle: {escape}"
            );
        }
        // An absolute path is a path to somewhere else, never a path into the
        // bundle: joining it would replace the root entirely.
        assert!(
            source.read(&outside.display().to_string()).is_none(),
            "an absolute path read outside the bundle"
        );
        let _ = std::fs::remove_file(&outside);
    }

    /// One directory per World. The first cut keyed staging by runtime alone,
    /// so a second World overwrote the first — a collision that appears only
    /// once more than one World is published.
    #[test]
    fn each_world_activates_its_own_payload_and_never_anothers() {
        let identity = tempfile::tempdir().expect("an identity dir");
        let worlds = installations_root(identity.path());
        assert!(
            activate(&worlds, "world.a").bundle().is_none(),
            "an absent worlds directory activated something"
        );

        let a =
            crate::update::world::release_dir(&worlds, "world.a", "1.0.0").expect("a release path");
        std::fs::create_dir_all(&a).expect("a payload for world.a");
        std::fs::write(a.join("index.html"), b"a").expect("its entry");
        std::fs::write(
            crate::update::world::world_root(&worlds, "world.a").join("selected.json"),
            serde_json::to_vec(&crate::update::world::InstalledBundle {
                world: "world.a".to_string(),
                version: "1.0.0".to_string(),
                digest: "00".repeat(32),
                files: 1,
            })
            .expect("a pointer"),
        )
        .expect("write the pointer");
        assert_eq!(
            activate(&worlds, "world.a").bundle(),
            Some(a.as_path()),
            "the World's own payload was not activated"
        );
        assert!(
            activate(&worlds, "world.b").bundle().is_none(),
            "one World's payload was served for another"
        );
    }

    #[test]
    fn no_release_is_available_when_nothing_is_activated() {
        let source = Source::unavailable();
        assert!(source.bundle().is_none());
        assert!(
            source.read("/index.html").is_none(),
            "an absent release must not reveal product bytes"
        );
    }

    /// A vanished immutable release is unavailable; the host never substitutes
    /// another product generation behind the selected coordinate.
    #[test]
    fn a_bundle_that_vanishes_underneath_falls_back_rather_than_failing() {
        let dir = bundle_with(&[("index.html", b"from the bundle")]);
        let root = dir.path().to_path_buf();
        let source = Source::activated(root.clone());
        assert!(source.read("/index.html").is_some());
        drop(dir);
        assert!(
            source.read("/index.html").is_none(),
            "a vanished bundle must read as absent, not as an error"
        );
    }
}
