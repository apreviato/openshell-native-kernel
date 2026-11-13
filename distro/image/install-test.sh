#!/usr/bin/env bash
# The live medium's installer end to end (install.sh), in QEMU with OVMF:
#
#   1. The live system boots from a small disk, as from a USB stick, with an
#      empty NVMe disk; it installs itself there (as root, as the session
#      supervisor runs it) and powers off.
#   2. The NVMe disk boots alone, through the signed chain: GRUB starts slot a
#      with the installed release, from the initramfs the installer copied.
#
#   install-test.sh <kernel.efi> <initramfs> <output-dir>
#
# <initramfs> is a compat image of the media profile: /opt/openshell-media and
# openshell-native-install. Its /test/boot_hello.sh is the probe
# install-boot.sh. SB_KEYS names the Secure Boot key folder; OVMF_CODE and
# OVMF_VARS an OVMF with variables (Debian and Ubuntu: ovmf).
set -euo pipefail

[[ $# -eq 3 ]] || { sed -n '2,16p' "$0" >&2; exit 2; }
kernel=$(realpath "$1") initramfs=$(realpath "$2") output=$3
here=$(cd "$(dirname "$0")" && pwd)
source "$here/secureboot.sh"
mkdir -p "$output"
work=$(mktemp -d "${TMPDIR:-/tmp}/openshell-install-test-XXXXXX")
trap 'rm -rf "$work"' EXIT
code=${OVMF_CODE:-/usr/share/OVMF/OVMF_CODE_4M.fd}
vars=${OVMF_VARS:-/usr/share/OVMF/OVMF_VARS_4M.fd}

# The "stick": the disc GRUB on an ESP, the live release on a second partition.
mkdir -p "$work/esp/boot/grub" "$work/live"
sb_esp_files "$work/esp" "${SB_KEYS:?the Secure Boot key folder}" disc
sb_sign "$SB_KEYS" "$kernel" "$work/live/kernel"
cp "$initramfs" "$work/live/initramfs.cpio.gz"
cat > "$work/esp/boot/grub/grub.cfg" <<'EOF'
set timeout=0
search --no-floppy --label --set=root OSN-LIVE
linux ($root)/kernel SHELL=/bin/sh LOGNAME=root HOME=/ USER=root PATH=/bin:/usr/bin loglevel=error earlycon console=hvc0 -- sh -l /test/boot_hello.sh
initrd ($root)/initramfs.cpio.gz
boot
EOF
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
    timeout --kill-after=15s "${OPENSHELL_INSTALL_TEST_TIMEOUT:-30m}" qemu-system-x86_64 \
        -machine q35 -cpu Icelake-Server,+x2apic -smp 2 -m 6G --no-reboot -display none \
        -drive "if=pflash,format=raw,unit=0,readonly=on,file=$code" -drive "if=pflash,format=raw,unit=1,file=$work/vars.fd" \
        -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
        -drive if=none,format=raw,id=nvme,file="$work/nvme.img" "$@" \
        -device virtio-serial-pci,disable-legacy=on,disable-modern=off -device virtconsole,chardev=console \
        -chardev file,id=console,path="$log" -serial file:"$log.serial" < /dev/null > "$log.qemu" 2>&1 || true
    tr -d '\r' < "$log" | grep -av '38;2' > "$log.txt" || true
}

# 1. From the stick: install on the NVMe disk.
qemu "$output/install.log" \
    -drive if=none,format=raw,id=stick,file="$work/stick.img" \
    -device virtio-blk-pci,drive=stick,bootindex=0,disable-legacy=on,disable-modern=off \
    -device nvme,drive=nvme,serial=osn-install-test
if ! grep -q '^OPENSHELL_INSTALL_DONE$' "$output/install.log.txt"; then
    echo 'install-test: the installer did not finish' >&2
    tail -n 40 "$output/install.log.txt" >&2
    exit 1
fi
grep -a '^OPENSHELL_INSTALL\|^PROGRESS' "$output/install.log.txt"

# 2. The NVMe disk alone: GRUB boots the installed release from slot a.
qemu "$output/installed.log" -device nvme,drive=nvme,serial=osn-install-test,bootindex=0
release=$(grep -a -o 'OPENSHELL_INSTALL_RELEASE [^ ]*' "$output/install.log.txt" | cut -d' ' -f2)
# GRUB's lines reach the serial port (grub.cfg, release.sh's boot.cfg).
if ! grep -aq 'OpenShell Native: booting a' "$output/installed.log.serial" ||
    ! grep -aq "OpenShell Native: release $release\$" <(tr -d '\r' < "$output/installed.log.serial") ||
    ! grep -aq 'initramfs is ready' "$output/installed.log.txt"; then
    echo 'install-test: the installed disk did not boot slot a' >&2
    tail -c 3000 "$output/installed.log.serial" >&2
    tail -n 30 "$output/installed.log.txt" >&2
    exit 1
fi
echo OPENSHELL_NATIVE_INSTALL_OK
