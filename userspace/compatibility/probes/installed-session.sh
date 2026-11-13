#!/bin/sh
# Starts the Desktop with persistent state on disk, or temporary state in live mode.
# The root supervisor invokes this script again only after Desktop health.
set -eu

root=/opt/openshell-compat
data=/data
ab=$data/openshell/ab
# The release channel's keys (distro/image/channel.sh) and the data schema
# releases must accept (CHANNEL_SCHEMA there).
release_keys=/opt/openshell-media/release-keys
data_schema=1

# Writes the staged release into the inactive slot (update.rs): openshell-ab
# checks its signed manifest against the release keys and every file against
# the manifest, and schedules the trial boot. The result is the supervisor's
# "last". Uses $updates_dir, $device (the data partition) and $slot.
apply_update() {
    name=$(head -n1 "$updates_dir/staged")
    rm -f "$updates_dir/staged"
    work=$updates_dir/work
    target=/tmp/openshell-update-slot
    result=
    rm -rf "$work"
    mkdir -p "$work" "$target"
    case "$name" in
        openshell-native-*-amd64.tar) ;;
        *) result="failed the staged update has an invalid name" ;;
    esac
    # The other system slot of the data partition's disk: system_a is 2, system_b 3.
    case "$slot" in a) other=3 ;; b) other=2 ;; *) other= ;; esac
    if [ -z "$result" ] && [ -n "$other" ] && tar -xf "$updates_dir/inbox/$name" -C "$work" MANIFEST MANIFEST.sig kernel initramfs.cpio.gz boot.cfg &&
        mount -t ext2 "${device%5}$other" "$target"; then
        if "$root/openshell-ab" "$ab" update-mounted "$work" "$work/MANIFEST" "$data_schema" "$release_keys" "$target" > "$updates_dir/log" 2>&1; then
            result="applied $(sed -n 's/^release=//p' "$work/MANIFEST")"
        fi
        umount "$target"
    fi
    [ -n "$result" ] || result="failed $(tail -n1 "$updates_dir/log" 2>/dev/null | tr -cd '[:print:]' | cut -c1-200)"
    printf '%s\n' "$result" > "$updates_dir/last"
    rm -rf "$work" "$updates_dir/inbox/$name" "$updates_dir/inbox/$name".*
    sync
    case "$result" in applied*) return 0 ;; *) return 1 ;; esac
}
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
settings=
updates=
# The Desktop's mode, as OpenShell Live tells it from its files (hostd context.ts).
mode=installed
if [ "${1:-}" = live ]; then
    mode=live
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
    # The time zone and the account's name chosen in Settings (settings.rs).
    settings="settings_dir=$data/openshell/system"
    # Updates from the release channel (userspace/shell/src/update.rs): the
    # Desktop downloads into the inbox, the supervisor records what is staged,
    # and init writes it into the other slot before stopping (apply_update).
    updates_dir=$data/openshell/updates
    mkdir -p "$updates_dir/inbox"
    chmod 0755 "$updates_dir"
    chown 65534:65534 "$updates_dir/inbox"
    chmod 0700 "$updates_dir/inbox"
    updates="updates_dir=$updates_dir
env=OPENSHELL_NATIVE_UPDATES_INBOX=$updates_dir/inbox
env=OPENSHELL_NATIVE_RELEASE_KEYS=$release_keys"
    # A release staged before a stop that did not apply it: now, then its trial boot.
    if [ -f "$updates_dir/staged" ]; then
        apply_update && { sync; reboot -f; }
    fi
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
env=OPENSHELL_MODE=$mode
$confirm
$login
$settings
$updates
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
        if [ -n "$updates" ]; then apply_update || true; fi
        sync
        if grep -q ' /data ' /proc/mounts; then
            umount /data 2>/dev/null || mount -o remount,ro /data 2>/dev/null || true
        fi
        sync
        if [ "$status" = 64 ]; then poweroff -f; else reboot -f; fi
        ;;
esac
exit "$status"
