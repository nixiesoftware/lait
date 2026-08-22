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
| `.github/workflows/build-astrolabe.yml` | Automatic `workflow_run: ["Release"]` trigger removed; `workflow_dispatch` only. **No release ships from it.** |

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

Two things this file still records that the new path has not re-proved:

- **Windows.** `packaging/windows/astrolabe.nsi` installs `astrolabe.exe` as
  the *update stub* with the real client beside it. Tauri's NSIS target has no
  such shape, and reconciling them is a decision, not a port.
  `build-astrolabe.sh` refuses on Windows rather than emitting something
  unpublishable.
- **CI-side release building.** The quarantined workflow holds the Developer ID
  signing and notarization arrangement (its five repository secrets) and the
  SLSA provenance attestation. `build-astrolabe.sh` takes `--identity` and
  `--notarize` and does the signing half locally; a runner-side equivalent
  would inherit the rest from that file.

The one downstream consequence is already safe, and deliberately so:
`ci/publish-feed.sh --from-release <tag>` finds no `astrolabe-*` asset on a
release now, and **refuses** rather than quietly publishing an engine-only
feed — it makes you pass `--lait-only` to say you meant it. Nothing was changed
there; it was already built to fail loudly at exactly this.

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
