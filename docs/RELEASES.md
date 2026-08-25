# Release integrity

Lait and Astrolabe are distributed by the signed first-party feed at
`https://storage.googleapis.com/the-foundation-dist`. GitHub Actions builds
native artifacts, but Actions artifacts are staging inputs, not a channel and
not a location any installed client resolves.

## Trust chain

1. A reviewed commit is tagged `vX.Y.Z`; the tag must match the workspace
   version.
2. Repository-owned workflows build five host archives and native Astrolabe
   packages for Windows, Apple Silicon macOS, and x86_64 Linux.
3. The release gate requires every declared installer and updater tree and
   verifies each SHA-256 sidecar.
4. A maintainer downloads that complete build, seals its content hashes into an
   Ed25519-signed immutable manifest, uploads and reads back every object, then
   moves the signed channel pointer last.
5. Installed clients pin the feed public key, verify pointer and manifest
   signatures, enforce the compatibility floor, verify the selected artifact,
   and only then stage it.

Release objects are immutable and long-cached. `channels/test` and
`channels/stable` are the only mutable host objects and are served with
`no-cache`. Stable promotion fetches and verifies the exact manifest exercised
on `test`, then moves only the stable pointer; it never rebuilds or re-uploads
the release.

## Platform authenticity

- The macOS disk image and application are Developer ID signed, notarized, and
  stapled. The updater tree is packed from that same signed application.
- Windows currently relies on the signed feed and artifact hash; Authenticode
  is not yet enabled.
- Each native build receives a GitHub build-provenance attestation. This is an
  additional source/build claim, not the distribution trust anchor.

## World releases

Worlds have their own signed channel pointers and immutable manifests under
`channels/worlds/<world-id>/` and `releases/worlds/<world-id>/<version>/`.
A host tag never publishes or promotes a World. First-party World source
changes publish a new version to `test`; an already occupied version is never
rebuilt. Stable promotion fetches that exact tested manifest and is only a
signed pointer move.

## Consumer verification

The client performs the cryptographic checks automatically. For a manually
downloaded installer, compare it to the hash in the decoded, signature-verified
manifest rather than trusting an adjacent checksum fetched from the same
unauthenticated location.

GitHub provenance can additionally be checked with:

```sh
gh attestation verify ./artifact --repo nixiesoftware/lait
```

That verifies which repository workflow produced the bytes. It does not replace
the feed signature and does not establish that the software is safe.
