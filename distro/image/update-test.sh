#!/usr/bin/env bash
# The live medium's updater end to end (update.sh), in QEMU with OVMF:
#
#   1. The live system boots from a small disk, as from a USB stick, whose
#      kernel command line carries its release's signed manifest (as the
#      boot menu of build-media.sh does), and installs itself on an empty
#      NVMe disk.
#   2. It boots from the stick again, with the installation's partitions now
#      seen by the kernel, and updates that installation from itself (the
#      boot menu's Update entry): slot b is written and goes on trial.
#   3. The NVMe disk boots alone, through the signed chain: GRUB starts slot b.
#
#   update-test.sh <payload> <initramfs> <openshell-ab> <output-dir>
#
# <payload> is the media payload (tools/qemu/run.sh, prepare_media_payload):
# the signed kernel and boot.cfg the initramfs carries, with the release key's
# public half. <initramfs> is a compat image of the media profile whose
# /test/boot_hello.sh is the probe update-boot.sh. SB_KEYS names the key
# folder (the Secure Boot key and the release key); OVMF_CODE and OVMF_VARS
# an OVMF with variables (Debian and Ubuntu: ovmf).
set -euo pipefail

[[ $# -eq 4 ]] || { sed -n '2,21p' "$0" >&2; exit 2; }
payload=$(realpath "$1") initramfs=$(realpath "$2") ab=$(realpath "$3") output=$4
here=$(cd "$(dirname "$0")" && pwd)
source "$here/secureboot.sh"
source "$here/channel.sh"
mkdir -p "$output"
work=$(mktemp -d "${TMPDIR:-/tmp}/openshell-update-test-XXXXXX")
trap 'rm -rf "$work"' EXIT
code=${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}
vars=${OVMF_VARS:-/usr/share/OVMF/OVMF_VARS_4M.fd}
release=$(cat "$payload/release")

# The release's signed manifest, as build-media.sh makes it for the medium.
mkdir -p "$work/release"
cp "$payload/kernel" "$payload/boot.cfg" "$work/release/"
ln -s "$initramfs" "$work/release/initramfs.cpio.gz"
channel_manifest "$work/release" "$ab" "${SB_KEYS:?the key folder}" "$release"
manifest="openshell.manifest=$(base64 -w0 "$work/release/MANIFEST") openshell.manifest_sig=$(base64 -w0 "$work/release/MANIFEST.sig")"

# The "stick": the disc GRUB on an ESP, the live release on a second partition.
mkdir -p "$work/esp/boot/grub" "$work/live"
sb_esp_files "$work/esp" "$SB_KEYS" disc
cp "$payload/kernel" "$work/live/kernel"
cp "$initramfs" "$work/live/initramfs.cpio.gz"
{
    echo 'set timeout=0'
    echo 'search --no-floppy --label --set=root OSN-LIVE'
    echo "linux (\$root)/kernel SHELL=/bin/sh LOGNAME=root HOME=/ USER=root PATH=/bin:/usr/bin loglevel=error earlycon console=hvc0 $manifest -- sh -l /test/boot_hello.sh"
    echo 'initrd ($root)/initramfs.cpio.gz'
    echo 'boot'
} > "$work/esp/boot/grub/grub.cfg"
mkfs.vfat -C -n OSN-ESP "$work/esp.img" $((32 * 1024)) > /dev/null
(cd "$work/esp" && mcopy -s -i "$work/esp.img" ./* ::/)
size=$(( $(du -sm --apparent-size "$work/live" | cut -f1) + 64 ))
mke2fs -q -t ext2 -L OSN-LIVE -d "$work/live" "$work/live.img" "${size}M"
truncate -s "$((size + 40))M" "$work/stick.img"
parted -s "$work/stick.img" mklabel gpt mkpart esp fat32 1MiB 33MiB set 1 esp on mkpart live ext2 33MiB "$((33 + size))MiB" 2> >(grep -v udevadm >&2)
dd if="$work/esp.img" of="$work/stick.img" bs=1M seek=1 conv=notrunc status=none
dd if="$work/live.img" of="$work/stick.img" bs=1M seek=33 conv=notrunc status=none
truncate -s 20G "$work/nvme.img"

qemu() {
    local log=$1
    shift
    cp "$vars" "$work/vars.fd"
    timeout --kill-after=15s "${OPENSHELL_UPDATE_TEST_TIMEOUT:-30m}" qemu-system-x86_64 \
        -machine q35 -cpu Icelake-Server,+x2apic -smp 2 -m 6G --no-reboot -display none \
        -drive "if=pflash,format=raw,unit=0,readonly=on,file=$code" -drive "if=pflash,format=raw,unit=1,file=$work/vars.fd" \
        -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
        -drive if=none,format=raw,id=nvme,file="$work/nvme.img" "$@" \
        -device virtio-serial-pci,disable-legacy=on,disable-modern=off -device virtconsole,chardev=console \
        -chardev file,id=console,path="$log" -serial file:"$log.serial" < /dev/null > "$log.qemu" 2>&1 || true
    tr -d '\r' < "$log" | grep -av '38;2' > "$log.txt" || true
}
stick() {
    qemu "$1" \
        -drive if=none,format=raw,id=stick,file="$work/stick.img" \
        -device virtio-blk-pci,drive=stick,bootindex=0,disable-legacy=on,disable-modern=off \
        -device nvme,drive=nvme,serial=osn-update-test
}

# 1. From the stick: install on the NVMe disk.
stick "$output/install.log"
if ! grep -q '^OPENSHELL_UPDATE_INSTALLED$' "$output/install.log.txt"; then
    echo 'update-test: the installation did not finish' >&2
    tail -n 40 "$output/install.log.txt" >&2
    exit 1
fi

# 2. From the stick again: update that installation.
stick "$output/update.log"
sed -n '/^== update/,/^OPENSHELL_UPDATE_DONE/p' "$output/update.log.txt"
if ! grep -q '^OPENSHELL_UPDATE_DONE$' "$output/update.log.txt" || grep -q '^OPENSHELL_UPDATE_FAILED$' "$output/update.log.txt" ||
    ! grep -q '^b: Trial(3) ' "$output/update.log.txt"; then
    echo 'update-test: the update did not put slot b on trial' >&2
    tail -n 40 "$output/update.log.txt" >&2
    exit 1
fi

# 3. The NVMe disk alone: GRUB boots the update from slot b, on trial.
qemu "$output/installed.log" -device nvme,drive=nvme,serial=osn-update-test,bootindex=0
if ! grep -aq 'OpenShell Native: booting b' "$output/installed.log.serial" ||
    ! grep -aq 'initramfs is ready' "$output/installed.log.txt"; then
    echo 'update-test: the installed disk did not boot slot b' >&2
    tail -c 3000 "$output/installed.log.serial" >&2
    exit 1
fi
echo OPENSHELL_NATIVE_UPDATE_OK
