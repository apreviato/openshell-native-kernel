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
# Each system slot's copy of its release on the data partition, which later
# boots start from instead of unpacking the whole system into memory
# (probes/disk-root.sh, distro/image/release.sh's boot.cfg): <slot>/ is the
# copy, <slot>.env the GRUB environment block that says for which release it
# is good, and <slot>.failed how often starting from it failed.
system_roots=$data/openshell/system-root
# Room the copy needs on the data partition, in KiB (2.8 GB today, and growth).
disk_root_room=6291456

# Writes a GRUB environment block (1024 bytes, as distro/ab/src/bootenv.rs
# does): `grub_env <file> <name=value>...`.
grub_env() {
    file=$1
    shift
    {
        printf '# GRUB Environment Block\n'
        for pair in "$@"; do printf '%s\n' "$pair"; done
    } > "$file.new"
    size=$(wc -c < "$file.new")
    head -c $((1024 - size)) /dev/zero | tr '\0' '#' >> "$file.new"
    mv "$file.new" "$file"
    sync
}

# The system ran from its copy: the next boots start from it too.
disk_root_good() {
    grub_env "$system_roots/$slot.env" "openshell_tree_release=$release" "openshell_tree_state=good"
    rm -f "$system_roots/$slot.failed"
}

# On a boot from memory: copies the release, from the initramfs the boot
# loader gave the kernel, to the data partition. A copy that a boot could not
# start from (GRUB's mark "trying" is still there) counts as failed; after two
# failures the slot keeps starting from memory until the next update.
populate_disk_root() {
    tree=$system_roots/$slot
    env=$system_roots/$slot.env
    failed=$(cat "$tree.failed" 2> /dev/null || echo 0)
    if grep -q '^openshell_tree_state=trying$' "$env" 2> /dev/null; then
        failed=$((failed + 1))
        echo "$failed" > "$tree.failed"
    fi
    [ "$failed" -lt 2 ] || return 0
    [ -r /dev/initrd ] || return 0
    mkdir -p "$system_roots"
    rm -rf "$env" "$tree" "$tree.new"
    free=$(df -k "$data" | awk 'NR == 2 { print $4 }')
    if [ "${free:-0}" -lt "$disk_root_room" ]; then
        # The other slot's copy goes first: its boot copies it again.
        case "$slot" in a) other_tree=b ;; *) other_tree=a ;; esac
        rm -rf "$system_roots/$other_tree" "$system_roots/$other_tree.env"
        free=$(df -k "$data" | awk 'NR == 2 { print $4 }')
        [ "${free:-0}" -ge "$disk_root_room" ] || return 0
    fi
    mkdir "$tree.new"
    if (cd "$tree.new" && gzip -dc < /dev/initrd | cpio -idm 2> /dev/null) &&
        [ -x "$tree.new/init" ] && mkdir -p "$tree.new/data" "$tree.new/tmp"; then
        sync
        mv "$tree.new" "$tree"
        disk_root_good
    else
        rm -rf "$tree.new"
    fi
}

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
            # That slot's copy on the data partition is of its old release:
            # its first boot runs from memory and copies the new one.
            case "$other" in 2) target_slot=a ;; *) target_slot=b ;; esac
            rm -f "$system_roots/$target_slot.env" "$system_roots/$target_slot.failed"
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
# Started from the system's copy on the data partition (probes/disk-root.sh).
disk_root=
for argument in $(cat /proc/cmdline); do
    case "$argument" in
        openshell.slot=*) slot=${argument#*=} ;;
        openshell.release=*) release=${argument#*=} ;;
        openshell.data=*) data_uuid=${argument#*=} ;;
        openshell.root=disk) disk_root=1 ;;
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
        if [ -n "$disk_root" ]; then
            # The Desktop is up on the copy: later boots start from it too.
            disk_root_good
        else
            test -n "$mounted_device"
        fi
        exec "$root/openshell-ab" "$ab" confirm "$slot" "$release"
    fi
    . "$root/installed-storage.sh"
    device=$(installed_data_device "$data_uuid")
    if [ -n "$disk_root" ] && [ -d "$data/openshell" ]; then
        # disk-root.sh moved the data partition into this root: one mount of it.
        :
    elif [ -z "$mounted_device" ]; then
        mkdir -p "$data"
        mount -t ext2 "$device" "$data"
    else
        test "$mounted_device" = "$device"
    fi
    if [ "${1:-}" = populate ]; then
        populate_disk_root
        exit 0
    fi
    "$root/openshell-ab" "$ab" booted "$slot"
    # Memory: later boots start from the system's copy on the data partition
    # instead of unpacking all of it into memory. A boot from memory makes the
    # copy, slowly, while the session runs; a boot from the copy marks it good
    # once it has run a while or the Desktop is up (confirm).
    disk_root_job=
    if [ -n "$disk_root" ]; then
        (sleep 90 && disk_root_good) > /dev/null 2>&1 &
    else
        nice -n 19 sh "$root/installed-session.sh" populate > /dev/null 2>&1 &
        disk_root_job=$!
    fi
    # The time zone, the account's name and the keyboard layout chosen in the
    # installer or in Settings (settings.rs); X starts with the layout.
    settings="settings_dir=$data/openshell/system"
    if [ -s "$data/openshell/system/keyboard" ]; then
        settings="$settings
