#!/bin/sh
# The live system's part of the installer test (distro/image/install-test.sh):
# it installs itself on the first NVMe disk, as the session supervisor runs
# the installer from the Desktop, and powers off.
set -eu

installer=/opt/openshell-compat/bin/openshell-native-install
echo "OPENSHELL_INSTALL_RELEASE $(cat /opt/openshell-media/release)"
echo "== disks"
"$installer" list
device=$("$installer" list | awk '$1 ~ /^\/dev\/nvme/ { print $1; exit }')
test -n "$device"
"$installer" install "$device"
echo OPENSHELL_INSTALL_DONE
