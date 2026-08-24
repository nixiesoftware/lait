# Packaging and distribution

The first-party feed is the only canonical distribution boundary. Installed
clients resolve a signed channel pointer from
`https://storage.googleapis.com/the-foundation-dist/channels/<channel>`, verify
the signed immutable manifest it names, and download artifacts directly from
the same bucket. They do not resolve a Git tag, GitHub Release, crates.io
package, Homebrew formula, Scoop bucket, or winget manifest.

## Build boundary

Before a release tag exists, dispatch `.github/workflows/release.yml` on the
PR branch. The workflow freezes its event SHA, refuses a workspace version
whose `vX.Y.Z` tag already exists, and invokes two repository-owned native
builders against that exact commit:

- `.github/workflows/build-binaries.yml` emits the five host archives expected
  by the feed.
- `.github/workflows/build-astrolabe.yml` emits the Windows installer, signed
  and notarized Apple Silicon macOS disk image, Linux bundle, and one updater
  tree for each supported client platform.

The final `candidate-X.Y.Z-<sha>` Actions artifact records and attests both its
workspace version and full source SHA. It is short-lived build transport, not
a release channel, and clients never read it.

Download that candidate and run the hostile legacy-layout audit and a rebuild
of a disposable copy of real user data. Iterate by building a new SHA-addressed
candidate; do not tag a failing candidate and do not move either channel. Only
after the exact candidate passes should `vX.Y.Z` be created at its recorded
SHA. Tagging does not rebuild the bytes.

## Promotion boundary

Download, seal, and publish a successful run to `test`:

```sh
ci/publish-feed.sh --from-run <run-id> \
  --artifact-name candidate-X.Y.Z-<sha> \
  --version X.Y.Z --channel test \
  --seed ~/.lait-feed-signing.seed
```

After verifying a real client against `test`, promote the same immutable bytes
to `stable`:

```sh
ci/publish-feed.sh --version X.Y.Z --channel stable --promote \
  --seed ~/.lait-feed-signing.seed
```

For a candidate run, the publisher verifies every artifact's GitHub build
provenance against the recorded SHA and refuses unless `vX.Y.Z` resolves to
that same audited commit. It uploads artifacts and their signed manifest
first, reads every object back through the public endpoint, and moves the
channel pointer last.
Release objects use long immutable caching; channel pointers use `no-cache`.
The feed signing seed remains on the maintainer machine performing promotion.

`packaging/build-astrolabe.sh` is also the local/recovery builder. It accepts a
release version and output directory, verifies that the bundled `lait` sidecar
reports that same version, and refuses unsupported client targets. On macOS a
feed-ready build additionally requires `--identity` and `--notarize`; an
unsigned local disk image is deliberately labelled unsafe to publish.

## World channels

Worlds do not ride host tags. `.github/workflows/publish-worlds.yml` builds
first-party World runners when their source or shared runtime changes and
assembles one commit-addressed `world-candidate-<short-sha>` artifact: the
native bundles, their SHA-256 sidecars, provenance attestations, and a
`world-candidate-provenance.env` recording the exact source coordinate. The
workflow moves no channel and holds no signing key — the candidate is the
immutable byte set the real-data audits exercise before anything ships.

Publication is a local act. `ci/publish-world.sh --from-run <run-id>
--artifact-name world-candidate-<short-sha>` downloads the audited run's
artifact, refuses it unless its recorded source SHA, checksums, provenance
attestations, and signing workflow identity all verify, and publishes only
previously unoccupied product versions to their own `test` channels. An
existing version is never rebuilt or overwritten: product versions must be
bumped before changed bytes can occupy a new immutable World release
coordinate. Stable World promotion (`--promote`) fetches each tested manifest
and moves only its signed pointer; it does not rebuild a runner.

Astrolabe's signed host package carries only a reviewed first-party World
catalog (declarations and artwork). A catalog row may be uninstalled. Choosing
Install resolves that World's signed channel, downloads the platform artifact,
verifies and atomically selects it, and only then makes Open available. World
runners and application assets never ride an Astrolabe host release.
