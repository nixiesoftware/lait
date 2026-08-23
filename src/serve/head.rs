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

/// Where staged World payloads live: `<identity>/worlds/<world>/current/`.
///
/// One directory per World, because two Worlds sharing one is a collision
/// that appears only after more than one is published — which is to say after
/// it would have been expensive to find.
pub fn worlds_root(identity: &Path) -> PathBuf {
    identity.join("worlds")
}

/// Serve a World's staged payload, when one is staged for it.
///
/// Nothing here reads a declaration or verifies bytes: staging is what proves
/// a bundle *and* what decides whether this build can run it, so a directory
/// present under a World's name is one that was proven and admitted when it
/// landed. What this settles is only *which* source a head serves from.
pub fn activate(worlds: &Path, world: &str) -> Source {
    if let Some(candidate) = crate::update::world::active_dir(worlds, world) {
        tracing::info!(bundle = %candidate.display(), %world, "serving a staged World payload");
        return Source::activated(candidate);
    }
    Source::unavailable()
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
        let worlds = worlds_root(identity.path());
        assert!(
            activate(&worlds, "world.a").bundle().is_none(),
            "an absent worlds directory activated something"
        );

        let a =
            crate::update::world::release_dir(&worlds, "world.a", "1.0.0").expect("a release path");
        std::fs::create_dir_all(&a).expect("a payload for world.a");
        std::fs::write(a.join("index.html"), b"a").expect("its entry");
        std::fs::write(
            crate::update::world::world_root(&worlds, "world.a").join("current.json"),
            serde_json::to_vec(&crate::update::world::StagedBundle {
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
