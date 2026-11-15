#!/bin/sh
# Updates an installed OpenShell Native from the live medium, as root: the
# boot menu's "Update installed OpenShell Native (keep files)", as on
# OpenShell Live. The medium's release goes into the installation's system
# slot that is not running (its A/B transaction, openshell-ab update-offline,
# which checks the release's signed manifest and every file it writes), and
# the next start of the computer runs it on trial: its session confirms it,
# three failed starts go back to the release it had. The data partition,
# the account and everything people keep are left as they are. "restore"
# puts the release from before the last update on trial instead.
#
# The session supervisor runs it (userspace/shell/src/usb_update.rs) when
# the setup screen asks:
#
#   openshell-native-update list                one installation per line:
#                                               "<device> <bytes> <release> <previous release or ->"
#   openshell-native-update update <device>     progress as "PROGRESS <percent> <message>", then "DONE"
#   openshell-native-update restore <device>
#
# The release comes from memory, as the installer's does (install.sh): the
# initramfs the computer started from and /opt/openshell-media.
set -eu

media=${OPENSHELL_MEDIA:-/opt/openshell-media}
compat=/opt/openshell-compat
. "$media/medium.sh"

progress() { echo "PROGRESS $1 $2"; }
fail() { echo "FAILED $*" >&2; exit 1; }

# The data schema this medium's release keeps (installed-session.sh's), which
# its manifest must accept.
data_schema=$(sed -n 's/^data_schema=//p' "$compat/installed-session.sh" | head -n1)

work=
unmount_all() {
    for mount in "${work:-/nonexistent}/slot" "${work:-/nonexistent}/data"; do
        if grep -q " $mount " /proc/mounts 2>/dev/null; then
            umount "$mount" 2>/dev/null || true
        fi
    done
    [ -z "$work" ] || rm -rf "$work"
}
trap unmount_all EXIT

# Mounts a disk's data partition at $work/data (read-only unless asked).
mount_data() {
    mkdir -p "$work/data"
    mount -t ext2 ${2:+-o $2} "$(slot_partition "$1" data)" "$work/data"
}

# The installation's slots, from its selector: "<confirmed> <release> <other's release or ->".
slots_of() {
    "$compat/openshell-ab" "$1" status 2>/dev/null | awk '
        /^[ab]: / { slot = substr($1, 1, 1); state[slot] = $2; release[slot] = $3; if ($4 == "(confirmed)") confirmed = slot }
        END {
            if (confirmed == "") exit 1
            other = confirmed == "a" ? "b" : "a"
            previous = (state[other] == "Good") ? release[other] : "-"
            print confirmed, release[confirmed], previous
        }'
}

case ${1:-} in
list)
    for device in $(whole_disks); do
        live_medium "$device" && continue
        openshell_disk "$device" || continue
        work=$(mktemp -d /tmp/openshell-update.XXXXXX)
        if mount_data "$device" ro 2> /dev/null; then
            if found=$(slots_of "$work/data/openshell/ab"); then
                set -- $found
                echo "$device $(blockdev --getsize64 "$device") $2 $3"
            fi
            umount "$work/data"
        fi
        rm -rf "$work"
        work=
    done
    ;;
update | restore)
    action=$1
    device=${2:-}
    "$0" list | grep -q "^$device " || fail "$device holds no OpenShell Native installation to update"
    work=$(mktemp -d /tmp/openshell-update.XXXXXX)
    if [ "$action" = update ]; then
        progress 2 "Checking the USB release"
        release_dir "$work/release" || fail "this USB stick carries no signed release; make it again with the release key"
        release_intact "$work/release" || fail "the USB stick could not be read correctly; write it again"
        release=$(sed -n 's/^release=//p' "$work/release/MANIFEST")
    fi
    progress 15 "Opening the installation"
    mount_data "$device" || fail "the installation's data partition could not be opened"
    ab=$work/data/openshell/ab
    found=$(slots_of "$ab") || fail "the installation has no confirmed release"
    set -- $found
    confirmed=$1
    if [ "$action" = restore ]; then
        progress 50 "Restoring the previous release"
        "$compat/openshell-ab" "$ab" restore > "$work/ab.log" 2>&1 || fail "$(tail -n1 "$work/ab.log")"
    else
        case "$confirmed" in a) target_slot=b ;; *) target_slot=a ;; esac
        mkdir -p "$work/slot"
        mount -t ext2 "$(slot_partition "$device" "$target_slot")" "$work/slot" || fail "the installation's system partition could not be opened"
        progress 25 "Writing OpenShell Native $release"
        total=$(awk '/^file=/ { sum += $2 } END { print int(sum / 1024) + 1 }' "$work/release/MANIFEST")
        "$compat/openshell-ab" "$ab" update-offline "$work/release" "$work/release/MANIFEST" "$data_schema" "$media/release-keys" "$work/slot" \
            > "$work/ab.log" 2>&1 &
        writer=$!
        # Its progress: what reached the partition of what the release holds.
        while kill -0 "$writer" 2> /dev/null; do
            written=$(du -sk "$work/slot" 2> /dev/null | cut -f1)
            percent=$((25 + 60 * ${written:-0} / total))
            [ "$percent" -le 85 ] || percent=85
            progress "$percent" "Writing OpenShell Native $release"
            sleep 1
        done
        wait "$writer" || fail "$(tail -n1 "$work/ab.log" | sed 's/^openshell-ab: //')"
        progress 90 "Finishing"
        # That slot's copy on the data partition is of its old release: the
        # new one's first start runs from memory and copies it anew
        # (installed-session.sh, populate_disk_root).
        rm -f "$work/data/openshell/system-root/$target_slot.env" "$work/data/openshell/system-root/$target_slot.failed"
        sync
        umount "$work/slot"
    fi
    sync
    umount "$work/data"
    progress 100 "Done"
    echo DONE
    ;;
*)
    sed -n '2,20p' "$0" >&2
    exit 2
    ;;
esac
