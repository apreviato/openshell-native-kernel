#!/usr/bin/env bash
# The disk image end to end, in QEMU with OVMF: GRUB picks the slot from the
# A/B transaction's grubenv, the system records the boot, and each boot does
# what the test plan says (probes/image-boot.sh). Three disks:
#
#   fallback  2026.12.0 on trial in slot b never gets healthy: three boots of
#             b, then GRUB boots a again and b becomes bad.
#   confirm   2026.12.0 on trial is confirmed on its first boot and stays.
#   recovery  no readable selector or grubenv: GRUB boots the recovery.
#
#   boot-test.sh <openshell-ab> <kernel> <initramfs> <output-dir>
#
# OVMF_DIR names the folder of OVMF.fd. Needs qemu-system-x86_64 and what
# mkimage.sh needs.
set -euo pipefail

[[ $# -eq 4 ]] || { sed -n '2,13p' "$0" >&2; exit 2; }
ab=$(realpath "$1") kernel=$(realpath "$2") initramfs=$(realpath "$3") output=$4
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$output"
work=$(mktemp -d "${TMPDIR:-/tmp}/openshell-boot-test-XXXXXX")
trap 'rm -rf "$work"' EXIT
"$ab" keygen "$work/release.key" "$work/keys.pem"
mkdir -p "$work/keys" && mv "$work/keys.pem" "$work/keys/release.pem"

# A release: the kernel and initramfs under test, and its boot command.
release() {
    local name=$1 dir=$2
    mkdir -p "$dir"
    cp "$kernel" "$dir/kernel"
    cp "$initramfs" "$dir/initramfs.cpio.gz"
    cat > "$dir/boot.cfg" <<EOF
multiboot2 (\$root)/kernel SHELL=/bin/sh LOGNAME=root HOME=/ USER=root PATH=/bin:/benchmark loglevel=error earlycon console=hvc0 openshell.slot=\$openshell_slot openshell.release=$name -- sh -l /opt/openshell-compat/image-boot.sh
module2 --nounzip (\$root)/initramfs.cpio.gz
EOF
    {
        printf 'release=%s\narch=x86_64\nschema=1..1\n' "$name"
        for file in kernel initramfs.cpio.gz boot.cfg; do
            printf 'file=%s %s %s\n' "$file" "$(stat -c %s "$dir/$file")" "$(sha256sum "$dir/$file" | cut -d' ' -f1)"
        done
    } > "$dir/MANIFEST"
    "$ab" sign "$work/release.key" "$dir/MANIFEST"
}
release 2026.11.0 "$work/2026.11.0"
release 2026.12.0 "$work/2026.12.0"
release recovery "$work/recovery"
rm "$work/recovery/MANIFEST" "$work/recovery/MANIFEST.sig"

# A disk as the factory installs it, with 2026.12.0 staged and on trial.
installed() {
    local disk=$1
    "$ab" "$disk" install 2026.11.0
    mkdir -p "$disk/slot-a"
    cp "$work/2026.11.0"/{kernel,initramfs.cpio.gz,boot.cfg} "$disk/slot-a/"
    "$ab" "$disk" stage "$work/2026.12.0" "$work/2026.12.0/MANIFEST" 1 "$work/keys"
    "$ab" "$disk" trial
}

qemu() {
    local image=$1 log=$2
    timeout --kill-after=15s "${OPENSHELL_IMAGE_BOOT_TIMEOUT:-20m}" qemu-system-x86_64 \
        -cpu Icelake-Server,+x2apic -smp 1 -m 2G --no-reboot -nographic -display none \
        -machine q35,kernel-irqchip=split -device isa-debug-exit,iobase=0xf4,iosize=0x04 \
        -bios "${OVMF_DIR:-/root/ovmf/release}/OVMF.fd" \
        -drive if=none,format=raw,id=disk,file="$image" \
        -device virtio-blk-pci,bus=pcie.0,addr=0x6,drive=disk,bootindex=0,disable-legacy=on,disable-modern=off,queue-size=64,num-queues=1,request-merging=off,backend_defaults=off,discard=off,write-zeroes=off,event_idx=off,indirect_desc=off,queue_reset=off \
        -device virtio-serial-pci,disable-legacy=on,disable-modern=off -device virtconsole,chardev=mux \
        -chardev stdio,id=mux,mux=on,signal=off,logfile="$log" -monitor chardev:mux -serial file:"$log.serial" \
        < /dev/null > /dev/null 2>&1 || true
    tr -d '\r' < "$log" > "$log.txt"
}

# Boots a disk once per line of the plan and checks each boot, then the
# selector's last status.
run() {
    local name=$1 disk=$2 status=$3
    mkdir -p "$work/$name-data/openshell/image-test"
    printf '%s\n' "${plan[@]}" > "$work/$name-data/openshell/image-test/plan"
    "$here/mkimage.sh" "$disk" "$work/recovery" "$work/$name.img" "$work/$name-data"
    local step=1 line
    for line in "${plan[@]}"; do
        local log="$output/$name-$step.log"
        qemu "$work/$name.img" "$log"
        if ! grep -q "^OPENSHELL_IMAGE_STEP_DONE $step\$" "$log.txt" || grep -q '^OPENSHELL_IMAGE_WRONG_BOOT' "$log.txt"; then
            printf 'image %s, boot %s: expected %s\n' "$name" "$step" "$line" >&2
            grep -a 'OpenShell Native: booting\|^OPENSHELL_IMAGE' "$log.txt" >&2 || tail -n 30 "$log.txt" >&2
            return 1
        fi
        grep -a '^OPENSHELL_IMAGE' "$log.txt"
        step=$(( step + 1 ))
    done
    if ! grep -q "^OPENSHELL_IMAGE_STATUS $status\$" "$output/$name-$(( step - 1 )).log.txt"; then
        printf 'image %s: the selector does not end with %s
' "$name" "$status" >&2
        return 1
    fi
}

installed "$work/fallback"
plan=("b 2026.12.0 fail" "b 2026.12.0 fail" "b 2026.12.0 fail" "a 2026.11.0 confirm" "a 2026.11.0 confirm")
run fallback "$work/fallback" "b: Bad 2026.12.0"
installed "$work/confirm"
plan=("b 2026.12.0 confirm" "b 2026.12.0 confirm")
run confirm "$work/confirm" "b: Good 2026.12.0 (confirmed)"
mkdir -p "$work/blank"
plan=("recovery recovery none")
run recovery "$work/blank" "none"
echo OPENSHELL_NATIVE_IMAGE_OK
