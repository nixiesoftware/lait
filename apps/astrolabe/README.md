# astrolabe

The Dart interface over the Rust core in [`tools/astrolabe`](../../tools/astrolabe).
Everything below `lib/src/core/client.dart` is Rust: process supervision,
control-protocol traffic, observation, and the single model of client state.
This half draws it and holds nothing but drafts.

## Two sibling checkouts, by path

`pubspec.yaml` depends on two packages that are not on pub.dev and are not
vendored. They are resolved **relative to this file** and must sit beside the
`lait` checkout, not inside it:

```
<parent>/
├── lait/          ← this repository
├── covalence/     ← gitlab.com/onnixi/mosaic_flutter  (the package is `covalence`)
└── lit-ui/        ← gitlab.com/onnixi/lit-ui
```

```sh
git clone https://gitlab.com/onnixi/mosaic_flutter.git covalence
git clone https://gitlab.com/onnixi/lit-ui.git lit-ui
```

The covalence repository is named `mosaic_flutter` and the directory is not,
which is the one thing about this that cannot be guessed from the manifest.
`covalence_lints` is a package *inside* covalence, so cloning the one gets both.

## Building and running

```sh
flutter pub get
flutter run -d macos       # or -d windows / -d linux
```

The Rust halves are built and staged by the platform build, not by hand — an
Xcode build phase (`macos/rust_build.sh`) on macOS and custom CMake targets on
Windows and Linux. All three produce the same two artifacts and put them beside
the executable, because that is where both consumers look:
`Client._core()` in Dart for the core library, and `sidecar::beside` in Rust for
the `lait` daemon.

| | macOS | Windows | Linux |
|---|---|---|---|
| The core | `libastrolabe.dylib` | `astrolabe.dll` | `libastrolabe.so` |
| The sidecar | `lait` | `lait.exe` | `lait` |
| Staged into | `astrolabe.app/Contents/MacOS/` | the bundle's lib directory | the bundle root |

A first build compiles the whole Rust workspace and takes on the order of twenty
minutes. Afterwards, `ASTROLABE_SKIP_SIDECAR=1` drops the sidecar's link from
the loop and keeps whatever `lait` is already staged — worth setting whenever
the change under test is Dart.

**Windows only:** close a running client before building. It holds both images
open and cargo cannot relink them; `rust_build.cmake` says so rather than
letting MSBuild report `MSB8066`. macOS has no such rule — the staging step
unlinks before it copies.

### Linux and WSL2

The release baseline is Ubuntu 24.04 x86_64 with Flutter **3.41.6**, the same
exact SDK pinned in the installer workflow. Install Flutter's documented Linux
desktop prerequisites plus the tray plugin's AppIndicator development package:

```sh
sudo apt-get install clang lld-18 cmake ninja-build pkg-config libgtk-3-dev \
  libstdc++-12-dev libayatana-appindicator3-dev
flutter doctor -v
flutter build linux --release
```

WSL2 is a supported validation host when WSLg exposes the `linux` device. Set a
Linux-only Cargo output directory when the checkout is also built from Windows,
so the two host toolchains never share fingerprints:

```sh
CARGO_TARGET_DIR=/path/to/lait-linux-target flutter build linux --release
```

The relocatable package is the whole Flutter bundle, not the runner alone:

```sh
bash ../../packaging/linux/make-tarball.sh \
  --bundle build/linux/x64/release/bundle \
  --version <version> --target x86_64-unknown-linux-gnu --out ../../dist
```

## Verifying

```sh
flutter analyze
flutter test
```

The tests drive real controls against `Client.canned` and read what each surface
*asked for* — no bridge, no core, no daemon and no window. That is the property
worth keeping from the retiring egui interface, and it is why the suite runs the
same on either platform.

The Library authors an MCP binding for the selected World (`LAIT_WORLD`).
The editor parents `lait mcp`; this client never holds that process.

## What differs on macOS

Ported deliberately, and stated here because the differences are decisions
rather than omissions:

* **The window controls are the app's own, on both platforms.** macOS keeps its
  traffic lights when a title bar is merely hidden, so `astrolabeWindowOptions`
  passes `windowButtonVisibility: false`. Two sets of controls would disagree
  about what close means — the caption's hides to the tray.
* **An owned window is not a child window.** Windows' `GWLP_HWNDPARENT` gives a
  window that stays above its owner and minimises with it while still being
  dragged anywhere; macOS's `addChildWindow` also drags the child whenever the
  parent moves. `macos/Runner/WindowChrome.swift` leaves an owned window
  independent rather than adopting the nearer-looking API.
* **The App Sandbox is off.** Under it, `$HOME` is the container, so the state
  root a sandboxed client manages is a different directory from the one the
  `lait` on a person's PATH uses — two clients on one machine, silently
  disagreeing about which devices exist. See the entitlements files.
* **The single-instance guard does not run.** `single_instance::acquire()` is
  called only by the retiring `astrolabe-egui` binary, on either platform, so
  the Flutter client can be launched twice today. The lock-file implementation
  for non-Windows already exists; nothing calls it yet.
