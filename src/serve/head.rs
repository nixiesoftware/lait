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
    /// Why this source holds nothing, when the reason is worth a sentence.
    ///
    /// Only a declared-and-unusable link sets this. "No release is selected"
    /// is the ordinary absence and explains itself; "you named a directory and
    /// it is not there" is a mistake somebody just made and can fix, and it
    /// reaches them only if it is carried to where they are looking.
    refused: Option<String>,
}

impl Source {
    /// No selected release. Tests and diagnostics use this to model an
    /// unavailable World without inventing product bytes.
    pub fn unavailable() -> Self {
        Self {
            bundle: None,
            refused: None,
        }
    }

    /// A link was declared for this World and cannot be served.
    ///
    /// Distinct from [`Self::unavailable`] because the two are different facts
    /// with different remedies, and the response says which. A `tracing::error`
    /// is not enough on its own: the head runs in a process that installs a
    /// subscriber only when it was started as a daemon, so in the loop this
    /// seam exists for the reason had nowhere to go and the page reported that
    /// the *release* carried no entry document — blaming a release that was
    /// fine for a typo in an environment variable.
    pub fn refused(why: String) -> Self {
        Self {
            bundle: None,
            refused: Some(why),
        }
    }

    /// Why this source refuses, when it was a link that refused it.
    pub fn refusal(&self) -> Option<&str> {
        self.refused.as_deref()
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
            refused: None,
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
    let joined = root.join(candidate);
    // The lexical check above is not containment on its own, and it only ever
    // looked like it because of something true one layer away: a sealed
    // release is extracted by `update::tree`, which skips every entry that is
    // not a regular file, so a release tree contains no symlinks and a path
    // with no `..` in it could not leave. A linked directory is somebody's
    // working tree and carries whatever a checkout carries — `node_modules`,
    // a `dist` whose assets are symlinked, a stray link to `$HOME`. Then
    // `/some-link/.ssh/id_ed25519` has no `..` in it, resolves straight
    // through, and is served same-origin with `/api` to the World's own
    // script.
    //
    // So the proof is made real rather than inherited: resolve both sides and
    // require the result to still be under the root. A miss is safe here — the
    // caller falls back to the release's SPA entry — so a path that cannot be
    // resolved is simply not read.
    let within = joined.canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    within
        .starts_with(&root)
        .then(|| std::fs::read(&within).ok())?
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
/// # The one thing that outranks it, and the one that deliberately does not
///
/// [`LINK_VAR`] serves a directory in place of the release for the life of the
/// process launched holding it. It is the development path, and it is scoped
/// the way a development path should be: it is not written down, so nothing
/// this device says about itself tomorrow depends on it.
///
/// There was briefly a *recorded* one beside it, and it is gone on purpose.
/// A Library row's claim — this is release 0.9.3, installed, verified — is the
/// only claim this client makes about what a person is running, and it is
/// worth exactly as much as the number of ways it can be false. Making it
/// overridable and then promising to draw the override everywhere is a
/// strictly weaker guarantee than not letting it be overridden, and it holds
/// only as long as every surface remembers.
///
/// So a directory somebody is working on does not get to wear an installed
/// World's identity. It is a World of its own — see the local World registry —
/// which is also what makes it something that can later be shared on a
/// channel of its own without ever being confused for a published release.
pub fn activate(worlds: &Path, world: &str) -> Source {
    activate_with(&std::env::var(LINK_VAR).unwrap_or_default(), worlds, world)
}

/// [`activate`], with the declaration handed in.
///
/// Split from the environment for the reason [`linked`] is: these tests share
/// a process, so a variable set in one is set in all of them, and the decision
/// this function makes — *which* source answers, and whether a refusal keeps
/// its reason — is the half worth asserting.
fn activate_with(declared: &str, worlds: &Path, world: &str) -> Source {
    match linked(declared, world) {
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
            return Source::refused(why);
        }
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

    /// The hole that opened the moment a link could point at a working tree.
    ///
    /// A sealed release has no symlinks — `update::tree` skips every entry
    /// that is not a regular file — so the lexical `..` check was containment
    /// by inheritance. A checkout carries links, and then a path with no `..`
    /// in it walks straight out and is served same-origin with `/api`.
    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_a_linked_directory_is_not_a_way_out_of_it() {
        let outside = tempfile::tempdir().expect("somewhere else on the disk");
        std::fs::write(outside.path().join("id_ed25519"), b"a private key")
            .expect("a secret beside the tree");
        let tree = bundle_with(&[("index.html", b"the World's page")]);
        std::os::unix::fs::symlink(outside.path(), tree.path().join("escape"))
            .expect("the kind of link a checkout carries");

        let source = Source::activated(tree.path().to_path_buf());
        assert!(
            source.read("/escape/id_ed25519").is_none(),
            "a symlink is not a hole: no `..` appears in this path and it still leaves the root"
        );
        assert!(
            source.read("/index.html").is_some(),
            "and the directory still serves its own pages"
        );
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

    /// Refusing is half of it; the refusal has to be carried. `activate` is
    /// where the reason either survives into something a surface can read or
    /// is dropped into a `tracing::error` that this process has no subscriber
    /// for — which is exactly how a typo came to be reported as a release with
    /// no entry document.
    #[test]
    fn a_refused_link_carries_its_reason_out_of_activate() {
        let worlds = bundle_with(&[]);
        let gone = worlds.path().join("was-never-here");
        let declared = format!("com.lait.issues={}", gone.display());
        let source = activate_with(&declared, worlds.path(), "com.lait.issues");
        assert!(source.bundle().is_none(), "a refusal serves nothing");
        let why = source.refusal().expect("a refusal explains itself");
        assert!(
            why.contains("was-never-here"),
            "the reason must name the directory: {why}"
        );
    }

    /// The ordinary absence stays ordinary: nothing linked, nothing installed,
    /// and so nothing to explain beyond the empty release itself.
    #[test]
    fn an_unlinked_world_refuses_nothing_and_explains_nothing() {
        let worlds = tempfile::tempdir().expect("an installations root");
        let source = activate_with("", worlds.path(), "com.lait.issues");
        assert!(source.bundle().is_none());
        assert!(
            source.refusal().is_none(),
            "an empty release is not a refused link"
        );
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
