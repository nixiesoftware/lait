#!/bin/bash
# Build the Lait Signage Raspberry Pi 3 qualification image on macOS.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
cache="$here/.cache"
output="$cache/lait-signage-pi3.img"
network=""
ssh_key_path="${HOME}/.ssh/id_ed25519.pub"
base_image=""

usage() {
  echo "usage: $0 --network-config <file> [--ssh-key <public-key>] [--base-image <img>] [--output <img>]"
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --network-config) network=${2:?missing network config}; shift 2 ;;
    --ssh-key) ssh_key_path=${2:?missing SSH public key}; shift 2 ;;
    --base-image) base_image=${2:?missing base image}; shift 2 ;;
    --output) output=${2:?missing output image}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

[ "$(uname -s)" = Darwin ] || { echo "build.sh requires macOS hdiutil" >&2; exit 1; }
[ -f "$network" ] || { echo "--network-config must name a file" >&2; exit 1; }
[ -f "$ssh_key_path" ] || { echo "SSH public key not found: $ssh_key_path" >&2; exit 1; }
for tool in cargo cargo-zigbuild curl hdiutil python3 shasum tar xz; do
  command -v "$tool" >/dev/null || { echo "missing tool: $tool" >&2; exit 1; }
done

ssh_key=$(tr -d '\r\n' < "$ssh_key_path")
case "$ssh_key" in
  ssh-ed25519\ *|ssh-rsa\ *|ecdsa-sha2-*\ *) ;;
  *) echo "unsupported or malformed SSH public key" >&2; exit 1 ;;
esac

mkdir -p "$cache"
work=$(mktemp -d /tmp/lait-pi-build.XXXXXX)
mount_point="$work/bootfs"
attached=""
cleanup() {
  if [ -n "$attached" ]; then hdiutil detach "$attached" >/dev/null 2>&1 || true; fi
  rm -rf "$work"
}
trap cleanup EXIT INT TERM