env=OPENSHELL_X_KEYBOARD=$(head -n1 "$data/openshell/system/keyboard")"
    fi
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
    # supervisor keeps the hash, which the installer writes): on the lock
    # screen, as OpenShell Live's session does, when the image has it, and
    # with the greeter, which can also create a password, otherwise. The live
    # session starts without one, as OpenShell Live's.
    login="greeter=$root/bin/openshell-shell /opt/openshell /opt/openshell-cef/openshell-cef-host /tmp/openshell-greeter-state $root/bin/node
password_file=$data/openshell/account/password"
    if [ -x "$root/bin/openshell-lock" ]; then
        login="$login
lock=$root/bin/openshell-lock"
    fi
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
# Openbox, with Num Lock remembered where the image has the helper (openshell-wm).
wm=$root/bin/openbox
if [ -x "$root/bin/openshell-wm" ]; then wm=$root/bin/openshell-wm; fi
config=$(mktemp /tmp/openshell-installed-session.XXXXXX)
trap 'rm -f "$config"' EXIT
cat > "$config" <<EOF
user=65534:65534
display=:0
home=$state/home
log_dir=$data/openshell/session-logs
x=$root/bin/openshell-x :0 -screen 0 1280x800x24 -nolisten tcp -ac
wm=$wm
shell=$root/bin/openshell-shell /opt/openshell /opt/openshell-cef/openshell-cef-host $state $root/bin/node
env=PATH=$root/bin:/bin:/usr/bin
env=FONTCONFIG_FILE=$root/fonts.conf
env=XDG_CONFIG_DIRS=$root/xdg
env=XDG_DATA_DIRS=/usr/share
env=OPENSHELL_NATIVE_RUNTIME_DIR=$runtime
env=XDG_RUNTIME_DIR=$runtime
env=OPENSHELL_MODE=$mode
$confirm
$login
$settings
$updates
EOF
# The supervisor's own lines go to its journal (session-logs/<boot>/session.log)
# and, as it writes them to stderr too, to supervisor.log: never to the
# console, which is the screen on a computer, over the Desktop.
mkdir -p "$data/openshell/session-logs"
"$root/bin/openshell-session" "$config" >> "$data/openshell/session-logs/supervisor.log" 2>&1 &
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
        # A copy still being made stops (the next boot from memory starts it
        # again): the data partition must be unmounted clean.
        if [ -n "${disk_root_job:-}" ]; then
            kill "$disk_root_job" 2> /dev/null || true
            killall cpio gzip 2> /dev/null || true
            sleep 1
        fi
        # The system shut down cleanly from its copy: it works.
        if [ -n "$disk_root" ]; then disk_root_good || true; fi
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
