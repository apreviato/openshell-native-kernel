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
if [ "${1:-}" = live ]; then
    data=/tmp/openshell-live
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
config=$(mktemp /tmp/openshell-installed-session.XXXXXX)
trap 'rm -f "$config"' EXIT
cat > "$config" <<EOF
user=65534:65534
display=:0
home=$state/home
log_dir=$data/openshell/session-logs
x=$root/bin/Xvfb :0 -screen 0 1280x800x24 -nolisten tcp -ac
wm=$root/bin/openbox
shell=$root/bin/openshell-shell /opt/openshell /opt/openshell-cef/openshell-cef-host $state $root/bin/node
env=PATH=$root/bin:/bin:/usr/bin
env=FONTCONFIG_FILE=$root/fonts.conf
env=XDG_CONFIG_DIRS=$root/xdg
env=XDG_DATA_DIRS=/usr/share
env=OPENSHELL_NATIVE_RUNTIME_DIR=$runtime
$confirm
EOF
"$root/bin/openshell-session" "$config" &
supervisor=$!
trap 'kill -TERM "$supervisor" 2>/dev/null || true' HUP INT TERM
status=0
wait "$supervisor" || status=$?
if kill -0 "$supervisor" 2>/dev/null; then
    wait "$supervisor" || status=$?
fi
exit "$status"
