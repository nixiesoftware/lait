# Installing Astrolabe

Astrolabe is the canonical desktop client. Its Tauri host, matching `lait`
sidecar, license, notices, and reviewed first-party World catalog ship as one
native installation. The catalog is declaration and artwork only; it carries
no World executable or application payload. After first install the host
follows its signed `stable` feed and stages verified updates itself, while each
World follows its own independently signed channel.

Release files live at:

```text
https://storage.googleapis.com/the-foundation-dist/releases/<version>/
```

## Windows x86_64

Download and run `astrolabe-<version>-setup.exe`. It installs per-user under
`%LOCALAPPDATA%\Programs\Astrolabe`, creates the Start Menu shortcut, registers
`lait:` links, and launches through the stable updater stub. It also prepends
its own `current` directory to the per-user `PATH`, so portable `lait mcp`
editor bindings resolve the release from this installer rather than requiring
a Cargo-installed copy. Open a new terminal after installation to use `lait`
there.

```powershell
$version = '<version>'
$installer = Join-Path $env:TEMP "astrolabe-$version-setup.exe"
Invoke-WebRequest "https://storage.googleapis.com/the-foundation-dist/releases/$version/astrolabe-$version-setup.exe" -OutFile $installer
Start-Process -FilePath $installer -Wait
```

Windows 11 already carries WebView2. On an older machine the installer invokes
Microsoft's WebView2 bootstrapper if needed.

## macOS Apple Silicon

Download `astrolabe-<version>.dmg`, open it, and drag `Astrolabe.app` onto the
Applications shortcut shown in the disk image:

```sh
version='<version>'
curl -fLO "https://storage.googleapis.com/the-foundation-dist/releases/$version/astrolabe-$version.dmg"
open "astrolabe-$version.dmg"
```

Then launch Astrolabe from `/Applications`. The disk image and app are Developer
ID signed, notarized, and stapled. The current canonical desktop target is Apple
Silicon (`arm64`); Intel Macs can still run the bare host but do not yet have a
canonical Astrolabe bundle.

## Linux x86_64

Download and extract
`astrolabe-<version>-x86_64-unknown-linux-gnu.tar.gz`, then run the root
`astrolabe` stub. Keep the directory intact; the `current/` tree is the client
and sidecar release that the stub swaps atomically.

The bundle is built on Ubuntu 24.04 and expects GTK 3, WebKitGTK 4.1, and
AppIndicator runtime libraries.

## Headless host

The desktop installer is preferred. An always-on seed operator can download the
matching host archive from our release directory without involving Cargo or a
package registry. Select the archive named for the machine's Rust target triple;
for example, on x86_64 Linux:

```sh
version='<version>'
curl -fLO "https://storage.googleapis.com/the-foundation-dist/releases/$version/lait-x86_64-unknown-linux-gnu.tar.gz"
tar xzf "lait-x86_64-unknown-linux-gnu.tar.gz"
./lait-x86_64-unknown-linux-gnu/lait --version
```

Windows uses `lait-x86_64-pc-windows-msvc.zip`; macOS has both
`lait-aarch64-apple-darwin.tar.gz` and `lait-x86_64-apple-darwin.tar.gz`.
Cargo remains a source-build tool for contributors, not an installation or
release channel.

## First launch

Astrolabe starts its matching local sidecar and opens its own native window.
A reviewed catalog entry can be present in the Library without being installed:
its action reads **Install**, reports **Installing** progress while its signed
independent release is fetched and verified, and changes to **Open** only after
an immutable installed-bundle record has been selected. Choose **Found a
space** to create one or **Use an invite** to join a teammate. Closing a World
stops its supervised runner; the World remains installed and can be launched
again without reinstalling it.
