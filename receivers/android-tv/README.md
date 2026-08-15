# Astrolabe receiver for Android TV

This is the production Android TV receiver for Astrolabe Display. The bundled
receiver UI performs the protocol-major-1 pairing ceremony, HMAC-authenticated
program and asset requests, bounded verification and staging, monotonic
playback, and explicit offline/source/stale/refusal states.

The TV never loads remote application code. `WebViewAssetLoader` gives the
bundled UI a fixed HTTPS origin, navigation is closed, cleartext traffic is
disabled, and the JavaScript bridge exposes only load/save/clear for an
AES-GCM credential encrypted by a non-exportable Android Keystore key. Backup
and device transfer are disabled for receiver identity.

## Build and qualify

Run `gradle :app:assembleDebug` (or the generated Gradle wrapper) in this
directory. For release, configure the Nixie Solutions LLC signing key outside
the repository and run `:app:bundleRelease`. Install on Android TV API 26 or
newer, then complete `../QUALIFICATION.md`, including DPAD-only operation,
process death, network loss, certificate failure, rotation, and revocation.

The application ID is `com.nixiesoftware.astrolabe`; confirm Play Console
ownership before the first published artifact.
