#!/bin/sh
# The live system's part of the USB update test (distro/image/update-test.sh).
# Its first start installs it on the NVMe disk, as the session supervisor runs
# the installer; the next one (the kernel then sees the installation's
# partitions, as on a computer that has one) updates that installation from
# the stick, as the boot menu's Update entry has the supervisor run
# update.sh. Then it powers off.
set -eu
installer=/opt/openshell-compat/bin/openshell-native-install
updater=/opt/openshell-compat/bin/openshell-native-update
echo "OPENSHELL_UPDATE_RELEASE $(cat /opt/openshell-media/release)"
echo "== installations"
"$updater" list
device=$("$updater" list | awk '$1 ~ /^\/dev\/nvme/ { print $1; exit }')
if [ -z "$device" ]; then
    device=$("$installer" list | awk '$1 ~ /^\/dev\/nvme/ { print $1; exit }')
    test -n "$device"
    OPENSHELL_INSTALL_PASSWORD_HASH=$(printf %s update-test-password | /bin/cryptpw -m sha512 -P 0)
    export OPENSHELL_INSTALL_PASSWORD_HASH OPENSHELL_INSTALL_NAME="Update Test"
    "$installer" install "$device" 2>/dev/null | tail -n 1
    echo OPENSHELL_UPDATE_INSTALLED
    exit 0
fi
echo "== update"
"$updater" update "$device" 2>&1 | grep -v "^PROGRESS [3-8][0-9] " || echo "OPENSHELL_UPDATE_FAILED"
echo "== installations after"
"$updater" list
echo "== selector"
mkdir -p /tmp/data
mount -t ext2 -o ro "${device}p5" /tmp/data
/opt/openshell-compat/openshell-ab /tmp/data/openshell/ab status
grep -a -o "openshell_trial=[ab]" /tmp/data/openshell/ab/grubenv || true
umount /tmp/data
echo "== restore with nothing before"
"$updater" restore "$device" 2>&1 | tail -n 1 || true
echo OPENSHELL_UPDATE_DONE
