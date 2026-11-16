# The report a person who tries the live medium on a computer brings back,
# OSNDIAG.TXT on the stick's own FAT partition (the ESP that build-media.sh
# appends as partition 2), which Windows, macOS and phones read: what the
# kernel found (each PCI device, as `lspci -nn` names it, the USB devices,
# the touchpad, the Wi-Fi card and what its driver said), the network
# interfaces, and the Wi-Fi service's and the session's logs. A copy stays in
# the session's logs (osndiag.txt). installed-session.sh runs it in live mode,
# once the session has started and again later; each run replaces the file.
# No passwords or keys go in; the Wi-Fi log names the networks around.
#
#   sh diagnostics.sh <session logs directory> <seconds to wait first>...
set -u

logs=$1
shift
media=${OPENSHELL_MEDIA:-/opt/openshell-media}
# whole_disks, live_medium and partition, as the installer finds the stick.
. "$media/medium.sh"

section() {
    printf '\n===== %s\n' "$1"
}

# /dev/kmsg's records (<level>,<seq>,<microseconds>,-;<text>) as dmesg prints them.
kernel_log() {
    if command -v timeout > /dev/null 2>&1; then
        timeout 5 cat /dev/kmsg
    else
        cat /dev/kmsg
    fi 2> /dev/null | awk '{
        split($0, head, ";"); split(head[1], field, ",")
        printf "[%9.3f] %s\n", field[3] / 1000000, substr($0, index($0, ";") + 1)
    }'
}

pci_devices() {
    kernel_log | grep -E '[0-9a-f]{2}:[0-9a-f]{2}\.[0-7] \[[0-9a-f]{4}\] [0-9a-f]{4}:[0-9a-f]{4} ' |
        sed 's/^\[[ 0-9.]*\] //'
}

report() {
    echo "OpenShell Native: diagnostic report"
    echo "Send this file to whoever asked for it; it has no passwords."
    section "summary"
    # PCI class 0280: a network controller that is not Ethernet, the Wi-Fi cards.
    cards=$(pci_devices | grep -F '[0280]' | sed 's/^pci: //')
    echo "Wi-Fi cards (PCI): ${cards:-none}"
    interfaces=$(awk -F: 'NR > 2 { gsub(/ /, "", $1); if ($1 ~ /^wl/) print $1 }' /proc/net/dev 2> /dev/null | tr '\n' ' ')
    echo "Wi-Fi interfaces: ${interfaces:-none}"
    echo "PS/2 pointer: $(kernel_log | grep -F 'i8042: ' | grep -v -i keyboard | sed 's/^\[[ 0-9.]*\] i8042: //' | tr '\n' ';')"
    echo "I2C touchpad: $(kernel_log | grep -F 'HID-over-I2C' | sed 's/^\[[ 0-9.]*\] //')"
    section "system"
    cat /proc/version 2> /dev/null
    echo "uptime (s): $(cut -d' ' -f1 /proc/uptime 2> /dev/null)"
    # The command line, without the release's manifest and signature.
    tr ' ' '\n' < /proc/cmdline 2> /dev/null | grep -v '^openshell.manifest' | tr '\n' ' '
    echo
    head -n 3 /proc/meminfo 2> /dev/null
    section "PCI devices: bus:dev.fn [class] vendor:device (the kernel's)"
    pci_devices
    section "USB devices"
    for device in /sys/bus/usb/devices/*; do
        [ -r "$device/idVendor" ] || continue
        printf '%s %s:%s class %s %s %s\n' "${device##*/}" \
            "$(cat "$device/idVendor")" "$(cat "$device/idProduct" 2> /dev/null)" \
            "$(cat "$device/bDeviceClass" 2> /dev/null)" \
            "$(cat "$device/manufacturer" 2> /dev/null)" "$(cat "$device/product" 2> /dev/null)"
    done
    section "input devices"
    for device in /sys/class/input/*; do
        [ -r "$device/name" ] && echo "${device##*/}: $(cat "$device/name")"
    done
    cat /proc/bus/input/devices 2> /dev/null
    section "touchpad and mouse (the kernel's lines)"
    kernel_log | grep -i -E 'i8042|touchpad|synaptics|elan|alps|HID-over-I2C|mouse'
    section "network interfaces"
    cat /proc/net/dev 2> /dev/null
    cat /proc/net/wireless 2> /dev/null
    for device in /sys/class/rfkill/*; do
        [ -r "$device/type" ] && echo "rfkill ${device##*/}: $(cat "$device/type") soft=$(cat "$device/soft" 2> /dev/null) hard=$(cat "$device/hard" 2> /dev/null)"
    done
    ls -l /run/openshell-wifi /run/openshell-wifi/interfaces 2> /dev/null
    section "Wi-Fi (the kernel's lines)"
    kernel_log | grep -i -E 'wlan|wi-fi|wifi|iwlwifi|mt79|rtw8|ath1|brcm|firmware|80211'
    section "Wi-Fi service (wpa_supplicant), last 150 lines"
    tail -n 150 "$logs/wifi.log" 2> /dev/null
    section "session supervisor, last 80 lines"
    tail -n 80 "$logs/supervisor.log" 2> /dev/null
    section "X server's input lines"
    grep -i -E 'input|evdev|mouse|touchpad|keyboard' /tmp/openshell-xorg-*/Xorg.log 2> /dev/null | tail -n 60
    section "kernel log"
    kernel_log
}

# The stick this system started from, its FAT partition.
write_to_stick() {
    mount_point=/run/openshell-report
    for disk in $(whole_disks); do
        live_medium "$disk" || continue
        esp=$(partition "$disk" 2)
        [ -b "$esp" ] || continue
        mkdir -p "$mount_point"
        mount -t vfat "$esp" "$mount_point" 2> /dev/null || continue
        cp "$logs/osndiag.txt" "$mount_point/OSNDIAG.TXT" && written=$disk
        sync
        umount "$mount_point" 2> /dev/null || umount -l "$mount_point" 2> /dev/null
        [ -n "${written:-}" ] && return 0
    done
    return 1
}

for wait in "$@"; do
    sleep "$wait"
    report > "$logs/osndiag.txt" 2>&1
    written=
    if write_to_stick; then
        echo "diagnostics: OSNDIAG.TXT written to $written"
    else
        echo "diagnostics: no live stick to write OSNDIAG.TXT to (kept in $logs/osndiag.txt)"
    fi
done
