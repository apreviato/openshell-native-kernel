#!/bin/busybox sh
# OpenShell Native: an installed system started from its copy on the data
# partition, the init of a small initramfs (boot-root.cpio).
#
# The installed system used to run whole from memory: the kernel unpacked the
# release's initramfs (2.8 GB) into RAM at every boot and kept the compressed
# one (1 GB) besides. Now the first boot of a slot after an install or an
# update still does, and copies the system to /data/openshell/system-root/<slot>
# in the background (installed-session.sh, populate_disk_root). From the next
# boot on, GRUB loads this initramfs instead (distro/image/release.sh,
# boot.cfg), and here the copy becomes the root: the kernel reads from the
# disk what is used and caches it, as OpenShell Live's squashfs does.
#
# GRUB marks the attempt in the copy's GRUB environment block before the
# handover, and the system marks it good once it has run (installed-session.sh,
# disk_root_good). Whatever goes wrong here, the computer restarts, and GRUB,
# seeing the attempt unfinished, starts the system from memory again.
#
# The kernel's arguments after `--` are the session's, passed on to the copy's /init.

/bin/busybox --install -s /bin
export PATH=/bin
# The kernel's log is the only place a computer without a serial port keeps this.
say() {
    echo "<${2:-5}>openshell-disk-root: $1" > /dev/kmsg 2> /dev/null
}
fail() {
    say "$1; the next start runs from memory" 3
    sync
    reboot -f
    sleep 60
}

mkdir -p /proc /sys /dev /data /newroot
mount -t proc proc /proc
mount -t devtmpfs devtmpfs /dev 2> /dev/null

slot= data_uuid= release=
for argument in $(cat /proc/cmdline); do
    case "$argument" in
        openshell.slot=*) slot=${argument#*=} ;;
        openshell.data=*) data_uuid=${argument#*=} ;;
        openshell.release=*) release=${argument#*=} ;;
    esac
done
case "$slot" in a|b) ;; *) fail "no system slot on the command line" ;; esac

# The data partition, by the ext2 UUID GRUB found it with (installed-storage.sh).
uuid=$(printf '%s' "$data_uuid" | tr -d '-')
device=
for candidate in /dev/*5; do
    [ -b "$candidate" ] || continue
    actual=$(od -An -v -tx1 -j 1128 -N 16 "$candidate" 2> /dev/null | tr -d ' \n')
    if [ -n "$uuid" ] && [ "$actual" = "$uuid" ]; then
        device=$candidate
        break
    fi
done
[ -n "$device" ] || fail "no data partition with UUID $data_uuid"
mount -t ext2 "$device" /data || fail "$device does not mount"

tree=/data/openshell/system-root/$slot
[ -x "$tree/init" ] && [ -d "$tree/nix/store" ] && [ -d "$tree/data" ] || fail "the copy in $tree is incomplete"
say "release $release from $tree on $device"

# The copy is the root, with the data partition in it at /data (the same
# mount: never a second one of the same disk) and /tmp in memory.
mount --bind "$tree" /newroot || fail "$tree cannot be the root"
mount --move /data /newroot/data || fail "the data partition cannot move into the root"
mount -t tmpfs -o mode=1777 tmpfs /newroot/tmp || fail "no /tmp"
mount --move /dev /newroot/dev 2> /dev/null || umount /dev
umount /proc

cd /newroot || fail "the root is gone"
exec chroot . /init "$@"
fail "the copy's /init did not start"
