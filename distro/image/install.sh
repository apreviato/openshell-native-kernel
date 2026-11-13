#!/bin/sh
# Installs the running live system on a disk, as root: the same layout as
# mkimage.sh (GPT with the ESP, system_a, system_b, recovery and data), with
# the signed boot chain, OpenShell Native's release in slot a, confirmed, and
# the same release as recovery. The installed system asks for a password the
# first time it starts (the session supervisor's sign-in).
#
#   openshell-native-install list                candidate disks, "<device> <bytes>" per line
#   openshell-native-install install <device>    progress as "PROGRESS <percent> <message>", then "DONE"
#
# The kernel cannot read the live medium after booting (no USB storage
# driver), so the release comes from memory: /dev/initrd is the initramfs the
# computer started from (kernel patch 0021), and /opt/openshell-media holds
# the signed kernel, the ESP's boot files, grub.cfg and the boot commands of
# the system and recovery slots, boot.cfg and recovery.cfg (tools/qemu/run.sh,
# with release.sh).
# The disk's partition table is never re-read: each filesystem is written at
# its partition's offset.
set -eu

media=${OPENSHELL_MEDIA:-/opt/openshell-media}
compat=/opt/openshell-compat
MIB=1048576
MIN_DISK=$((16 * 1024 * MIB))

progress() { echo "PROGRESS $1 $2"; }
fail() { echo "FAILED $*" >&2; exit 1; }

# Whole disks the kernel offers (NVMe and VirtIO), never their partitions.
disks() {
    for device in /dev/nvme[0-9]*n[0-9]* /dev/vd[a-z]; do
        case "$device" in *p[0-9]*) continue ;; esac
        [ -b "$device" ] || continue
        size=$(blockdev --getsize64 "$device" 2>/dev/null) || continue
        [ "$size" -ge "$MIN_DISK" ] && echo "$device $size"
    done
    return 0
}

uuid() {
    hex=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
    echo "$hex" | sed -E 's/^(.{8})(.{4}).(.{3}).(.{3})(.{12})$/\1-\2-4\3-a\4-\5/'
}

case ${1:-} in
list)
    disks
    ;;
install)
    device=${2:-}
    disks | grep -q "^$device " || fail "$device is not a disk to install to"
    [ -r /dev/initrd ] || fail "this system did not start from a live medium"
    for file in kernel release boot.cfg recovery.cfg boot/EFI/BOOT/BOOTX64.EFI boot/EFI/BOOT/grubx64.efi boot/EFI/BOOT/mmx64.efi grub.cfg; do
        [ -e "$media/$file" ] || fail "the live medium lacks $file"
    done
    release=$(cat "$media/release")
    work=$(mktemp -d /tmp/openshell-install.XXXXXX)
    trap 'rm -rf "$work"' EXIT

    progress 2 "Reading the system"
    mkdir -p "$work/slot" "$work/recovery" "$work/data/openshell/ab"
    cat /dev/initrd > "$work/slot/initramfs.cpio.gz"
    cp "$media/kernel" "$work/slot/kernel"
    cp "$media/boot.cfg" "$work/slot/boot.cfg"

    # The selector: the release in slot a, confirmed, and grubenv for GRUB.
    "$compat/openshell-ab" "$work/ab" install "$release" > /dev/null
    cp "$work/ab/selector.0" "$work/ab/selector.1" "$work/ab/grubenv" "$work/data/openshell/ab/" 2>/dev/null || true

    # Sizes in MiB: room in each slot for a release twice this size, the rest for data.
    disk_mib=$(($(blockdev --getsize64 "$device") / MIB))
    payload=$(( ($(du -sm "$work/slot" | cut -f1) + 1) ))
    esp=64 slot=$((payload * 2 + 256)) recovery=$((payload + 128))
    data=$((disk_mib - 2 - esp - 2 * slot - recovery))
    [ "$data" -ge 4096 ] || fail "the disk is too small for OpenShell and 4 GiB of data"

    progress 8 "Partitioning $device"
    start=1
    a=$((start + esp)) b=$((a + slot)) r=$((b + slot)) d=$((r + recovery)) end=$((d + data))
    # The table in a file of the disk's size, copied to the disk's first and last MiB.
    table="$work/table.img"
    truncate -s "$((disk_mib * MIB))" "$table"
    parted -s "$table" mklabel gpt \
        mkpart esp fat32 "${start}MiB" "${a}MiB" set 1 esp on \
        mkpart system_a ext2 "${a}MiB" "${b}MiB" \
        mkpart system_b ext2 "${b}MiB" "${r}MiB" \
        mkpart recovery ext2 "${r}MiB" "${d}MiB" \
        mkpart data ext2 "${d}MiB" "${end}MiB" 2> /dev/null
    dd if="$table" of="$device" bs=1M count=1 conv=notrunc status=none
    dd if="$table" of="$device" bs=1M skip=$((disk_mib - 1)) seek=$((disk_mib - 1)) count=1 conv=notrunc status=none
    rm -f "$table"

    # ext2 at a partition's offset, with its UUID chosen here for grub.cfg.
    ext2() {
        label=$1 offset=$2 size=$3 source=$4 id=$(uuid)
        mke2fs -q -F -t ext2 -i 65536 -L "$label" -U "$id" -E "offset=$((offset * MIB))" -d "$source" "$device" "$((size * 1024))" ||
            fail "writing $label"
        echo "set openshell_${label#osn-}_uuid=$id" >> "$work/grub.cfg"
    }
    : > "$work/grub.cfg"
    mkdir -p "$work/empty"
    progress 15 "Writing system A"
    ext2 osn-system_a "$a" "$slot" "$work/slot"
    progress 45 "Writing the recovery system"
    cp "$work/slot/kernel" "$work/slot/initramfs.cpio.gz" "$work/recovery/"
    cp "$media/recovery.cfg" "$work/recovery/boot.cfg"
    rm -rf "$work/slot"
    ext2 osn-recovery "$r" "$recovery" "$work/recovery"
    rm -rf "$work/recovery"
    progress 70 "Preparing system B for updates"
    ext2 osn-system_b "$b" "$slot" "$work/empty"
    progress 75 "Preparing the data partition"
    ext2 osn-data "$d" "$data" "$work/data"

    progress 92 "Writing the boot loader"
    cat "$media/grub.cfg" >> "$work/grub.cfg"
    mkdir -p "$work/esp/EFI/debian"
    cp -R "$media/boot/." "$work/esp/"
    cp "$work/grub.cfg" "$work/esp/EFI/debian/grub.cfg"
    mkfs.vfat -C -n OSN-ESP "$work/esp.img" $((esp * 1024)) > /dev/null
    (cd "$work/esp" && mcopy -s -i "$work/esp.img" ./* ::/)
    dd if="$work/esp.img" of="$device" bs=1M seek="$start" conv=notrunc status=none
    sync
    progress 100 "Installed"
    echo DONE
    ;;
*)
    sed -n '2,16p' "$0" >&2
    exit 2
    ;;
esac
