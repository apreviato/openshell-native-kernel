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
    local args="SHELL=/bin/sh LOGNAME=root HOME=/ USER=root PATH=/bin:/usr/bin:/benchmark loglevel=error earlycon console=hvc0 openshell.slot=\$openshell_slot openshell.release=$name openshell.data=\$openshell_data_uuid"
    # GRUB sets $root, $openshell_slot, $openshell_data_uuid and, with the
    # data partition, $openshell_data (grub.cfg). The echo reaches the serial
    # port; the splash hides it on the screen.
    #
    # A system slot whose first boot copied the system to the data partition
    # (installed-session.sh, populate_disk_root) starts from that copy, with
    # the small boot-root.cpio (probes/disk-root.sh) instead of the whole
    # system in memory. The copy's GRUB environment block says for which
    # release it is ready; GRUB marks the attempt there first, and the system
    # marks it good once it runs. An attempt that never got so far, a copy of
    # another release or none starts the system from memory, as before.
    cat <<EOF
echo "OpenShell Native: release $name"
set openshell_tree=
if [ "\$openshell_slot" = a -o "\$openshell_slot" = b ]; then
    set openshell_tree_env=(\$openshell_data)/openshell/system-root/\$openshell_slot.env
    # Tested first: a missing file would print GRUB's error over the splash.
    if [ -n "\$openshell_data" -a -f "\$openshell_tree_env" ]; then
        set openshell_tree_release=
        set openshell_tree_state=
        if load_env --file \$openshell_tree_env openshell_tree_release openshell_tree_state; then
            if [ "\$openshell_tree_release" = "$name" -a "\$openshell_tree_state" = good ]; then
                set openshell_tree_state=trying
                if save_env --file \$openshell_tree_env openshell_tree_state; then
                    set openshell_tree=(\$openshell_data)/openshell/system-root/\$openshell_slot
                fi
            fi
        fi
    fi
fi
if [ -n "\$openshell_tree" ]; then
    echo "OpenShell Native: the system's copy on the data partition"
    linux (\$root)/kernel $args openshell.root=disk -- $*
    initrd \$openshell_tree/opt/openshell-compat/boot-root.cpio
else
    linux (\$root)/kernel $args -- $*
    initrd (\$root)/initramfs.cpio.gz
fi
EOF
}
