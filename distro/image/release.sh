#!/usr/bin/env bash
# A release as a system slot holds it, the same for the media, the disk image
# and its test: the kernel signed for Secure Boot (secureboot.sh), the
# initramfs and boot.cfg, the GRUB commands that boot it (grub.cfg sources
# the chosen slot's). Sourced by build-media.sh, boot-test.sh and, for the
# live medium's installer, tools/qemu/run.sh (install.sh copies the boot.cfg
# files written here):
#
#   release_files <folder> <kernel> <initramfs> <name> <init command...>
#   release_boot_cfg <name> <init command...>     boot.cfg alone, on stdout
#
# The kernel is Asterinas built with BOOT_PROTOCOL=linux-efi-handover64.
# SB_KEYS names the folder of OpenShell Native's signing key (sb_key).

source "$(dirname "${BASH_SOURCE[0]}")/secureboot.sh"

# The first process of each kind of release: the Desktop session (with
# "live" on a live medium) and the recovery shell.
RELEASE_SESSION=(sh -l /opt/openshell-compat/installed-session.sh)
RELEASE_RECOVERY=(sh -l)

release_files() {
    local dir=$1 kernel=$2 initramfs=$3 name=$4
    shift 4
    mkdir -p "$dir"
    sb_sign "${SB_KEYS:?the Secure Boot key folder}" "$kernel" "$dir/kernel"
    cp "$initramfs" "$dir/initramfs.cpio.gz"
    release_boot_cfg "$name" "$@" > "$dir/boot.cfg"
}

release_boot_cfg() {
    local name=$1
    shift
    # GRUB sets $root, $openshell_slot and $openshell_data_uuid (grub.cfg).
    # The echo reaches the serial port; the splash hides it on the screen.
    cat <<EOF
echo "OpenShell Native: release $name"
linux (\$root)/kernel SHELL=/bin/sh LOGNAME=root HOME=/ USER=root PATH=/bin:/usr/bin:/benchmark loglevel=error earlycon console=hvc0 openshell.slot=\$openshell_slot openshell.release=$name openshell.data=\$openshell_data_uuid -- $*
initrd (\$root)/initramfs.cpio.gz
EOF
}
