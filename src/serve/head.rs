//! Where a World's web head comes from: a downloaded bundle when one is
//! activated, the compiled-in tree otherwise (SUB-22).
//!
//! The embedded tree is the **floor**: compiled in, always present, always
//! the version this binary was built with, and therefore always compatible
//! with itself. A downloaded bundle sits over it, and the floor is what a
//! rollback returns to — a target that cannot be missing, because it cannot
//! be deleted.
//!
//! Only a bundle whose declared runtime version equals this build's is ever
//! activated. That check belongs to whoever activates ([`Source::activated`]);
//! by the time bytes are served the question is settled, which is what keeps
//! "a bundle newer than its host" from being a condition this path has to
//! handle.
//!
//! ## Traversal is ours to refuse now
//!
//! `include_dir` resolved against an embedded tree, so escaping it was not
//! possible and `shell` said so in as many words. A directory on disk has no
//! such property, so every lookup here refuses anything that is not a plain
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
    /// The compiled-in tree and nothing else — a build with no downloaded
    /// bundle, which is every build until one is published and staged.
    pub fn embedded() -> Self {
        Self { bundle: None }
    }

    /// Serve from `bundle` when it holds the asked-for path, else from the
    /// embedded floor.
    ///
    /// `runtime` is the bundle's declared runtime version and `expected` is
    /// this build's. They must be equal or the bundle is not activated at
    /// all: the mismatch is named here, once, and the floor keeps serving —
    /// never a partial activation, and never a silent one.
    pub fn activated(bundle: PathBuf, runtime: &str, expected: &str) -> Self {
        if runtime != expected {
            tracing::warn!(
                bundle = %bundle.display(),
                %runtime,
                %expected,
                "a World bundle targets another runtime version; serving the embedded head"
            );
            return Self::embedded();
        }
        Self {
            bundle: Some(bundle),
        }
    }

    /// The activated bundle's root, when the head is not serving the floor.
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
/// caller falls back to the embedded floor and then to the SPA entry — so the
/// refusal costs nothing and needs no separate error path.
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

/// The directory a staged World bundle is unpacked under, per World and
/// runtime version: `<identity>/heads/<runtime>/`.
///
/// Keyed by runtime version rather than by bundle version so a build only
/// ever finds bundles it can serve — the same "withheld by construction"
/// property the feed manifest gets from keying artifacts the same way, held
/// on this side too rather than trusted from the other.
pub fn bundles_root(identity: &Path) -> PathBuf {
    identity.join("heads")
}

/// Activate the staged bundle for this build's runtime version, when one is
/// staged.
///
/// Nothing here reads a manifest or verifies bytes: staging is what proves a
/// bundle, and a directory that is present under this runtime's name is one
/// that was proven when it landed. What this settles is only *which* source
/// a head serves from.
pub fn activate(bundles: &Path) -> Source {
    let runtime = crate::update::runtime::runtime_version();
    let candidate = bundles.join(&runtime);
    if candidate.is_dir() {
        tracing::info!(bundle = %candidate.display(), %runtime, "serving a staged World head");
        return Source::activated(candidate, &runtime, &runtime);
    }
    Source::embedded()
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

    #[test]
    fn an_activated_bundle_answers_and_an_absent_path_falls_through() {
        let dir = bundle_with(&[("index.html", b"<html>from the bundle</html>")]);
        let source = Source::activated(dir.path().to_path_buf(), "rt-a", "rt-a");
        assert_eq!(
            source.read("/index.html").as_deref(),
            Some(&b"<html>from the bundle</html>"[..])
        );
        assert!(
            source.read("/not-in-the-bundle.js").is_none(),
            "a path the bundle does not hold must fall through to the floor"
        );
    }

    /// The mismatch is settled at activation, not at every read: a bundle for
    /// another runtime never becomes the source at all, so no request can be
    /// served half from it.
    #[test]
    fn a_bundle_for_another_runtime_is_never_activated() {
        let dir = bundle_with(&[("index.html", b"the wrong runtime")]);
        let source = Source::activated(dir.path().to_path_buf(), "rt-other", "rt-mine");
        assert!(
            source.bundle().is_none(),
            "a mismatched bundle was activated"
        );
        assert!(source.read("/index.html").is_none());
    }

    /// `include_dir` made this impossible for free and the module it replaced
    /// said so. A directory on disk gives nothing for free.
    #[test]
    fn nothing_outside_the_bundle_is_readable_through_it() {
        let dir = bundle_with(&[("index.html", b"inside")]);
        let outside = dir.path().parent().expect("a parent").join("outside.txt");
        std::fs::write(&outside, b"not the bundle's to serve").expect("a file beside the bundle");
        let source = Source::activated(dir.path().to_path_buf(), "rt-a", "rt-a");

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

    #[test]
    fn a_root_with_no_bundle_for_this_runtime_serves_the_floor() {
        let identity = tempfile::tempdir().expect("an identity dir");
        let bundles = bundles_root(identity.path());
        assert!(
            activate(&bundles).bundle().is_none(),
            "an absent bundles directory activated something"
        );
        std::fs::create_dir_all(bundles.join("rt-some-other-runtime"))
            .expect("a bundle for another runtime");
        assert!(
            activate(&bundles).bundle().is_none(),
            "a bundle keyed to another runtime was activated"
        );
        let mine = bundles.join(crate::update::runtime::runtime_version());
        std::fs::create_dir_all(&mine).expect("a bundle for this runtime");
        assert_eq!(
            activate(&bundles).bundle(),
            Some(mine.as_path()),
            "the bundle for this runtime was not activated"
        );
    }

    #[test]
    fn the_embedded_floor_is_the_source_when_nothing_is_activated() {
        let source = Source::embedded();
        assert!(source.bundle().is_none());
        assert!(
            source.read("/index.html").is_none(),
            "the floor is read by the caller, not through the bundle path"
        );
    }

    /// Deleting a bundle is a supported act with a defined outcome: the floor
    /// serves. Nothing about it is an error, because the floor is compiled in
    /// and cannot be the thing that went missing.
    #[test]
    fn a_bundle_that_vanishes_underneath_falls_back_rather_than_failing() {
        let dir = bundle_with(&[("index.html", b"from the bundle")]);
        let root = dir.path().to_path_buf();
        let source = Source::activated(root.clone(), "rt-a", "rt-a");
        assert!(source.read("/index.html").is_some());
        drop(dir);
        assert!(
            source.read("/index.html").is_none(),
            "a vanished bundle must read as absent, not as an error"
        );
    }
}
