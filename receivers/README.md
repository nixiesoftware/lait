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

All five implement the same closed protocol-major-1 contract in
`../crates/display-protocol`. A receiver can enroll, authenticate, negotiate a
narrower frame capability, obtain only its exact assignment, verify and stage
opaque assets, advance with monotonic time, report bounded health, and render
native trust/source/delivery/refusal states. It cannot name a World, Space,
surface, operation, acting identity, filesystem path, external URL, or product
route.

The reference receiver consumes the pinned self-hosted bootstrap copied from
Astrolabe Displays and verifies that exact certificate before it sends pairing
material. The five television packages are still configured for the hosted
Web-PKI origin while their platform-native pinned transports are completed;
they must not be represented as compatible with Astrolabe's self-signed LAN
certificate yet. Web receivers require CORS to allow `POST`/`GET`, the protocol
request headers listed in `../crates/display-protocol/PROTOCOL.md`, and to
expose `X-Astrolabe-Next-Challenge`. Every transport rejects redirects.

Generated packages, build directories, signing keys, publisher credentials,
and device passwords must stay outside source control. Complete
`QUALIFICATION.md` on physical televisions before any store or fleet rollout.
