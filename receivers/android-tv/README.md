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

Run `./gradlew :app:assembleDebug` (`gradlew.bat` on Windows) in this
directory. For release, configure the Nixie Solutions LLC signing key outside
the repository and run `./gradlew :app:bundleRelease`. Install on Android TV API 26 or
newer, then complete `../QUALIFICATION.md`, including DPAD-only operation,
process death, network loss, certificate failure, rotation, and revocation.

`app/src/main/assets/receiver-bootstrap.json` is the only coordinator
provisioning input. The checked-in package uses the hosted Web-PKI origin. A
private/sideload build may replace it with Astrolabe's copied pinned bootstrap;
the native bridge validates the certificate PEM against its SHA-256, pins the
presented leaf, retains hostname verification, refuses redirects, and exposes
only bounded `/head/v1/` requests to the bundled surface. It never calls the
WebView SSL-error bypass API.

Live CMAF playback is available only when the coordinator has a Web-PKI
certificate that the WebView can validate. A private pinned-certificate build
therefore negotiates the Frame tier; the app-local Java trust manager cannot be
inherited safely by the WebView-owned WebSocket.

The application ID is `com.nixiesoftware.astrolabe`; confirm Play Console
ownership before the first published artifact.
