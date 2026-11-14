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
    members="MANIFEST MANIFEST.sig kernel initramfs.cpio.gz boot.cfg"
    # Nothing in the inbox is trusted before openshell-ab: each member must
    # come out as a plain file, never a link or anything else the archive
    # could make of it.
    if [ -z "$result" ] && [ -z "$other" ]; then
        result="failed the system runs from no known slot"
    fi
    if [ -z "$result" ]; then
        if tar -xf "$updates_dir/inbox/$name" -C "$work" $members; then
            for member in $members; do
                if [ -L "$work/$member" ] || [ ! -f "$work/$member" ]; then
                    result="failed the update's $member is not a plain file"
                fi
            done
        else
            result="failed the update cannot be unpacked"
        fi
    fi
    if [ -z "$result" ] && mount -t ext2 "${device%5}$other" "$target"; then
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
# The firewall (userspace/shell/src/firewall.rs): the rules the supervisor
# keeps go to the kernel before anything listens, when it was left on.
if [ -n "${settings:-}" ] && [ -e "$data/openshell/system/firewall.on" ] && [ -e /proc/net/firewall ]; then
    cat "$data/openshell/system/firewall.rules" > /proc/net/firewall || echo "the firewall's rules were not taken" >&2
fi
# Wi-Fi (userspace/shell/src/wifi.rs): wpa_supplicant runs the kernel's Wi-Fi
# interfaces, which the supervisor hands to it when the session asks. Its
# control sockets are root's alone: whoever reaches them tells it which
# files to read and which plug-ins to load. It starts again if it ends; its
# log is this boot's.
wifi=
if [ -x "$root/bin/wpa_supplicant" ]; then
    mkdir -p /run/openshell-wifi/interfaces
    chmod 0700 /run/openshell-wifi /run/openshell-wifi/interfaces
    : > "$data/openshell/session-logs/wifi.log"
    (
        while :; do
            # What it writes (the saved networks, with their passwords) is
            # for root alone.
            (umask 077; exec "$root/bin/wpa_supplicant" -g /run/openshell-wifi/global -t) >> "$data/openshell/session-logs/wifi.log" 2>&1
            rm -f /run/openshell-wifi/global
            sleep 2
        done
    ) &
    wifi=$!
fi
# Remote access by SSH (userspace/shell/src/ssh.rs): OpenSSH's server runs
# while the supervisor keeps its mark, which the session asks for in
# Settings. An installed system only: the account's password is what lets
# someone in. The host keys are made once and stay on the data partition, so
# the computer keeps its identity; the server's log is this boot's.
ssh_run=/run/openshell-ssh
ssh_keys=$data/openshell/system/ssh
# What the server needs before it starts; fails where something is missing.
ssh_prepare() {
    mkdir -p "$ssh_keys" /var/empty
    chmod 0700 "$ssh_keys"
    chmod 0755 /var/empty
    for kind in ed25519 rsa; do
        if [ ! -s "$ssh_keys/ssh_host_${kind}_key" ]; then
            rm -f "$ssh_keys/ssh_host_${kind}_key" "$ssh_keys/ssh_host_${kind}_key.pub"
            "$root/bin/ssh-keygen" -q -t "$kind" -N '' -C openshell -f "$ssh_keys/ssh_host_${kind}_key" || return 1
        fi
    done
    # The keys' fingerprints, which the supervisor gives the Desktop.
    for kind in ed25519 rsa; do
        "$root/bin/ssh-keygen" -lf "$ssh_keys/ssh_host_${kind}_key.pub" || return 1
    done > "$ssh_run/fingerprints.new"
    mv "$ssh_run/fingerprints.new" "$ssh_run/fingerprints"
    # The server's own user: the part of it that listens to someone who has
    # not signed in yet runs as this one, in an empty folder.
    grep -q '^sshd:' /etc/passwd || echo 'sshd:x:74:74:SSH server:/var/empty:/bin/false' >> /etc/passwd
    grep -q '^sshd:' /etc/group || echo 'sshd:x:74:' >> /etc/group
    # The account as the server looks for it: the session's home and a
    # shell. Without a password only keys let in (~/.ssh/authorized_keys).
    account=$(awk -F: '$3 == 65534 { print $1; exit }' /etc/passwd)
    [ -n "$account" ] || return 1
    account_shell=/bin/sh
    if [ -x /bin/bash ]; then account_shell=/bin/bash; fi
    if ! awk -F: -v home="$state/home" -v shell="$account_shell" '$3 == 65534 && $6 == home && $7 == shell { found = 1 } END { exit !found }' /etc/passwd; then
        awk -F: -v OFS=: -v home="$state/home" -v shell="$account_shell" '$3 == 65534 { $6 = home; $7 = shell } { print }' /etc/passwd > /etc/passwd.ssh || return 1
        mv /etc/passwd.ssh /etc/passwd
    fi
    # The password is checked as the lock screen checks it (login.rs): the
    # supervisor's SHA-512 crypt hash, against BusyBox's cryptpw. This
    # OpenSSH checks passwords only through PAM, which hands the password to
    # this script on its input (pam_exec); account and session need nothing
    # more. /etc/shadow is left alone.
    [ -d "$root/pam" ] || return 1
    cat > "$ssh_run/check-password" <<EOF
#!/bin/sh
[ "\$PAM_USER" = "$account" ] || exit 1
stored=\$(head -n1 "$data/openshell/account/password" 2> /dev/null)
case "\$stored" in
    '\$6\$'*) ;;
    *) exit 1 ;;
