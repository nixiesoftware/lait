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

A Pi, a NAS, a VPS: an always-on device that holds your Spaces and never draws
a window. Download the host archive named for the machine's Rust target triple
and run its `install` mode as root; for example, on x86_64 Linux:

```sh
version='<version>'
curl -fLO "https://storage.googleapis.com/the-foundation-dist/releases/$version/lait-x86_64-unknown-linux-gnu.tar.gz"
tar xzf "lait-x86_64-unknown-linux-gnu.tar.gz"
sudo ./lait-x86_64-unknown-linux-gnu/lait install
```

A Raspberry Pi uses `lait-aarch64-unknown-linux-gnu.tar.gz`.

The binary you untarred is only the bootstrapper. `lait install` resolves the
`stable` channel through the same signed chain the daemon updates by — pointer,
manifest, size, digest — and installs *that* release's binary, never itself, so
a stale `version` in the line above is harmless and an unverifiable download is
a refusal rather than an install. It writes:

```text
/var/lib/lait/bin/lait              the proven binary, owned by the `lait` system user
/var/lib/lait/bin/installed.json    version, target, channel, unit
/var/lib/lait/…                     the identity, minted by the daemon at its first boot
/etc/systemd/system/lait.service    Restart=always, LAIT_SUPERVISED=1, LAIT_DISPLAY=off
```

then enables and starts the unit and prints what the daemon has to say — a
pairing code to enter in Astrolabe once that surface ships, or where to look.
From then on the daemon follows the channel itself: a published release is
proven, swapped over `bin/lait`, and the daemon exits for systemd to start the
new one. `journalctl -u lait -f` is its log; `systemctl status lait` its state.
After five failed starts in five minutes systemd leaves it down until
`systemctl reset-failed lait` or the install line is run again — re-running it
is also how a wedged box is repaired, since it installs whatever `stable`
proves by then.

Flags: `--user` installs under `~/.local/share/lait` with a user unit and no
root; `--channel test` follows the test channel, recorded beside the identity;
`--displays` leaves the display coordinator on (it binds port 7443);
`--root <dir>` picks another root. An install line never crosses a root that
was installed the other way.

Windows uses `lait-x86_64-pc-windows-msvc.zip`; macOS has both
`lait-aarch64-apple-darwin.tar.gz` and `lait-x86_64-apple-darwin.tar.gz`, and
neither has an install mode — the desktop client is the canonical install
there. Cargo remains a source-build tool for contributors, not an installation
or release channel.

## First launch

Astrolabe starts its matching local sidecar and opens its own native window.
A reviewed catalog entry can be present in the Library without being installed:
its action reads **Install**, reports **Installing** progress while its signed
independent release is fetched and verified, and changes to **Open** only after
an immutable installed-bundle record has been selected. Choose **Found a
space** to create one or **Use an invite** to join a teammate. Closing a World
stops its supervised runner; the World remains installed and can be launched
again without reinstalling it.
