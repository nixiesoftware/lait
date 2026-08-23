# The Flutter client is deprecated

**Tauri (`apps/astrolabe-web`) is the canonical and only live Astrolabe
interface.** This project is kept for reference and history. Nothing builds it,
nothing tests it, nothing ships it, and no change elsewhere in the repository is
obliged to keep it working.

## It does not currently build

Its pinned private `lit-ui` commit no longer carries a `pubspec.yaml`, so
`flutter pub get` fails before analysis, tests, or any platform build:

```
Could not find a file named "pubspec.yaml" in
https://gitlab.com/onnixi/lit-ui.git 1dbadc06…
```

That is a statement of fact, not a task. Reviving this project means resolving
that pin first, and then everything under "What was unwired" below.

## What was unwired

| Wire | What happened |
|---|---|
| `flutter_rust_bridge` dependency in `tools/astrolabe` | Removed. It is out of `Cargo.lock` and out of `THIRD-PARTY-NOTICES.md`. |
| `tools/astrolabe/src/frb_generated.rs` (4,615 generated lines) | Deleted. Every Rust build was compiling it. |
| `#[frb(sync)]` / `#[frb(ignore)]` on `api/mod.rs` | Removed. The boundary carries no generator annotations. |
| `api::watch(StreamSink<ClientView>)` | Removed. `api::subscribe` — the native callback path Tauri uses — is the only way to watch the view stream. |
| `ci/bridge-drift.sh` | Deleted. There is no generated binding left to drift. |
| `ci/dart-licences.sh` | Kept, unwired, and marked deprecated in its own header. |
| The Flutter jobs in `.github/workflows/build-astrolabe.yml` | Replaced by native Tauri jobs. The workflow again follows a successful tagged host release. |

The generated Dart under `lib/src/bridge/` is left exactly as it was on the day
this was written. It is now a snapshot of a boundary that has since moved: the
Rust half no longer regenerates, so those files will disagree with
`tools/astrolabe/src/api/mod.rs` more with every change. Do not read them as
current.

## What replaced it

`packaging/build-astrolabe.sh` — Tauri builds the installer, and the **feed**
distributes it. Nothing ships through a git forge any more; installed machines
follow a signed channel pointer on the dist host. Tauri's bundler is enabled,
the sidecar rides inside via `bundle.externalBin`, `mainBinaryName` is
`astrolabe` so `update::custody_of` finds it, and the script asserts the pair
by running the bundled sidecar and comparing its version to the release. macOS
is proven end to end: `.app`, `.dmg`, and the `astrolabe-tree-…` artifact that
`packaging/make-tree.sh` accepted from the Tauri bundle unchanged.

The replacement path keeps the two release properties the Flutter pipeline once
proved:

- **Windows.** ~~Reconciling Tauri's NSIS target with the stub layout is a
  decision, not a port.~~ Decided: the evergreen design forbids Tauri's
  install-to-update model (an update must never force a restart), so the stub
  layout is the only installed shape, Tauri's bundler installers never ship,
  and `astrolabe.nsi` — first install only — was ported to carry the Tauri
  pair. `build-astrolabe.sh` builds it.
- **CI-side release building.** Native Tauri jobs call the same script on
  Windows, Apple Silicon macOS, and Linux. The macOS job imports the existing
  Developer ID and notarization credentials; all three attach provenance for
  the installer built from the release tag.

`ci/publish-feed.sh --from-run <run-id> --version <version>` is the canonical
feed promotion path. The run's assembled artifact is transient build transport;
the signed GCS feed is the distribution. The publisher refuses an incomplete
release rather than quietly publishing an engine-only feed, and it cannot source
bytes from a GitHub Release.

## Why Flutter was left behind

Recorded plainly, because "we changed our minds" is not a reason anyone can act
on later. The Tauri host destructures `api::ClientView` exhaustively, so a field
added to the boundary stops compiling until somebody decides what the client
does with it. Flutter's half was generated, and its drift check
(`ci/bridge-drift.sh`) was real, worked, and was wired to no workflow — so a
change to `api/mod.rs` with no regeneration produced a Dart binding that
compiled, ran, and disagreed with the model, with no machine to say so. One
interface is checked by a compiler; the other was checked by remembering. That
gap, plus the cost of keeping a second interface compiling against every model
change, is the whole argument.
