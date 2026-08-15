# Astrolabe webOS receiver

This is the production frame receiver for LG webOS 6.0+ (Chromium 79+) under
the Astrolabe Display protocol-major-1 contract. It does not contain a reviewer
demo, a fake pairing code, a World client, or a generic URL player.

The small package in `package/` is the LG hosted-app entry point. It redirects
only to `https://nixiesoftware.com/astrolabe/display/`. That deployment must
contain `hosted/` exactly, including the synchronized shared runtime.

On first launch the receiver:

1. verifies the fixed Web-PKI coordinator origin;
2. creates independent receiver-nonce and poll keys;
3. shows the six-word coordinator/receiver confirmation phrase and full
   fingerprint;
4. waits for both on-screen confirmation and authenticated Astrolabe approval;
5. stores the pending proof key before proving possession; and
6. commits only an enrolled receiver credential—not an assignment or any
   Space/World authority.

After enrollment it negotiates bounded frame capability, obtains single-use
challenges, authenticates every program/change/asset request with HMAC-SHA-256,
verifies the complete program revision, streams within the declared byte bound,
checks asset SHA-256 and decoded dimensions, atomically swaps its in-memory
staging set, advances with `performance.now()`, and applies native offline,
partial, unavailable, stale, revoked, unsupported, and re-pair states.

The proof key is encrypted with a non-extractable AES-GCM key kept in IndexedDB.
The exact persistence behavior and key backing remain a physical webOS 6.0+
qualification item; if IndexedDB, non-extractable key cloning, or Web Crypto is
unavailable, the receiver fails closed before pairing.

## Checks and packaging

```sh
npm install
npm test
npm run check:webos
npm run package:webos
```

`sync:runtime` mechanically copies `receivers/shared/web/*.mjs` into the hosted
deployment before every test/check/package command. The shared implementation
passes the Rust-owned language-neutral fixtures independently in Node.

The immutable LG app ID candidate is `com.nixiesoftware.app.astrolabe`; confirm
it before first publication. The runtime coordinator route is fixed at
`https://nixiesoftware.com/head/v1/` and credentials never appear in a URL.

Primary platform references:

- https://webostv.developer.lge.com/develop/tools/cli-dev-guide
- https://webostv.developer.lge.com/develop/getting-started/web-app-types
- https://webostv.developer.lge.com/develop/references/appinfo-json
- https://webostv.developer.lge.com/distribute/app-self-checklist
