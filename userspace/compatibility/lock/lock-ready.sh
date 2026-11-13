#!/bin/sh
# XSecureLock runs this once its cover and input grabs hold, as on OpenShell
# Live (lock-ready.sh there): it records XSecureLock's pid where the Desktop
# looks for it ($XDG_RUNTIME_DIR/openshell-locked, apps/desktop
# shared/screenLock.ts) and has it show the password prompt at once, not a
# blank screen first.
set -eu
parent=$PPID
[ "$(cat "/proc/$parent/comm")" = xsecurelock ] || exit 1
printf '%s\n' "$parent" > "${XDG_RUNTIME_DIR:?}/openshell-locked"
kill -USR2 "$parent"
