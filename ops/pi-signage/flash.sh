#!/bin/bash
# Safely write and verify a Lait Signage image on macOS.
set -euo pipefail

image=${1:?usage: flash.sh <image> [/dev/diskN]}
disk=${2:-}
[ -f "$image" ] || { echo "image not found: $image" >&2; exit 1; }

if [ -z "$disk" ]; then
  count=$(diskutil list external physical | grep -cE '^/dev/disk[0-9]+')
  [ "$count" = 1 ] || { echo "expected exactly one external physical disk, found $count" >&2; diskutil list external physical; exit 1; }
  disk=$(diskutil list external physical | grep -oE '^/dev/disk[0-9]+' | head -1)
fi

case "$disk" in /dev/disk[0-9]*) ;; *) echo "invalid whole-disk target: $disk" >&2; exit 1 ;; esac
info=$(diskutil info "$disk")
printf '%s\n' "$info" | grep -qE 'Whole:[[:space:]]+Yes' || { echo "$disk is not a whole disk" >&2; exit 1; }
printf '%s\n' "$info" | grep -qE 'Removable Media:[[:space:]]+Removable' || { echo "$disk is not removable; refusing" >&2; exit 1; }
disk_bytes=$(printf '%s\n' "$info" | awk '/Disk Size/{gsub(/[()]/,"",$5); print $5}')
image_bytes=$(stat -f %z "$image")
[ "$disk_bytes" -ge "$image_bytes" ] || { echo "image does not fit on $disk" >&2; exit 1; }

echo "image:  $image ($image_bytes bytes)"
echo "target: $disk ($disk_bytes bytes, removable whole disk)"

askpass=$(mktemp /tmp/lait-flash-askpass.XXXXXX)
cleanup() { rm -f "$askpass"; }
trap cleanup EXIT INT TERM
printf '#!/bin/bash\nosascript -e '\''tell application "System Events" to display dialog "sudo password (flash Lait Signage SD card %s)" default answer "" with hidden answer with title "Lait Signage"'\'' -e '\''text returned of result'\''\n' "$disk" > "$askpass"
chmod 0700 "$askpass"

raw=/dev/r${disk#/dev/}
diskutil unmountDisk "$disk" >/dev/null
SUDO_ASKPASS="$askpass" sudo -A dd if="$image" of="$raw" bs=4m 2>&1 | tail -1
sync

offset=$(diskutil info "${disk}s2" | awk '/Partition Offset/{print $3}')
size=$(diskutil info "${disk}s2" | awk '/Disk Size/{gsub(/[()]/,"",$5); print $5}')
blocks=$((size / 4194304))
skip=$((offset / 4194304))
expected=$(dd if="$image" bs=4m skip="$skip" count="$blocks" 2>/dev/null | shasum -a 256 | cut -c1-16)
actual=$(SUDO_ASKPASS="$askpass" sudo -A dd if="$raw" bs=4m skip="$skip" count="$blocks" 2>/dev/null | shasum -a 256 | cut -c1-16)
[ "$actual" = "$expected" ] || { echo "verification failed: image $expected card $actual" >&2; exit 1; }
echo "verify: rootfs OK ($actual)"
diskutil eject "$disk"
