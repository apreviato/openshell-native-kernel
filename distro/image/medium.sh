# What the live medium's installer (install.sh) and its updater (update.sh)
# share: the computer's disks as they see them, and the medium's own release
# with its signed manifest. Sourced by both from the medium's payload
# (/opt/openshell-media, tools/qemu/run.sh).

# The filesystems of an OpenShell Native disk's partitions 2 to 5, by their
# labels (mkimage.sh and install.sh make them so; grub.cfg and
# installed-session.sh find them by number and UUID).
OPENSHELL_LABELS="osn-system_a osn-system_b osn-recovery osn-data"

# Whether a disk is a live medium of OpenShell Native (an ISO 9660 volume
# named OPENSHELL_NATIVE, build-media.sh), such as the stick this system
# started from: never offered.
live_medium() {
    [ "$(dd if="$1" bs=1 skip=32769 count=5 2>/dev/null | tr -d '\000')" = CD001 ] &&
        [ "$(dd if="$1" bs=1 skip=32808 count=16 2>/dev/null | tr -d '\000')" = OPENSHELL_NATIVE ]
}

# Whole disks the kernel offers (NVMe, SATA and USB, VirtIO), never their
# partitions.
whole_disks() {
    for device in /dev/nvme[0-9]*n[0-9]* /dev/sd[a-z] /dev/sd[a-z][a-z] /dev/vd[a-z]; do
        case "$device" in *p[0-9]*) continue ;; esac
        [ -b "$device" ] || continue
        echo "$device"
    done
    return 0
}

# A disk's partition N, as the kernel names it: nvme0n1p5, sda5, vda5.
partition() {
    case "$1" in
        *[0-9]) echo "${1}p$2" ;;
        *) echo "$1$2" ;;
    esac
}

# Whether a disk holds OpenShell Native: its partitions' filesystems, by
# label, as the kernel sees the partitions (not by the GPT's names, which
# parted cannot read where the kernel gives a disk 4096-byte sectors and the
# table was written for 512).
openshell_disk() {
    number=2
    for label in $OPENSHELL_LABELS; do
        [ "$(e2label "$(partition "$1" "$number")" 2>/dev/null)" = "$label" ] || return 1
        number=$((number + 1))
    done
}

# The partition of a system slot (a: 2, b: 3), and the data partition's (5).
slot_partition() {
    case "$2" in
        a) partition "$1" 2 ;;
        b) partition "$1" 3 ;;
        data) partition "$1" 5 ;;
    esac
}

# The medium's release as a release directory (channel.sh's archive holds
# the same): the signed kernel, the initramfs the computer started from
# (/dev/initrd, kernel patch 0021) and the installed system's boot.cfg, with
# MANIFEST and MANIFEST.sig, which build-media.sh puts on the kernel's
# command line (openshell.manifest, openshell.manifest_sig, base64: the
# manifest covers the initramfs, so it cannot be inside it). Fails without
# them: a medium built without a release key.
#   release_dir <folder>
release_dir() {
    mkdir -p "$1"
    for argument in $(cat /proc/cmdline); do
        case "$argument" in
            openshell.manifest=*) printf '%s' "${argument#*=}" | base64 -d > "$1/MANIFEST" 2>/dev/null || return 1 ;;
            openshell.manifest_sig=*) printf '%s' "${argument#*=}" | base64 -d > "$1/MANIFEST.sig" 2>/dev/null || return 1 ;;
        esac
    done
    [ -s "$1/MANIFEST" ] && [ -s "$1/MANIFEST.sig" ] || return 1
    ln -sf "$media/kernel" "$1/kernel"
    ln -sf "$media/boot.cfg" "$1/boot.cfg"
    ln -sf /dev/initrd "$1/initramfs.cpio.gz"
}

# Whether the release directory's files are the manifest's (a stick read
# badly at boot is not installed): each file's size and SHA-256.
release_intact() {
    dir=$1
    grep '^file=' "$dir/MANIFEST" | while read -r line; do
        set -- ${line#file=}
        name=$1 size=$2 sum=$3
        [ "$(head -c "$size" "$dir/$name" | sha256sum | cut -d' ' -f1)" = "$sum" ] || exit 1
        # A file is that long (the initramfs is what the boot loader loaded:
        # its file, whole).
        if [ -f "$dir/$name" ]; then
            [ "$(stat -L -c %s "$dir/$name")" = "$size" ] || exit 1
        fi
    done
}