if [ -z "$base_image" ]; then
  base_image="$cache/base.img"
  if [ ! -f "$base_image" ]; then
    echo "downloading Raspberry Pi OS Lite arm64"
    redirect=$(curl -fsSIL https://downloads.raspberrypi.com/raspios_lite_arm64_latest | awk 'tolower($1)=="location:"{url=$2} END{gsub(/\r/,"",url); print url}')
    [ -n "$redirect" ] || { echo "could not resolve Raspberry Pi OS URL" >&2; exit 1; }
    curl -fsSL "$redirect" -o "$cache/base.img.xz"
    curl -fsSL "$redirect.sha256" -o "$cache/base.img.xz.sha256"
    expected=$(awk 'NR==1{print $1}' "$cache/base.img.xz.sha256")
    actual=$(shasum -a 256 "$cache/base.img.xz" | awk '{print $1}')
    [ "$actual" = "$expected" ] || { echo "base image checksum mismatch" >&2; exit 1; }
    xz -dk "$cache/base.img.xz"
  fi
fi
[ -f "$base_image" ] || { echo "base image not found: $base_image" >&2; exit 1; }

echo "building arm64 display receiver"
(cd "$repo" && cargo zigbuild --release --locked --target aarch64-unknown-linux-gnu.2.28 -p astrolabe-display-reference --bins)

echo "fetching the signed stable Lait bootstrap"
curl -fsSL https://storage.googleapis.com/the-foundation-dist/channels/stable -o "$work/stable.json"
version=$(python3 - "$work/stable.json" <<'PY'
import base64, json, sys
envelope=json.load(open(sys.argv[1]))
print(json.loads(base64.b64decode(envelope["payload"]))["version"])
PY
)
release="https://storage.googleapis.com/the-foundation-dist/releases/$version"
curl -fsSL "$release/install.sh" -o "$work/install.sh"
curl -fsSL "$release/lait-aarch64-unknown-linux-gnu.tar.gz" -o "$work/lait.tar.gz"
sh -n "$work/install.sh"
expected=$(python3 - "$work/install.sh" <<'PY'
import re, sys
text=open(sys.argv[1]).read()
match=re.search(
    r"aarch64\s*\|\s*arm64\)\s*target='aarch64-unknown-linux-gnu'\s*digest='([0-9a-f]{64})'",
    text,
)
if not match:
    raise SystemExit("published installer has no aarch64 digest")
print(match.group(1))
PY
)
actual=$(shasum -a 256 "$work/lait.tar.gz" | awk '{print $1}')
[ -n "$expected" ] && [ "$actual" = "$expected" ] || { echo "Lait archive checksum mismatch" >&2; exit 1; }
mkdir -p "$work/release"
tar xzf "$work/lait.tar.gz" -C "$work/release"

payload="$work/payload"
COPYFILE_DISABLE=1 cp -R "$here/payload" "$payload"
cp "$repo/target/aarch64-unknown-linux-gnu/release/astrolabe-display-reference" "$payload/astrolabe-display-reference"
cp "$repo/target/aarch64-unknown-linux-gnu/release/shell" "$payload/astrolabe-display-shell"
cp "$work/release/lait-aarch64-unknown-linux-gnu/lait" "$payload/lait-bootstrap"
chmod 0755 "$payload/astrolabe-display-reference" "$payload/astrolabe-display-shell" "$payload/lait-bootstrap"
(cd "$payload" && find . -type f ! -name manifest.sha256 -print0 | sort -z | xargs -0 shasum -a 256 > manifest.sha256)

cat > "$work/user-data" <<EOF
#cloud-config
timezone: America/Chicago
keyboard:
  model: pc105
  layout: us
ssh_pwauth: false
disable_root: true
manage_etc_hosts: false
users:
- name: operator
  gecos: Lait operator
  groups: users,adm,dialout,audio,netdev,video,plugdev,input,gpio,spi,i2c,render,sudo
  shell: /bin/bash
  lock_passwd: true
  sudo: ALL=(ALL) NOPASSWD:ALL
  ssh_authorized_keys:
  - $ssh_key
runcmd:
- [sh, -c, "install -m 0755 /boot/firmware/lait-pi/lait-pi-provision /usr/local/sbin/lait-pi-provision"]
- [sh, -c, "install -m 0644 /boot/firmware/lait-pi/systemd/lait-pi-provision.service /etc/systemd/system/lait-pi-provision.service"]
- [sh, -c, "systemctl daemon-reload"]
- [sh, -c, "systemctl enable --now --no-block lait-pi-provision.service"]
- [sh, -c, "systemctl enable --now --no-block ssh.service || systemctl enable --now --no-block sshd.service || true"]
EOF
printf 'instance-id: lait-signage-pi3-%s\nlocal-hostname: lait-signage\n' "$(date -u +%Y%m%d%H%M%S)" > "$work/meta-data"

mkdir -p "$(dirname "$output")"
candidate="$work/lait-signage-pi3.img"
cp -c "$base_image" "$candidate"
mkdir -p "$mount_point"
attach_output=$(hdiutil attach -mountpoint "$mount_point" "$candidate")
attached=$(printf '%s\n' "$attach_output" | awk '/FDisk_partition_scheme/{print $1; exit}')
[ -n "$attached" ] || { echo "could not identify attached image device" >&2; exit 1; }

cp "$work/user-data" "$mount_point/user-data"
cp "$network" "$mount_point/network-config"
cp "$work/meta-data" "$mount_point/meta-data"
COPYFILE_DISABLE=1 cp -R "$payload" "$mount_point/lait-pi"

# A connector exists even with no monitor attached, so labwc and wayvnc can
# render the exact kiosk surface from the first boot.
config="$mount_point/config.txt"
grep -q '^arm_64bit=1' "$config" || printf 'arm_64bit=1\n' >> "$config"
grep -q '^disable_splash=' "$config" && sed -i '' 's/^disable_splash=.*/disable_splash=1/' "$config" || printf 'disable_splash=1\n' >> "$config"
grep -q '^hdmi_force_hotplug=' "$config" && sed -i '' 's/^hdmi_force_hotplug=.*/hdmi_force_hotplug=1/' "$config" || printf 'hdmi_force_hotplug=1\n' >> "$config"

cmdline="$mount_point/cmdline.txt"
line=$(cat "$cmdline")
line=$(printf '%s' "$line" | sed -E 's/console=tty1/console=tty3/')
for option in quiet loglevel=0 logo.nologo vt.global_cursor_default=0 systemd.show_status=0 consoleblank=0; do
  case " $line " in *" $option "*) ;; *) line="$line $option" ;; esac
done
printf '%s\n' "$line" > "$cmdline"

(cd "$mount_point/lait-pi" && shasum -a 256 -c manifest.sha256)
sync
hdiutil detach "$attached" >/dev/null
attached=""
mv "$candidate" "$output"
digest=$(shasum -a 256 "$output" | awk '{print $1}')
echo "built $output"
echo "sha256 $digest"
echo "operator key $(ssh-keygen -lf "$ssh_key_path" | awk '{print $2}')"
echo "Lait stable $version"