esac
salt=\${stored#\\\$6\\\$}
salt=\${salt%%\\\$*}
computed=\$(tr -d '\\000' | /bin/cryptpw -m sha512 -P 0 -S "\$salt") || exit 1
[ -n "\$computed" ] && [ "\$computed" = "\$stored" ]
EOF
    chmod 0700 "$ssh_run/check-password"
    mkdir -p /etc/pam.d
    cat > /etc/pam.d/sshd <<EOF
auth required $root/pam/pam_exec.so quiet expose_authtok $ssh_run/check-password
auth optional $root/pam/pam_permit.so
account required $root/pam/pam_permit.so
password required $root/pam/pam_deny.so
session required $root/pam/pam_permit.so
EOF
    # IPv4 only: the kernel's sockets do not listen on every IPv6 address yet.
    cat > "$ssh_run/sshd_config" <<EOF
Port 22
AddressFamily inet
ListenAddress 0.0.0.0
HostKey $ssh_keys/ssh_host_ed25519_key
HostKey $ssh_keys/ssh_host_rsa_key
PidFile $ssh_run/sshd.pid
UsePAM yes
PermitRootLogin no
AllowUsers $account
PasswordAuthentication yes
KbdInteractiveAuthentication no
PermitEmptyPasswords no
AuthorizedKeysFile .ssh/authorized_keys
UseDNS no
PrintMotd no
Subsystem sftp internal-sftp
SetEnv PATH=$root/bin:/bin:/usr/bin
EOF
}
ssh_service=
if [ -n "${settings:-}" ] && [ -x "$root/bin/sshd" ]; then
    # Nothing of an earlier boot (the system's copy on the disk keeps /run).
    rm -rf "$ssh_run"
    mkdir -p "$ssh_run"
    chmod 0755 "$ssh_run"
    (
        set +e
        while :; do
            if [ -e "$data/openshell/system/ssh.on" ]; then
                if [ ! -e "$ssh_run/running" ]; then
                    if ssh_prepare; then
                        : > "$ssh_run/running"
                        # "running" lasts as long as the server does.
                        (
                            "$root/bin/sshd" -D -e -f "$ssh_run/sshd_config"
                            rm -f "$ssh_run/running" "$ssh_run/sshd.pid"
                        ) &
                    fi
                    # One that ends at once starts again, slowly.
                    sleep 5
                fi
            elif [ -e "$ssh_run/running" ]; then
                # Turned off: the server stops; who is signed in stays, as
                # with Live's service.
                kill "$(cat "$ssh_run/sshd.pid" 2> /dev/null)" 2> /dev/null || killall sshd 2> /dev/null
                sleep 1
            fi
            sleep 1
        done
    ) >> "$data/openshell/session-logs/ssh.log" 2>&1 &
    ssh_service=$!
fi
# The computer's name, as Debian's live system has one: the name its DHCP
# requests already carry (the kernel's), which it knows itself by through
# /etc/hosts (programs that look up their own name get an answer at once)
# and which Avahi publishes as <name>.local.
case "$(hostname 2> /dev/null)" in
    '' | '(none)' | localhost) hostname openshell 2> /dev/null || echo openshell > /proc/sys/kernel/hostname ;;
esac
host_name=$(hostname 2> /dev/null)
if [ -n "$host_name" ] && ! grep -q "[[:space:]]$host_name\$" /etc/hosts 2> /dev/null; then
    # On a line of its own (the image's file has no last newline).
    {
        [ -s /etc/hosts ] && [ -n "$(tail -c 1 /etc/hosts)" ] && echo
        printf '127.0.1.1\t%s\n' "$host_name"
    } >> /etc/hosts
