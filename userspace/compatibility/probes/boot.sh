#!/bin/sh
set -eu

trap 'echo OPENSHELL_NATIVE_PROBE_FAILED' EXIT
test -r /proc/self/status
test -d /dev/pts
test -t 0
test -t 1
echo OPENSHELL_NATIVE_PTY_OK

status=0
/etc/openshell-native-static-exit || status=$?
test "$status" -eq 42
echo OPENSHELL_NATIVE_STATIC_ELF_OK

test -b /dev/vda
grep -q '^/dev/vda /ext2 ext2 ' /proc/mounts
probe=$(mktemp /ext2/openshell-native.XXXXXX)
printf '%s\n' 'openshell-native' > "$probe"
sync
mv "$probe" "$probe.renamed"
test "$(cat "$probe.renamed")" = openshell-native
rm "$probe.renamed"
sync
echo OPENSHELL_NATIVE_BLOCK_IO_OK

trap - EXIT
echo 'Successfully booted.'
