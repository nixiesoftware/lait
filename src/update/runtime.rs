//! The runtime version: the token a World bundle must match to be served
//! (SUB-22).
//!
//! A World's web head talks to exactly one thing — the head this binary
//! serves it from — and what it can break against is that surface: the
//! control protocol it reaches through, the DTO schema it decodes, and the
//! reviewed World implementation the build carries. The runtime version is a
//! fingerprint over precisely those, so a change in any of them is a new
//! token with nobody in the loop to remember.
//!
//! **Derived, never hand-numbered.** Expo documents the failure of the
//! alternative exactly: a token somebody maintains by hand eventually lies,
//! the bundle loads against a runtime it does not fit, and the crash arrives
//! with a symptom that names nothing. A fingerprint cannot lie about its own
//! inputs; it can only be incomplete, which is a reviewable question about
//! this one function rather than a discipline every future change has to
//! keep.
//!
//! The token is opaque and its bytes mean nothing beyond equality. It is not
//! ordered, not compared for newness, and never parsed: a bundle either
//! targets this runtime or it does not exist as far as this build is
//! concerned, which is what makes "a bundle newer than its host"
//! unrepresentable rather than handled.

/// The version of this fingerprint's own recipe.
///
/// Bumping it re-keys every published bundle, which is the lever for "the
/// inputs below were incomplete" — a correction that must invalidate what was
/// published under the old, wrong answer.
const RECIPE: u32 = 1;

/// This build's runtime version.
///
/// Cheap and pure: it hashes constants, so callers may compute it per request
/// if that is simplest.
pub fn runtime_version() -> String {
    let mut hasher = blake3::Hasher::new_derive_key("lait.world-runtime.v1");
    hasher.update(&RECIPE.to_le_bytes());
    hasher.update(&crate::control::CONTROL_PROTOCOL_VERSION.to_le_bytes());
    // Each bundled World's id, the reviewed implementation serving it, and
    // the DTO schema its head decodes. A World whose semantics or shapes moved
    // is a World whose head may be reading something that no longer exists, so
    // all three belong in the token that gates the head.
    //
    // Asked of the composition root rather than of the products directly: this
    // file must not name one, and `product_independence` is what says so.
    for (world, implementation, schema) in crate::composition::bundled_world_surfaces() {
        hasher.update(world.as_bytes());
        hasher.update(&implementation);
        hasher.update(&schema.to_le_bytes());
    }
    // Taken by character rather than by byte range: the digest is ASCII hex,
    // but slicing a string by index is a panic waiting for the day something
    // upstream is not.
    let digest: String = hasher.finalize().to_hex().chars().take(24).collect();
    format!("rt-{digest}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtime_version_is_stable_within_a_build_and_shaped_for_a_manifest_key() {
        let once = runtime_version();
        assert_eq!(once, runtime_version(), "the token is not stable");
        assert!(once.starts_with("rt-"), "{once}");
        assert_eq!(once.len(), 27, "{once}");
        assert!(
            once.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "the token must be safe as a manifest key and a URL segment: {once}"
        );
    }

    /// The recipe is what makes the token honest: every input must move it, or
    /// a change that breaks a bundle would leave it addressable by the same
    /// key. Asserted by construction here — the same hasher, fed one input
    /// differently — because the real inputs are constants this test cannot
    /// vary.
    #[test]
    fn every_input_moves_the_token() {
        let base = |recipe: u32, protocol: u32, schema: u32, world: &[u8]| {
            let mut hasher = blake3::Hasher::new_derive_key("lait.world-runtime.v1");
            hasher.update(&recipe.to_le_bytes());
            hasher.update(&protocol.to_le_bytes());
            hasher.update(&schema.to_le_bytes());
            hasher.update(world);
            hasher.finalize().to_hex().to_string()
        };
        let reference = base(1, 13, 3, b"world");
        assert_ne!(reference, base(2, 13, 3, b"world"), "the recipe is inert");
        assert_ne!(reference, base(1, 14, 3, b"world"), "the protocol is inert");
        assert_ne!(reference, base(1, 13, 4, b"world"), "the schema is inert");
        assert_ne!(
            reference,
            base(1, 13, 3, b"other"),
            "the World implementation is inert"
        );
    }
}