fi
# Service discovery (mDNS/DNS-SD), as on Live: the D-Bus system bus and
# Avahi, through which CUPS's tools find the network's printers (Settings ▸
# Printers lists them, `lpstat -e`) and which makes this computer known as
# <name>.local. Each runs as its own user, as on Debian, and starts again if
# it ends; Avahi follows interfaces and addresses as they come (the
# kernel's route notifications).
discovery_service=
name_service=
if [ -x "$root/bin/dbus-daemon" ] && [ -x "$root/bin/avahi-daemon" ] && [ -f /etc/dbus-1/system.conf ]; then
    grep -q '^messagebus:' /etc/passwd || echo 'messagebus:x:71:71:D-Bus system bus:/run/dbus:/bin/false' >> /etc/passwd
    grep -q '^messagebus:' /etc/group || echo 'messagebus:x:71:' >> /etc/group
    grep -q '^avahi:' /etc/passwd || echo 'avahi:x:70:70:Avahi mDNS/DNS-SD:/run/avahi-daemon:/bin/false' >> /etc/passwd
    grep -q '^avahi:' /etc/group || echo 'avahi:x:70:' >> /etc/group
    # Avahi's bus policy names Debian's group for network administrators.
    grep -q '^netdev:' /etc/group || echo 'netdev:x:72:' >> /etc/group
    # The bus and Avahi read this computer's identity.
    [ -s /etc/machine-id ] || "$root/bin/dbus-uuidgen" --ensure=/etc/machine-id
    # /var/run is /run, as on Debian: nss-mdns asks Avahi at
    # /var/run/avahi-daemon/socket, glibc the name service at
    # /var/run/nscd/socket.
    [ -e /var/run ] || ln -s /run /var/run
    # Nothing of an earlier boot (sockets, pid files).
    rm -rf /run/dbus /run/avahi-daemon
    mkdir -p /run/dbus /run/avahi-daemon
    chown avahi:avahi /run/avahi-daemon
    : > "$data/openshell/session-logs/discovery.log"
    # <name>.local for every program: glibc asks the name service daemon
    # (its socket in /var/run/nscd), which alone loads nss-mdns.
    if [ -x "$root/bin/nsncd" ] && [ -d "$root/nss" ]; then
        mkdir -p /var/run/nscd
        (
            set +e
            while :; do
                LD_LIBRARY_PATH="$root/nss" "$root/bin/nsncd"
                sleep 5
            done
        ) >> "$data/openshell/session-logs/discovery.log" 2>&1 &
        name_service=$!
    fi
    (
        set +e
        (
            while :; do
                # The file is the system bus's (`<type>system</type>`);
                # `--system` would ask for it a second time.
                "$root/bin/dbus-daemon" --nofork --nopidfile --config-file=/etc/dbus-1/system.conf
                rm -f /run/dbus/system_bus_socket
                sleep 5
            done
        ) &
        bus_loop=$!
        # Asked to end (the session ends): the bus's loop too.
        trap 'kill "$bus_loop" 2> /dev/null; exit 0' TERM
        while :; do
            for i in $(seq 1 100); do
                [ -S /run/dbus/system_bus_socket ] && break
                sleep 0.2
            done
            "$root/bin/avahi-daemon" --no-rlimits
            rm -f /run/avahi-daemon/pid
            sleep 5
        done
    ) >> "$data/openshell/session-logs/discovery.log" 2>&1 &
    discovery_service=$!
fi
# Printing: CUPS's scheduler, for this computer's programs only (the
# loopback address and its socket), with the session's person as its
# administrator, who adds and removes printers in Settings: nobody else has
# an account here. Printers and their settings stay on the data partition
# (a live session's last until it ends). It starts again if it ends; its log
# is this boot's.
cups_service=
if [ -x "$root/bin/cupsd" ] && [ -d "$root/cups/lib/cups" ]; then
    if [ -n "${settings:-}" ]; then
        cups_state=$data/openshell/system/cups
    else
        cups_state=/tmp/openshell-cups
    fi
    # Nothing of an earlier boot (its socket, its certificates).
    rm -rf /run/cups
    mkdir -p "$cups_state" /run/cups /var/spool/cups/tmp /var/cache/cups
    chmod 0755 "$cups_state" /run/cups
    # The jobs' folder and its scratch space, as CUPS keeps them: the
    # scheduler refuses to start without them.
    chgrp nogroup /var/spool/cups /var/spool/cups/tmp /var/cache/cups
    chmod 0710 /var/spool/cups
    chmod 1770 /var/spool/cups/tmp
    chmod 0770 /var/cache/cups
    # CUPS's tools look for the scheduler's socket and their settings in
    # /etc/cups, and the scheduler keeps its files there.
    if [ ! -L /etc/cups ]; then
        rm -rf /etc/cups
        ln -s "$cups_state" /etc/cups
    fi
    cat > "$cups_state/client.conf" <<EOF
