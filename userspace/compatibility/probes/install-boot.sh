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
# The installer's answers, as the supervisor hands them over (install.rs).
OPENSHELL_INSTALL_PASSWORD_HASH=$(printf %s install-test-password | /bin/cryptpw -m sha512 -P 0)
export OPENSHELL_INSTALL_PASSWORD_HASH
export OPENSHELL_INSTALL_NAME="Install Test" OPENSHELL_INSTALL_KEYBOARD="br abnt2" OPENSHELL_INSTALL_TIMEZONE=America/Sao_Paulo
"$installer" install "$device"
echo OPENSHELL_INSTALL_DONE
