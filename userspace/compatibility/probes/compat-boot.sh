#!/bin/sh
set -eu

cd /opt/openshell-compat
test -d /dev/pts
grep -q '^/dev/vda /ext2 ext2 ' /proc/mounts
export OPENSHELL_PROBE_TMP=/ext2
export OPENSHELL_PROBE_TLS=/opt/openshell-compat/tls
python_status=0
node_status=0
./python -I ./python.py || python_status=$?
./node ./node.mjs || node_status=$?
if [ "$node_status" -ne 0 ]; then
    ./python -I ./inotify.py /ext2 /tmp || true
fi
if ! ./seccomp-probe; then
    echo OPENSHELL_NATIVE_SECCOMP_UNAVAILABLE
fi
if ! ./python -I ./namespaces.py; then
    echo OPENSHELL_NATIVE_NAMESPACES_UNAVAILABLE
fi
# The same checks that Chromium runs before it starts its zygote.
if ! ./python -I ./unprivileged.py ./chromium-userns; then
    echo OPENSHELL_NATIVE_CHROMIUM_USERNS_UNAVAILABLE
fi
hostd_status=0
if [ -d /opt/openshell ]; then
    # The real hostd of the bundle runs as an unprivileged user with a disposable profile.
    scratch=/tmp/openshell-desktop
    mkdir -p "$scratch"
    chown 65534:65534 "$scratch"
    PATH="/opt/openshell-compat/bin:$PATH" TMPDIR="$scratch" ./python -I ./unprivileged.py \
        ./node ./hostd-smoke.mjs /opt/openshell "$scratch/out" || hostd_status=$?
    cat "$scratch"/out/hostd-*/hostd-smoke.json 2>/dev/null || true
    if [ "$hostd_status" -ne 0 ]; then
        tail -n 40 "$scratch"/out/hostd-*/hostd.log 2>/dev/null || true
    fi
    if [ "$hostd_status" -eq 0 ]; then
        echo OPENSHELL_NATIVE_HOSTD_OK
    fi
fi
cef_status=0
if [ -e /opt/openshell-cef ]; then
    # An X server, a window manager and the CEF host as an unprivileged user.
    scratch=/tmp/openshell-graphics
    chmod 1777 /tmp
    mkdir -p "$scratch" /tmp/.X11-unix /dev/shm
    chmod 1777 /tmp/.X11-unix
    chown 65534:65534 "$scratch"
    if ! grep -q ' /dev/shm ' /proc/mounts; then
        mount -t tmpfs -o mode=1777 shm /dev/shm || true
    fi
    graphics_path="/opt/openshell-compat/bin:$PATH"
    export FONTCONFIG_FILE=/opt/openshell-compat/fonts.conf
    PATH="$graphics_path" Xvfb :0 -screen 0 1280x800x24 -nolisten tcp -ac > "$scratch/xvfb.log" 2>&1 &
    xserver=$!
    tries=0
    while [ ! -S /tmp/.X11-unix/X0 ] && [ "$tries" -lt 120 ]; do
        sleep 0.5
        tries=$((tries + 1))
    done
    if [ -S /tmp/.X11-unix/X0 ]; then
        echo OPENSHELL_NATIVE_X11_OK
        DISPLAY=:0 HOME="$scratch" XDG_CONFIG_DIRS=/opt/openshell-compat/xdg PATH="$graphics_path" \
            ./python -I ./unprivileged.py /opt/openshell-compat/bin/openbox > "$scratch/openbox.log" 2>&1 &
        wm=$!
        sleep 2
        if ! kill -0 "$wm" 2>/dev/null; then
            echo "Openbox exited early:"
            tail -n 20 "$scratch/openbox.log"
        fi
        # Chromium starts slowly in an emulated guest.
        DISPLAY=:0 HOME="$scratch" PATH="$graphics_path" OPENSHELL_CEF_DIAGNOSTICS=1 \
            OPENSHELL_CEF_SMOKE_TIMEOUT_MS=300000 ./python -I ./unprivileged.py \
            ./node ./cef-smoke.mjs host /opt/openshell-cef/openshell-cef-host "$scratch/out" || cef_status=$?
        cat "$scratch"/out/cef-*/cef-smoke.json 2>/dev/null || true
        if [ "$cef_status" -eq 0 ]; then
            echo OPENSHELL_NATIVE_CEF_OK
        else
            grep -v file_path_watcher "$scratch"/out/cef-*/cef.log 2>/dev/null | tail -n 200 || true
        fi
        kill "$wm" 2>/dev/null || true
    else
        cef_status=1
        tail -n 40 "$scratch/xvfb.log"
    fi
    kill "$xserver" 2>/dev/null || true
fi
if [ "$python_status" -ne 0 ] || [ "$node_status" -ne 0 ] || [ "$hostd_status" -ne 0 ] || [ "$cef_status" -ne 0 ]; then
    exit 1
fi
echo 'Successfully booted.'
