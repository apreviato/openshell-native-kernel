#!/bin/sh
# One boot of the disk image test (distro/image/boot-test.sh): the system
# records the boot GRUB chose (openshell-ab booted), then does what the test
# plan on the data partition says for this boot, before it powers off:
#
#   <slot> <release> <confirm|fail>    one line per boot
#
# confirm is a healthy session (openshell-session confirms the boot); fail is
# a release that never gets there.
set -eu

cd /opt/openshell-compat
cmdline=" $(cat /proc/cmdline) "
value() { echo "$cmdline" | sed -n "s/.* $1=\\([^ ]*\\) .*/\\1/p"; }
slot=$(value openshell.slot)
release=$(value openshell.release)

mkdir -p /data
for device in /dev/vd?[0-9]; do
    if mount -t ext2 "$device" /data 2>/dev/null; then
        if [ -d /data/openshell/image-test ]; then break; fi
        umount /data
    fi
done
ab=/data/openshell/ab
plan=/data/openshell/image-test/plan
done_list=/data/openshell/image-test/done
test -f "$plan"
touch "$done_list"
step=$(( $(wc -l < "$done_list") + 1 ))
expected=$(sed -n "${step}p" "$plan")
echo "OPENSHELL_IMAGE_BOOT $step $slot $release (expected $expected)"

# The recovery boots without a selector to record the boot in.
if [ "$slot" != recovery ]; then
    ./openshell-ab "$ab" booted "$slot"
fi
set -- $expected
if [ "$slot" != "$1" ] || [ "$release" != "$2" ]; then
    echo "OPENSHELL_IMAGE_WRONG_BOOT $step"
elif [ "$3" = confirm ]; then
    ./openshell-ab "$ab" confirm "$slot"
fi
status=$(./openshell-ab "$ab" status 2>/dev/null) || status=none
echo "$status" | sed 's/^/OPENSHELL_IMAGE_STATUS /'
echo "$slot $release" >> "$done_list"
sync
umount /data
echo "OPENSHELL_IMAGE_STEP_DONE $step"