ServerName /run/cups/cups.sock
EOF
    # The programs that make a job into what the printer takes, and the
    # ones that send it, run as the session's user.
    cat > "$cups_state/cups-files.conf" <<EOF
SystemGroup root
ServerRoot $cups_state
ServerBin $root/cups/lib/cups
DataDir $root/cups/share/cups
StateDir /run/cups
CacheDir /var/cache/cups
RequestRoot /var/spool/cups
TempDir /var/spool/cups/tmp
AccessLog stderr
ErrorLog stderr
PageLog stderr
SetEnv PATH $root/cups/lib/cups/filter:$root/cups/bin:$root/bin:/bin:/usr/bin
SetEnv FONTCONFIG_FILE $root/fonts.conf
User nobody
Group nogroup
FileDevice No
EOF
    cat > "$cups_state/cupsd.conf" <<EOF
Listen 127.0.0.1:631
Listen /run/cups/cups.sock
LogLevel warn
AccessLogLevel none
DefaultShared No
Browsing No
BrowseLocalProtocols none
WebInterface No
DefaultAuthType None
<Location />
  Order allow,deny
  Allow localhost
</Location>
<Location /admin>
  Order allow,deny
  Allow localhost
</Location>
<Policy default>
  JobPrivateAccess default
  JobPrivateValues default
  SubscriptionPrivateAccess default
  SubscriptionPrivateValues default
  <Limit All>
    Order deny,allow
  </Limit>
</Policy>
EOF
    : > "$data/openshell/session-logs/cups.log"
    (
        set +e
        while :; do
            "$root/bin/cupsd" -f -c "$cups_state/cupsd.conf" -s "$cups_state/cups-files.conf" >> "$data/openshell/session-logs/cups.log" 2>&1 &
            cupsd=$!
            # The socket is everyone's, as systemd's socket unit makes it on
            # Debian: the scheduler leaves it as its umask made it, root's
            # alone, and the session's tools could not reach it.
            for i in $(seq 1 100); do
                [ -S /run/cups/cups.sock ] && break
                kill -0 "$cupsd" 2> /dev/null || break
                sleep 0.2
            done
            chmod 0666 /run/cups/cups.sock 2> /dev/null
            wait "$cupsd"
            rm -f /run/cups/cups.sock
            sleep 5
        done
    ) &
    cups_service=$!
fi
"$root/bin/openshell-session" "$config" >> "$data/openshell/session-logs/supervisor.log" 2>&1 &
supervisor=$!
trap 'kill -TERM "$supervisor" 2>/dev/null || true' HUP INT TERM
status=0
wait "$supervisor" || status=$?
if kill -0 "$supervisor" 2>/dev/null; then
    wait "$supervisor" || status=$?
fi
# The session ended: so does the supplicant, which leaves its network.
if [ -n "$wifi" ]; then
    kill "$wifi" 2>/dev/null || true
    killall wpa_supplicant 2>/dev/null || true
fi
# And the SSH server, with whoever is signed in, and the printing scheduler.
if [ -n "$ssh_service" ]; then
    kill "$ssh_service" 2>/dev/null || true
    killall sshd sshd-session 2>/dev/null || true
    rm -f "$ssh_run/running" "$ssh_run/sshd.pid"
fi
if [ -n "$cups_service" ]; then
    kill "$cups_service" 2>/dev/null || true
    killall cupsd 2>/dev/null || true
fi
if [ -n "$discovery_service" ]; then
    # Its loop ends once the daemon it waits for does.
    kill "$discovery_service" 2>/dev/null || true
    killall avahi-daemon 2>/dev/null || true
    sleep 0.5
    killall dbus-daemon 2>/dev/null || true
fi
if [ -n "$name_service" ]; then
    kill "$name_service" 2>/dev/null || true
    killall nsncd 2>/dev/null || true
fi
# The session ended otherwise (it failed): the console in text mode again,
# with its keyboard, as openshell-x --seat left it for X (KDSETMODE
# KD_TEXT, KDSKBMODE K_UNICODE), so what is written there shows.
case "$status" in
    64|65) ;;
    *)
        if [ -x "$root/python" ] && [ -e /dev/tty1 ]; then
            "$root/python" -I -c '
import fcntl, os
try:
    fd = os.open("/dev/tty1", os.O_RDWR | os.O_NOCTTY)
    fcntl.ioctl(fd, 0x4B3A, 0)
    fcntl.ioctl(fd, 0x4B45, 3)
    os.close(fd)
except OSError:
    pass
' 2> /dev/null || true
        fi
        ;;
esac
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
