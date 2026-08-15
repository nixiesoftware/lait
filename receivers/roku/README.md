# Astrolabe receiver for Roku TV

This SceneGraph channel implements Astrolabe Display protocol major 1. It uses
Roku's random UUID source for receiver entropy, `roHMAC`/SHA-256 for the frozen
transcripts, `roDeviceCrypto` plus the transactional registry for receiver
identity, bootstrap-pinned or verified Web PKI HTTPS, authenticated range transfers, bounded asset
staging, digest and decoded-dimension checks, monotonic playback, and native
unassigned/offline/stale/revoked/refusal states.

There is no activation-code shortcut, demo playlist, catalog, browser, generic
RPC, or externally supplied media URL. Every displayed image comes from an
opaque assignment/revision-bound asset route and remains hidden until Roku's
decoder reports the authenticated dimensions.

## Build, sideload, and publish

1. Run `npm install`, `npm test`, and `npm run package:roku` here for
   BrightScript/XML compilation, receiver checks, and a sideloadable
   `dist/astrolabe-roku.zip`.
2. Enable developer mode on a Roku OS 12.5+ TV, create its developer password,
   then upload that zip through the device development web server.
3. Run the physical-device qualification matrix.
4. Package it with the Nixie Solutions LLC Roku developer ID and complete
   `../QUALIFICATION.md` before creating a Store release.

Signing and final `.pkg` creation require the target Roku device and publisher
developer key; they are intentionally not repository secrets.

`receiver-bootstrap.json` is the package's only coordinator provisioning
input. For a private self-hosted build, replace it with the setup JSON copied
from Astrolabe before packaging. The channel verifies the PEM's DER SHA-256 and
passes that exact PEM to `roUrlTransfer.SetCertificatesFile`; the default Store
configuration continues to use Roku's Web-PKI root bundle.
