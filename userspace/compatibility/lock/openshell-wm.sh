#!/bin/sh
# The session's window manager (openshell-session's wm=): Openbox, with the
# keyboard's Num Lock remembered across restarts, as on OpenShell Live, whose
# xsession.sh starts the same helper (numlock.py) before the first password
# prompt. Here it starts with the window manager, before the greeter. The
# supervisor stops the helper with the window manager (one process group).
# initramfs.nix fills in the @…@ paths.
state=${XDG_STATE_HOME:-$HOME/.local/state}/openshell/numlock
runtime=${OPENSHELL_NATIVE_RUNTIME_DIR:-${TMPDIR:-/tmp}/openshell-wm-$(id -u)}
mkdir -p "$runtime"
@python@ -I @numlock@ --state "$state" --ready "$runtime/openshell-numlock-ready" > "$runtime/openshell-numlock.log" 2>&1 &
exec @openbox@ "$@"
