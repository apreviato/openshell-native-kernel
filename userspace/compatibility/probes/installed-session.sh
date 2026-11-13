#!/bin/sh
# Starts the Desktop with persistent state on disk, or temporary state in live mode.
# The root supervisor invokes this script again only after Desktop health.
set -eu

root=/opt/openshell-compat
data=/data
ab=$data/openshell/ab
slot=
release=
data_uuid=
for argument in $(cat /proc/cmdline); do
    case "$argument" in
        openshell.slot=*) slot=${argument#*=} ;;
        openshell.release=*) release=${argument#*=} ;;
        openshell.data=*) data_uuid=${argument#*=} ;;
    esac
done
confirm=
login=
if [ "${1:-}" = live ]; then
    data=/tmp/openshell-live
    # The live medium installs itself on a disk (Install OpenShell), as root.
    if [ -x "$root/bin/openshell-native-install" ]; then
        login="installer=$root/bin/openshell-native-install"
    fi
else
    case "$slot" in a|b) ;; *) exit 1 ;; esac
    test -n "$release"
    mounted_device=$(awk '$2 == "/data" && $3 == "ext2" && $4 ~ /(^|,)rw(,|$)/ { print $1 }' /proc/mounts)
    if [ "${1:-}" = confirm ]; then
        test -n "$mounted_device"
        exec "$root/openshell-ab" "$ab" confirm "$slot" "$release"
    fi
    . "$root/installed-storage.sh"
    device=$(installed_data_device "$data_uuid")
    if [ -z "$mounted_device" ]; then
        mkdir -p "$data"
        mount -t ext2 "$device" "$data"
    else
        test "$mounted_device" = "$device"
    fi
    "$root/openshell-ab" "$ab" booted "$slot"
    confirm="confirm=/bin/sh $root/installed-session.sh confirm"
    # The installed system asks for its password before the Desktop (the
    # supervisor keeps the hash; the first boot creates it). The live session
    # starts without one, as OpenShell Live's.
    login="greeter=$root/bin/openshell-shell /opt/openshell /opt/openshell-cef/openshell-cef-host /tmp/openshell-greeter-state $root/bin/node
password_file=$data/openshell/account/password"
fi

state=$data/openshell/desktop
mkdir -p "$state"
chmod 0700 "$state"
chown 65534:65534 "$state"
chmod 1777 /tmp
mkdir -p /dev/shm
mkdir -p /tmp/.X11-unix
chmod 1777 /tmp/.X11-unix
if ! grep -q ' /dev/shm ' /proc/mounts; then
    mount -t tmpfs -o mode=1777 shm /dev/shm
fi
runtime=/tmp/openshell-installed
mkdir -p "$runtime"
chmod 0700 "$runtime"
chown 65534:65534 "$runtime"
# The computer's screen, keyboard and mouse belong to the session user, who
# runs X (Xorg on DRM, or Xvfb without a screen: openshell-x.sh).
"$root/bin/openshell-x" --seat 65534
config=$(mktemp /tmp/openshell-installed-session.XXXXXX)
trap 'rm -f "$config"' EXIT
cat > "$config" <<EOF
user=65534:65534
display=:0
home=$state/home
log_dir=$data/openshell/session-logs
x=$root/bin/openshell-x :0 -screen 0 1280x800x24 -nolisten tcp -ac
wm=$root/bin/openbox
shell=$root/bin/openshell-shell /opt/openshell /opt/openshell-cef/openshell-cef-host $state $root/bin/node
env=PATH=$root/bin:/bin:/usr/bin
env=FONTCONFIG_FILE=$root/fonts.conf
env=XDG_CONFIG_DIRS=$root/xdg
env=XDG_DATA_DIRS=/usr/share
env=OPENSHELL_NATIVE_RUNTIME_DIR=$runtime
$confirm
$login
EOF
"$root/bin/openshell-session" "$config" &
supervisor=$!
trap 'kill -TERM "$supervisor" 2>/dev/null || true' HUP INT TERM
status=0
wait "$supervisor" || status=$?
if kill -0 "$supervisor" 2>/dev/null; then
    wait "$supervisor" || status=$?
fi
# The session asked to power off or restart (userspace/shell/src/power.rs,
# POWEROFF_STATUS and REBOOT_STATUS): the supervisor cannot, init can, once
# the data partition is written out.
case "$status" in
    64|65)
        sync
        if grep -q ' /data ' /proc/mounts; then
            umount /data 2>/dev/null || mount -o remount,ro /data 2>/dev/null || true
        fi
        sync
        if [ "$status" = 64 ]; then poweroff -f; else reboot -f; fi
        ;;
esac
exit "$status"
