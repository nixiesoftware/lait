# Lait Signage Raspberry Pi image

This builds a reproducible Raspberry Pi OS Lite arm64 qualification image for
a Raspberry Pi 3. The appliance runs all of these at once:

- the signed stable Lait host, including the Linux network plane and display
  coordinator;
- the native Astrolabe Display reference receiver;
- a `labwc`/`mpv` kiosk shell; and
- loopback-only `wayvnc` for observation through an SSH tunnel.

The image is created from a clean Raspberry Pi OS base. It does not preserve a
user or password from an earlier card. `build.sh` creates exactly one operator
account, locks password authentication, and installs the requested SSH public
key. Lait and the display receiver mint their own state on first boot.

Build on macOS:

```sh
ops/pi-signage/build.sh \
  --network-config /path/to/network-config \
  --ssh-key ~/.ssh/id_ed25519.pub
```

The default output is `.cache/lait-signage-pi3.img`. The build requires
`cargo-zigbuild` and Zig; Homebrew packages both. A cached uncompressed base
image can be supplied with `--base-image`.

Flash only after resolving the intended removable disk:

```sh
ops/pi-signage/flash.sh ops/pi-signage/.cache/lait-signage-pi3.img /dev/diskN
```

On first boot, provisioning installs the kiosk packages and resolves the
signed stable Lait channel. It may take several minutes and retries on later
boots until complete. The receiver waits for
`/boot/firmware/signage-bootstrap.json`.

Observe the node:

```sh
ssh operator@<pi-address> lait-pi-health
ssh operator@<pi-address> sudo journalctl -u lait-pi-provision -f
```

Or launch the localhost-only web panel. It discovers the Pi by proving that
the `operator` SSH key works, so another SSH or Lait node on the LAN cannot be
mistaken for this appliance:

```sh
python3 ops/pi-signage/monitor.py --open
```

The panel shows first-boot progress, temperatures and resource pressure,
service state, coordinator identity, receiver handoff, and bounded journal
tails. Its **Open live display** button starts the same loopback SSH tunnel
described below.

Observe the display remotely. `wayvnc` listens only inside the Pi, so the VNC
port is never exposed to the LAN:

```sh
ssh -N -L 5900:127.0.0.1:5900 operator@<pi-address>
open vnc://127.0.0.1:5900
```
