#!/bin/sh
# The session's X server (openshell-session's `x=`): Xorg on the computer's
# own screen when the kernel offers DRM (simpledrm over the UEFI framebuffer),
# with the evdev driver on each input device (libinput needs udev); Xvfb
# otherwise, as in the QEMU smokes.
#
#   openshell-x <display> [Xvfb arguments...]
#
# It runs as the session user: `openshell-x --seat <user>` (as root, before
# the session) hands that user the screen, the input devices and the console,
# as logind does for the active seat. No udev: the devices present at start
# are the session's. OPENSHELL_X=xvfb keeps Xvfb.
#
# initramfs.nix sets OPENSHELL_XORG, OPENSHELL_XORG_MODULES, OPENSHELL_XORG_FONTS
# and OPENSHELL_XVFB to the store paths.
set -eu

card=/dev/dri/card0

if [ "${1:-}" = --seat ]; then
    user=$2
    for device in "$card" /dev/dri/renderD* /dev/fb0 /dev/input/event* /dev/tty0 /dev/tty1; do
        if [ -e "$device" ]; then
            chown "$user" "$device"
            chmod 0600 "$device"
        fi
    done
    exit 0
fi

display=$1
shift
if [ "${OPENSHELL_X:-auto}" = xvfb ] || [ ! -e "$card" ] || [ ! -r "$card" ]; then
    exec "$OPENSHELL_XVFB" "$display" "$@"
fi

runtime=${TMPDIR:-/tmp}/openshell-xorg-$(id -u)
mkdir -p "$runtime"
config=$runtime/xorg.conf
{
    cat <<'EOF'
Section "ServerFlags"
    Option "AutoAddDevices" "false"
    Option "AutoEnableDevices" "false"
    Option "DontVTSwitch" "true"
    Option "DontZap" "true"
    Option "BlankTime" "0"
    Option "StandbyTime" "0"
    Option "SuspendTime" "0"
    Option "OffTime" "0"
EndSection

Section "Device"
    Identifier "card"
    Driver "modesetting"
    Option "kmsdev" "/dev/dri/card0"
    Option "AccelMethod" "none"
    Option "ShadowFB" "true"
EndSection

Section "Screen"
    Identifier "screen"
    Device "card"
EndSection
EOF
    inputs=
    index=0
    # Scrolling: the kernel drives touchpads only as PS/2 mice, which send no
    # wheel. Holding the right button while moving scrolls (evdev's wheel
    # emulation, both ways); a press without moving is still a right click.
    # A mouse with a wheel scrolls with it as well. Keyboards ignore these options.
    scrolling='    Option "EmulateWheel" "true"\n    Option "EmulateWheelButton" "3"\n    Option "EmulateWheelTimeout" "250"\n    Option "XAxisMapping" "6 7"\n    Option "YAxisMapping" "4 5"\n'
    for device in /dev/input/event*; do
        [ -r "$device" ] || continue
        index=$((index + 1))
        printf 'Section "InputDevice"\n    Identifier "input%s"\n    Driver "evdev"\n    Option "Device" "%s"\n%bEndSection\n\n' "$index" "$device" "$scrolling"
        inputs="$inputs    InputDevice \"input$index\"\n"
    done
    printf 'Section "ServerLayout"\n    Identifier "openshell"\n    Screen "screen"\n%b    Option "AllowEmptyInput" "true"\nEndSection\n' "$inputs"
} > "$config"

# The console as it is: no VT of its own, no switching. -background none
# leaves the boot splash on the screen where the driver can, until the
# desktop's boot screen draws.
exec "$OPENSHELL_XORG" "$display" -config "$config" -modulepath "$OPENSHELL_XORG_MODULES" -fp "$OPENSHELL_XORG_FONTS" \
    -logfile "$runtime/Xorg.log" -nolisten tcp -ac -noreset -background none -sharevts -novtswitch -keeptty vt1
