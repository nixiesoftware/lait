# Astrolabe receiver for Apple TV

This native SwiftUI tvOS application implements Astrolabe Display protocol
major 1. It uses `SecRandomCopyBytes`, CryptoKit SHA-256/HMAC, a
ThisDeviceOnly Keychain item, an ephemeral no-cookie/no-cache URLSession that
refuses redirects and enforces response bounds while streaming, strict JSON
field validation, verified asset decode dimensions, monotonic playback, and
native pairing/unassigned/offline/stale/revoked/refusal UI. Eligible live
assignments use the coordinator's authenticated HLS-v3 MPEG-TS edge through
AVPlayer; the receiver never accepts a caller-supplied media URL.

It does not load a web view, remote code, arbitrary URL, catalog, World route,
or demo program. Its coordinator origin is fixed to the origin named in
`ReceiverBootstrap.json`, and every post-enrollment request consumes a
single-use challenge.

## Build and qualify

1. Install XcodeGen on a Mac and run `xcodegen generate` in this directory.
2. Select the Nixie Solutions LLC development team for
   `com.nixiesoftware.astrolabe` without checking credentials into the repo.
3. Run the `AstrolabeReceiverTests` scheme; it independently reproduces the
   Rust conformance fixture.
4. Build for a physical Apple TV running tvOS 17 or newer and complete
   `../QUALIFICATION.md` with the Siri Remote, VoiceOver, network interruption,
   process death, certificate failure, assignment rotation, and revocation.

The checked-in bootstrap uses `https://nixiesoftware.com`. A private build can
replace that resource with the pinned setup copied from Astrolabe. Its
`URLSessionDelegate` accepts only the exact bootstrapped leaf SHA-256 for the
exact origin host, while redirects and cross-origin requests remain closed.

App Store icon/top-shelf art and the final signing profile remain release
assets; the receiver implementation and project definition are complete here.
