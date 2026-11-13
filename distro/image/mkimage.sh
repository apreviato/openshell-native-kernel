#!/usr/bin/env bash
# The OpenShell Native disk: GPT with an EFI system partition (the signed
# boot chain of secureboot.sh and grub.cfg), the system slots system_a and
# system_b, recovery and data.
#
#   mkimage.sh <disk-dir> <recovery-dir> <output.img> [data-dir]
#
# <disk-dir> is a directory that openshell-ab manages (distro/ab): its
# slot-a/ and slot-b/ become the system partitions, and the selector with
# grubenv goes to data:/openshell/ab. Without them, GRUB boots the recovery.
# <recovery-dir> holds the recovery release, and [data-dir] more of data.
# Each release is kernel, initramfs.cpio.gz and boot.cfg, the GRUB commands
# that boot it (grub.cfg sources the chosen slot's).
#
# No root and no loop devices: each filesystem is built as a file (mke2fs -d,
# mtools) and copied into its partition. SB_KEYS names the folder of the
# Secure Boot key, whose certificate the ESP carries for MokManager.
set -euo pipefail

[[ $# -eq 3 || $# -eq 4 ]] || { sed -n '2,18p' "$0" >&2; exit 2; }
disk=$1 recovery=$2 output=$3 extra=${4:-}
here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d "${TMPDIR:-/tmp}/openshell-image-XXXXXX")
trap 'rm -rf "$work"' EXIT

source "$here/secureboot.sh"

# Partitions in MiB: ESP, the two slots and recovery the size of the largest release, then data.
mib() { du -sm --apparent-size "$1" | cut -f1; }
largest=1
for release in "$disk/slot-a" "$disk/slot-b" "$recovery"; do
    [[ -d "$release" ]] && (( $(mib "$release") > largest )) && largest=$(mib "$release")
done
esp=32 slot=$(( largest + largest / 5 + 16 )) data=${OPENSHELL_IMAGE_DATA_MIB:-64}
if [[ -n "$extra" ]]; then
    payload=$(mib "$extra")
    data=$((data + payload + payload / 5))
fi
names=(esp system_a system_b recovery data)
sizes=("$esp" "$slot" "$slot" "$slot" "$data")
labels=("" osn-system_a osn-system_b osn-recovery osn-data)

ext2() {
    local label=$1 source=$2 size=$3
    mke2fs -q -t ext2 -L "$label" -d "$source" "$work/$label.img" "${size}M"
}
mkdir -p "$work/empty" "$work/data/openshell/ab"
if [[ -n "$extra" ]]; then cp -R "$extra/." "$work/data/"; fi
for name in selector.0 selector.1 grubenv; do
    if [[ -f "$disk/$name" ]]; then cp "$disk/$name" "$work/data/openshell/ab/"; fi
done
ext2 osn-system_a "$( [[ -d "$disk/slot-a" ]] && echo "$disk/slot-a" || echo "$work/empty")" "$slot"
ext2 osn-system_b "$( [[ -d "$disk/slot-b" ]] && echo "$disk/slot-b" || echo "$work/empty")" "$slot"
ext2 osn-recovery "$recovery" "$slot"
ext2 osn-data "$work/data" "$data"

# Bind the EFI loader to the filesystems created for this image.
for label in "${labels[@]:1}"; do
    uuid=$(LC_ALL=C dumpe2fs -h "$work/$label.img" 2>/dev/null | awk '/^Filesystem UUID:/ { print $3 }')
    test -n "$uuid"
    printf 'set openshell_%s_uuid=%s\n' "${label#osn-}" "$uuid"
done > "$work/grub.cfg"
cat "$here/grub.cfg" >> "$work/grub.cfg"
# Debian's GRUB for disks reads /EFI/debian/grub.cfg of the ESP it booted
# from, and never a live stick's menu.
sb_esp_files "$work/esp" "${SB_KEYS:?the Secure Boot key folder}" disk
mkdir -p "$work/esp/EFI/debian"
cp "$work/grub.cfg" "$work/esp/EFI/debian/grub.cfg"
mkfs.vfat -C -n OSN-ESP "$work/esp.img" $(( esp * 1024 )) > /dev/null
(cd "$work/esp" && mcopy -s -i "$work/esp.img" ./* ::/)

# The disk: 1 MiB for the GPT, the partitions, 1 MiB for the backup GPT.
total=2
for size in "${sizes[@]}"; do total=$(( total + size )); done
rm -f "$output"
truncate -s "${total}M" "$output"
# parted asks udev to reread partitions, which a plain file does not need.
parted() { command parted "$@" 2> >(grep -v udevadm >&2); }
parted -s "$output" mklabel gpt
start=1
for index in "${!names[@]}"; do
    end=$(( start + sizes[index] ))
    parted -s "$output" mkpart "${names[index]}" "${start}MiB" "${end}MiB"
    if (( index == 0 )); then
        parted -s "$output" set 1 esp on
        image="$work/esp.img"
    else
        image="$work/${labels[index]}.img"
    fi
    dd if="$image" of="$output" bs=1M seek="$start" conv=notrunc status=none
    start=$end
done
echo "mkimage: $output (${total} MiB): ${names[*]}"
