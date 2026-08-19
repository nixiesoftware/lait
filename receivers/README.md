# Astrolabe television receivers

These are production receivers for the Astrolabe Display protocol, not sample
players or application clients:

| Target | Implementation | Protected identity | Local verification |
| --- | --- | --- | --- |
| Reference/native shell | Rust executable + atomic presentation handoff | Mechanics private secret store (DPAPI-bound on Windows, owner-only on Unix) | `cargo test -p astrolabe-display-reference && cargo clippy -p astrolabe-display-reference --all-targets` |
| LG webOS | hosted web app + packaged store launcher | non-extractable Web Crypto key in IndexedDB | `cd webos && npm test && npm run check:webos` |
| Samsung Tizen | packaged TV web app | Tizen KeyManager, rotating generations | `cd tizen && npm test` |
| Android TV | bundled WebView surface + native bridge | Android Keystore AES-GCM | `cd android-tv && node --test && .\\gradlew.bat :app:assembleDebug` |
| Roku TV | native SceneGraph/BrightScript | roDeviceCrypto + transactional registry | `cd roku && npm test` |
| Apple TV | native SwiftUI | ThisDeviceOnly Keychain | `cd tvos && npm test`, then XCTest on macOS |

All six implement the same closed protocol-major-1 contract in
`../crates/display-protocol`. A receiver can enroll, authenticate, negotiate a
narrower playback capability, obtain only its exact assignment, verify and
stage opaque frame assets or assignment-bound live grants, advance with
monotonic time, report bounded health, and render native
trust/source/delivery/refusal states. webOS, Tizen, and Web-PKI Android TV
builds consume the coordinator's bounded CMAF stream through MSE. Roku and tvOS
consume its real HLS-v3 MPEG-TS edge through their native video stacks. A
receiver cannot name a World, Space, surface, operation, acting identity,
filesystem path, external media URL, or product route.

The current delivery order is the native desktop/reference receiver first and
LG webOS second. Android TV, Roku, Tizen, and tvOS remain maintained
conformance implementations; production qualification and distribution for
those platforms are deferred until the desktop and webOS release gates hold.

The reference receiver consumes the pinned self-hosted bootstrap copied from
Astrolabe Displays and verifies that exact certificate before it sends pairing
material. Android TV, Apple TV, and Roku use that same bootstrap contract in
their native transports: Android and Apple hash the presented leaf, while Roku
validates the bootstrap PEM/fingerprint pair and installs that PEM as the
request-local CA file. Their checked-in bootstrap selects the hosted Web-PKI
origin; a private/sideload build can replace that one JSON resource with the
object copied from Astrolabe.

Android's dynamically pinned bootstrap continues to protect every HTTPS
request, but the WebView-owned WebSocket cannot inherit the app-local Java trust
manager. Such a build therefore negotiates the Frame tier instead of claiming a
live decoder it cannot open. Android live playback requires the coordinator's
Web-PKI origin, matching webOS and Tizen; it still talks directly to the
Astrolabe coordinator and does not require a media cloud.

LG webOS and Samsung Tizen remain Web-PKI receivers. Their web engines do not
offer an application-scoped safe override for an untrusted LAN certificate, so
self-hosted deployments route those two packages through a publicly trusted
coordinator endpoint. Web receivers require CORS to allow `POST`/`GET`, the
protocol request headers listed in `../crates/display-protocol/PROTOCOL.md`, and to
expose `X-Astrolabe-Next-Challenge`. Every transport rejects redirects.

Generated packages, build directories, signing keys, publisher credentials,
and device passwords must stay outside source control. Complete
`QUALIFICATION.md` on physical televisions before any store or fleet rollout.
