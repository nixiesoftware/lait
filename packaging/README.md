# Packaging and distribution

The first-party feed is the only canonical distribution boundary. Installed
clients resolve a signed channel pointer from
`https://storage.googleapis.com/the-foundation-dist/channels/<channel>`, verify
the signed immutable manifest it names, and download artifacts directly from
the same bucket. They do not resolve a Git tag, GitHub Release, crates.io
package, Homebrew formula, Scoop bucket, or winget manifest.

## Build boundary

Pushing `vX.Y.Z` runs `.github/workflows/release.yml`. The workflow verifies
that the tag matches the workspace version and invokes two repository-owned
native builders:

- `.github/workflows/build-binaries.yml` emits the five host archives expected
  by the feed.
- `.github/workflows/build-astrolabe.yml` emits the Windows installer, signed
  and notarized Apple Silicon macOS disk image, Linux bundle, and one updater
  tree for each supported client platform.

The final `release-X.Y.Z` Actions artifact is short-lived build transport. It
is not a release channel and clients never read it.

## Promotion boundary

Download, seal, and publish a successful run to `test`:

```sh
ci/publish-feed.sh --from-run <run-id> --version X.Y.Z --channel test \
  --seed ~/.lait-feed-signing.seed
```

After verifying a real client against `test`, promote the same immutable bytes
to `stable`:

```sh
ci/publish-feed.sh --from-run <run-id> --version X.Y.Z --channel stable \
  --seed ~/.lait-feed-signing.seed
```

The publisher uploads artifacts and their signed manifest first, reads every
object back through the public endpoint, and moves the channel pointer last.
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
publishes them to their own `test` channels. Stable World promotion is an
explicit manual dispatch. Product versions must be bumped before changed bytes
can occupy a new immutable World release coordinate.
