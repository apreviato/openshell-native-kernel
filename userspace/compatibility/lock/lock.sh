#!/bin/sh
# openshell-lock: the screen lock of OpenShell Native, as OpenShell Live's
# (lock.sh there). XSecureLock covers the screen and holds the keyboard and
# the mouse; OpenShell's lock screen (lock_screen.py, OpenShell Live's) is its
# auth module, and openshell-authproto checks the password with the session
# supervisor (userspace/shell, login.rs). It stays in the foreground until
# the screen is unlocked. hostd's lock service runs it (Lock, and after the
# idle time chosen in Settings ▸ Lock screen). initramfs.nix fills in the
# @…@ paths.
set -eu
exec @env@ -i \
    PATH=@path@ HOME="$HOME" GDK_BACKEND=x11 \
    DISPLAY="${DISPLAY:?No X11 session}" XAUTHORITY="${XAUTHORITY:-$HOME/.Xauthority}" \
    XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:?No session runtime directory}" \
    OPENSHELL_SYSTEM_SOCKET="${OPENSHELL_SYSTEM_SOCKET:?No session supervisor}" \
    FONTCONFIG_FILE="${FONTCONFIG_FILE:-}" \
    LANG=C.UTF-8 LC_ALL=C.UTF-8 \
    XCURSOR_THEME=Adwaita XCURSOR_PATH=@cursors@ \
    XSECURELOCK_AUTH=@auth@ \
    XSECURELOCK_AUTHPROTO=@authproto@ \
    XSECURELOCK_SAVER=@saver@ \
    XSECURELOCK_AUTH_BACKGROUND_COLOR='#15171c' XSECURELOCK_BACKGROUND_COLOR='#050607' \
    XSECURELOCK_AUTH_FOREGROUND_COLOR='#eeeeee' XSECURELOCK_AUTH_WARNING_COLOR='#ffb86c' \
    XSECURELOCK_SHOW_USERNAME=1 XSECURELOCK_SHOW_HOSTNAME=0 \
    XSECURELOCK_SHOW_DATETIME=1 XSECURELOCK_SHOW_KEYBOARD_LAYOUT=1 \
    XSECURELOCK_PASSWORD_PROMPT=asterisks XSECURELOCK_AUTH_TIMEOUT=600 \
    XSECURELOCK_BLANK_TIMEOUT=5 XSECURELOCK_BLANK_DPMS_STATE=off \
    @xsecurelock@ -- @ready@
