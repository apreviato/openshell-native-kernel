#!/usr/bin/env bash
set -euo pipefail

action=$1
repository=$2
tag=$3
revision=$4
expected_rust=$5

work=/tmp/openshell-native-work
artifacts=/tmp/openshell-native-out
mkdir -p "$work" "$artifacts"
cd "$work"

git init --quiet source
cd source
timeout 10m git fetch --no-tags --depth=1 "$repository" "refs/tags/$tag"
git checkout --quiet --detach FETCH_HEAD
actual_revision=$(git rev-parse HEAD)
if [[ "$actual_revision" != "$revision" ]]; then
    printf 'Expected upstream commit %s, got %s\n' "$revision" "$actual_revision" >&2
    exit 1
fi

rust_channel=$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)
if [[ "$rust_channel" != "$expected_rust" ]]; then
    printf 'Expected Rust channel %s, got %s\n' "$expected_rust" "$rust_channel" >&2
    exit 1
fi

export CARGO_TARGET_DIR="$work/target"
export NIX_CONFIG="${NIX_CONFIG:-}"$'\nsandbox = false'
compat=/tmp/openshell-native-compat
probes="$compat/probes"
boot_args=()
boot_timeout=10m

bundle=/tmp/openshell-native-bundle
shell_sources=/tmp/openshell-native-shell
ab_sources=/tmp/openshell-native-ab
image_sources=/tmp/openshell-native-image

# What each action builds and runs. apps is the kiosk with the Linux apps
# profile; wine adds the Windows apps profile (OPENSHELL_WINE.md) to it.
uses_bundle() { [[ "$action" == desktop || "$action" == kiosk || "$action" == apps || "$action" == wine ]]; }
uses_graphics() { [[ "$action" == graphics || "$action" == kiosk || "$action" == apps || "$action" == wine ]]; }
drives_desktop() { [[ "$action" == kiosk || "$action" == apps || "$action" == wine ]]; }
uses_apps() { [[ "$action" == apps || "$action" == wine ]]; }
uses_wine() { [[ "$action" == wine ]]; }

prepare_compat() {
    install -m 0644 "$compat/initramfs.nix" test/initramfs/nix/openshell-compat.nix
    rm -rf test/initramfs/nix/openshell-probes test/initramfs/nix/openshell-bundle \
        test/initramfs/nix/openshell-shell test/initramfs/nix/openshell-ab test/initramfs/nix/openshell-apps \
        test/initramfs/nix/openshell-wine
    cp -R "$probes" test/initramfs/nix/openshell-probes
    cp -R "$ab_sources" test/initramfs/nix/openshell-ab
    # The Desktop profiles add the OpenShell bundle to the initramfs.
    if uses_bundle; then
        cp -R "$bundle" test/initramfs/nix/openshell-bundle
    fi
    # The graphics profiles build the shell and add an X server.
    if uses_graphics; then
        cp -R "$shell_sources" test/initramfs/nix/openshell-shell
    fi
    # The apps profile adds Xpra and the probe applications, and the wine
    # profile Wine (initramfs.nix).
    if uses_apps; then
        mkdir test/initramfs/nix/openshell-apps
    fi
    if uses_wine; then
        cp -R "$compat/wine" test/initramfs/nix/openshell-wine
    fi
}

# Runs the CEF smokes, and the Desktop shell smoke for the kiosk, on the
# builder's Linux with the packages of the guest, as an unprivileged user in
# Xvfb and Openbox. The container must allow unprivileged user namespaces.
run_control_graphics() {
    local scratch="$work/control-graphics" status=0 attribute
    for attribute in cefHost launcher mesa fonts graphicsTools node python; do
        if ! timeout --kill-after=15s 60m nix-build test/initramfs/nix/openshell-compat.nix \
            -A "$attribute" -o "$work/control-$attribute" >> "$artifacts/control-graphics-build.log" 2>&1; then
            record_check control-cef failed
            return 1
        fi
    done
    # Xvfb and libglvnd find Mesa where NixOS installs it.
    mkdir -p /run /tmp/.X11-unix
    ln -sfn "$(readlink -f "$work/control-mesa")" /run/opengl-driver
    chmod 1777 /tmp /tmp/.X11-unix
    install -d -m 0755 -o 65534 -g 65534 "$scratch"
    local tools="$work/control-graphicsTools/bin"
    local path="$tools:$work/control-node/bin:$work/control-python/bin:$PATH"
    "$tools/Xvfb" :7 -screen 0 1280x800x24 -nolisten tcp -ac > "$artifacts/control-xvfb.log" 2>&1 &
    local xserver=$!
    sleep 3
    DISPLAY=:7 HOME="$scratch" XDG_CONFIG_DIRS="$work/control-graphicsTools/etc/xdg" \
        "$work/control-python/bin/python3" -I "$probes/unprivileged.py" "$tools/openbox" \
        > "$artifacts/control-openbox.log" 2>&1 &
    local wm=$!
    sleep 2
    # The Linux apps profile's environment, for the smokes of the apps action.
    local apps_env=()
    control_smoke() {
        local name=$1
        shift
        if DISPLAY=:7 HOME="$scratch" PATH="$path" FONTCONFIG_FILE="$work/control-fonts" \
            OPENSHELL_CEF_SMOKE_TIMEOUT_MS=90000 env "${apps_env[@]}" timeout --kill-after=10s 15m \
            "$work/control-python/bin/python3" -I "$probes/unprivileged.py" \
            "$work/control-node/bin/node" "$@" "$scratch/$name" 2>&1 | tee "$artifacts/$name.log"; then
            record_check "$name" passed
        else
            record_check "$name" failed
            status=1
        fi
        cp -R "$scratch/$name" "$artifacts/$name" 2>/dev/null || true
    }
    local host="$work/control-cefHost/runtime/openshell-cef-host"
    control_smoke control-cef "$probes/cef-smoke.mjs" host "$host"
    control_smoke control-cef-isolation "$probes/cef-smoke.mjs" isolation "$host"
    if uses_apps; then
        if ! timeout --kill-after=15s 60m nix-build test/initramfs/nix/openshell-compat.nix \
            -A appsTools -o "$work/control-appsTools" >> "$artifacts/control-graphics-build.log" 2>&1; then
            record_check control-apps failed
            return 1
        fi
        local apps="$work/control-appsTools"
        # The image's layout (initramfs.nix): where hostd and D-Bus look, as on Debian.
        mkdir -p /usr/share/xpra /etc/dbus-1
        ln -sfn "$(readlink -f "$apps/share/xpra/www")" /usr/share/xpra/www
        ln -sfn "$(readlink -f "$apps/etc/dbus-1/session.conf")" /etc/dbus-1/session.conf
        path="$apps/bin:$path"
        apps_env=(XDG_DATA_DIRS="$apps/share" OPENSHELL_SMOKE_LINUX_APPS=1)
        control_smoke control-apps "$probes/apps-smoke.mjs" "$bundle"
    fi
    if uses_wine; then
        if ! timeout --kill-after=15s 60m nix-build test/initramfs/nix/openshell-compat.nix \
            -A wineTools -o "$work/control-wineTools" >> "$artifacts/control-graphics-build.log" 2>&1; then
            record_check control-wine failed
            return 1
        fi
        path="$work/control-wineTools/bin:$path"
        apps_env+=(XDG_DATA_DIRS="$work/control-appsTools/share:$work/control-wineTools/share" OPENSHELL_SMOKE_WINE=1
            OPENSHELL_WINE_PROBE_MSI="$work/control-wineTools/share/openshell/openshell-probe-setup.msi")
        control_smoke control-wine "$probes/wine-smoke.mjs"
    fi
    if drives_desktop; then
        control_smoke control-shell "$probes/shell-smoke.mjs" \
            "$work/control-launcher/bin/openshell-shell" "$bundle" "$host"
        # The supervisor runs as root and starts the session as the user.
        if PATH="$path" FONTCONFIG_FILE="$work/control-fonts" XDG_CONFIG_DIRS="$work/control-graphicsTools/etc/xdg" \
            OPENSHELL_CEF_SMOKE_TIMEOUT_MS=90000 timeout --kill-after=10s 15m "$work/control-node/bin/node" \
            "$probes/session-smoke.mjs" "$work/control-launcher/bin/openshell-session" \
            "$work/control-launcher/bin/openshell-shell" "$bundle" "$host" "$tools/Xvfb" "$tools/openbox" \
            "$scratch/control-session" 2>&1 | tee "$artifacts/control-session.log"; then
            record_check control-session passed
        else
            record_check control-session failed
            status=1
        fi
    fi
    kill "$wm" "$xserver" 2>/dev/null || true
    return "$status"
}

# Runs the hostd smoke of the bundle on the builder's Linux as an unprivileged user.
run_control_hostd() {
    local scratch="$work/control-hostd"
    if timeout --kill-after=15s 25m nix-build test/initramfs/nix/openshell-compat.nix \
        -A node -o "$work/node" 2>&1 | tee "$artifacts/control-hostd-build.log" &&
        timeout --kill-after=15s 25m nix-build test/initramfs/nix/openshell-compat.nix \
        -A python -o "$work/python" 2>&1 | tee -a "$artifacts/control-hostd-build.log" &&
        install -d -m 0755 -o 65534 -g 65534 "$scratch" &&
        PATH="$work/python/bin:$PATH" TMPDIR="$scratch" timeout --kill-after=10s 3m \
        "$work/python/bin/python3" -I "$probes/unprivileged.py" "$work/node/bin/node" \
        "$probes/hostd-smoke.mjs" "$bundle" "$scratch/out" 2>&1 | tee "$artifacts/control-hostd.log"; then
        record_check control-hostd passed
    else
        record_check control-hostd failed
        cp "$scratch"/out/hostd-*/hostd.log "$artifacts/control-hostd-stderr.log" 2>/dev/null || true
        return 1
    fi
}

record_check() {
    printf '%s\t%s\n' "$1" "$2" | tee -a "$artifacts/checks.tsv"
}

run_control_probe() {
    local runtime=$1 executable=$2
    shift 2
    if timeout --kill-after=15s 25m nix-build test/initramfs/nix/openshell-compat.nix \
        -A "$runtime" -o "$work/$runtime" 2>&1 | tee "$artifacts/control-$runtime-build.log" &&
        timeout --kill-after=10s 90s "$work/$runtime/$executable" "$@" 2>&1 |
        tee "$artifacts/control-$runtime.log"; then
        record_check "control-$runtime" passed
    else
        record_check "control-$runtime" failed
        return 1
    fi
}

control_status=0
if [[ "$action" == control || "$action" == baseline ]]; then
    prepare_compat
    run_control_probe python bin/python3 -I "$probes/python.py" || control_status=1
    if timeout --kill-after=15s 10m nix-build test/initramfs/nix/openshell-compat.nix -A tls -o "$work/tls"; then
        OPENSHELL_PROBE_TLS="$work/tls" run_control_probe node bin/node "$probes/node.mjs" || control_status=1
    else
        record_check control-node failed
        control_status=1
    fi
    run_control_probe seccomp bin/seccomp-probe || control_status=1
    if [[ "$action" == control ]]; then
        exit "$control_status"
    fi
    if CARGO_TARGET_DIR="$work/shell-target" timeout --kill-after=15s 5m cargo test --locked \
        --manifest-path "$compat/Cargo.toml" 2>&1 | tee "$artifacts/control-rust-shell.log"; then
        record_check control-rust-shell passed
    else
        record_check control-rust-shell failed
        control_status=1
    fi
    # The A/B transaction: unit tests, including power cuts, then the CLI on disk.
    if CARGO_TARGET_DIR="$work/ab-target" timeout --kill-after=15s 5m cargo test --locked \
        --manifest-path "$ab_sources/Cargo.toml" 2>&1 | tee "$artifacts/control-ab.log" &&
        CARGO_TARGET_DIR="$work/ab-target" timeout --kill-after=15s 5m cargo build --locked --release \
        --manifest-path "$ab_sources/Cargo.toml" 2>&1 | tee -a "$artifacts/control-ab.log" &&
        sh "$ab_sources/smoke.sh" "$work/ab-target/release/openshell-ab" 2>&1 | tee -a "$artifacts/control-ab.log"; then
        record_check control-ab passed
    else
        record_check control-ab failed
        control_status=1
    fi
fi

if uses_bundle; then
    prepare_compat
    run_control_hostd || control_status=1
fi
if uses_graphics; then
    prepare_compat
    run_control_graphics || control_status=1
fi

# The series applies in file name order to the pinned upstream revision.
patches=(/tmp/openshell-native-patches/*.patch)
for patch in "${patches[@]}"; do
    if ! git apply --index "$patch"; then
        printf 'Kernel patch does not apply: %s\n' "${patch##*/}" >&2
        record_check kernel-patch failed
        exit 1
    fi
done
if git diff --cached --check; then
    record_check kernel-patch passed
else
    record_check kernel-patch failed
    exit 1
fi

ovmf_dir=${OVMF_DIR:-/root/ovmf/release}
profile=(
    TARGET_ARCH=x86_64 BOOT_METHOD=grub-rescue-iso BOOT_PROTOCOL=multiboot2
    OVMF=on "OVMF_DIR=$ovmf_dir" ENABLE_KVM=0 SMP=1 MEM=2G RELEASE=1
    INITRAMFS=on CONSOLE=hvc0 QEMU_DISPLAY=none NETDEV=none
)

{
    printf 'upstream=%s\nrevision=%s\nrust_channel=%s\n' "$tag" "$actual_revision" "$rust_channel"
    printf '%s\n' "${profile[@]}"
    printf '\n'
    timeout 10m rustc --version
    qemu-system-x86_64 --version
    sha256sum "$ovmf_dir/OVMF.fd"
    sha256sum "${patches[@]}"
} | tee "$artifacts/toolchain.txt"

build_status=0
timeout --kill-after=15s 45m make kernel "${profile[@]}" AUTO_TEST=none 2>&1 |
    tee "$artifacts/build.log" || build_status=$?
kernel_lock="$CARGO_TARGET_DIR/osdk/asterinas-run-base/Cargo.lock"
if [[ -f "$kernel_lock" ]]; then
    cp "$kernel_lock" "$artifacts/kernel-Cargo.lock"
fi
if (( build_status != 0 )); then
    record_check kernel-build failed
    exit "$build_status"
fi

iso="$CARGO_TARGET_DIR/osdk/asterinas-osdk-bin.iso"
if [[ ! -s "$iso" ]]; then
    printf 'Expected OSDK ISO missing: %s\n' "$iso" >&2
    record_check kernel-build failed
    exit 1
fi
cp "$iso" "$artifacts/asterinas.iso"
(cd "$artifacts" && sha256sum asterinas.iso > SHA256SUMS)
record_check kernel-build passed

if [[ "$action" == build ]]; then
    exit 0
fi

run_boot() {
    local name=$1 log marker status=0
    shift
    if ! rm -f qemu.log qemu-serial.log; then
        record_check "$name" failed
        return 1
    fi
    timeout --kill-after=15s "$boot_timeout" make run_kernel "${profile[@]}" "${boot_args[@]}" AUTO_TEST=boot 2>&1 |
        tee "$artifacts/$name.log" || status=$?
    for log in qemu.log qemu-serial.log; do
        if [[ -f "$log" ]]; then
            cp "$log" "$artifacts/$name-$log" || status=1
        fi
    done
    local console="$artifacts/$name-console.txt"
    if [[ -s qemu.log ]]; then
        tr -d '\r' < qemu.log > "$console" || status=1
    fi
    if (( status != 0 )); then
        record_check "$name" failed
        return "$status"
    fi
    if [[ ! -s "$console" ]]; then
        printf 'Missing QEMU console for %s\n' "$name" >&2
        record_check "$name" failed
        return 1
    fi

    for marker in 'Successfully booted.' "$@"; do
        if ! grep -Fxq "$marker" "$console"; then
            printf 'Missing guest marker for %s: %s\n' "$name" "$marker" >&2
            record_check "$name" failed
            return 1
        fi
    done
    record_check "$name" passed
}

run_compat() {
    if ! timeout --kill-after=15s 25m nix-build test/initramfs/nix/openshell-compat.nix \
        -A image -o "$work/compat-initramfs" 2>&1 | tee "$artifacts/compat-initramfs.log"; then
        record_check compat-initramfs failed
        return 1
    fi
    record_check compat-initramfs passed
    # The graphics closure (GTK, Mesa and Chromium) makes a large initramfs.
    # Chromium runs several processes at once, and TCG gives each vCPU a host thread.
    # Mesa adds LLVM to it, and the kiosk drives the whole Desktop.
    local memory=4G cpus=1
    boot_timeout=10m
    if [[ "$action" == graphics ]]; then
        memory=12G
        cpus=4
        boot_timeout=40m
    elif [[ "$action" == kiosk ]]; then
        memory=12G
        cpus=4
        boot_timeout=90m
    elif uses_apps; then
        memory=12G
        cpus=4
        boot_timeout=$(uses_wine && echo 150m || echo 120m)
    fi
    boot_args=("MEM=$memory" "SMP=$cpus" "CARGO_OSDK_INITRAMFS_OPTION=--initramfs=$work/compat-initramfs")
    local status=0 sandbox_status=0 console="$artifacts/native-compat-console.txt"
    local markers=(OPENSHELL_NATIVE_PYTHON_OK OPENSHELL_NATIVE_NODE_OK OPENSHELL_NATIVE_AB_OK)
    local workloads=(ab)
    if uses_bundle; then
        workloads+=(hostd)
    fi
    if uses_graphics; then
        workloads+=(x11 cef cef_isolation)
    fi
    if drives_desktop; then
        workloads+=(shell session)
    fi
    if uses_apps; then
        workloads+=(apps)
    fi
    if uses_wine; then
        workloads+=(wine)
    fi
    local workload
    for workload in "${workloads[@]}"; do
        markers+=("OPENSHELL_NATIVE_${workload^^}_OK")
    done
    run_boot native-compat "${markers[@]}" || status=$?
    for workload in "${workloads[@]}"; do
        if [[ -f "$console" ]] && grep -Fxq "OPENSHELL_NATIVE_${workload^^}_OK" "$console"; then
            record_check "native-${workload//_/-}" passed
        else
            record_check "native-${workload//_/-}" failed
            status=1
        fi
    done
    local layer marker
    for layer in seccomp namespaces chromium_userns; do
        marker="OPENSHELL_NATIVE_${layer^^}"
        if [[ -f "$console" ]] && grep -Fxq "${marker}_OK" "$console"; then
            record_check "native-${layer//_/-}" passed
        elif [[ -f "$console" ]] && grep -Fxq "${marker}_UNAVAILABLE" "$console"; then
            record_check "native-${layer//_/-}" failed
            sandbox_status=1
        else
            record_check "native-${layer//_/-}" unverified
            sandbox_status=1
        fi
    done
    if [[ "$action" == sandbox ]] && (( sandbox_status != 0 )); then
        printf 'A sandbox layer was not verified. See the native-compat logs.\n' >&2
        return 1
    fi
    return "$status"
}

# The disk image (distro/image): GPT with the system slots, recovery and
# data, GRUB choosing the slot from the A/B transaction's grubenv, booted in
# QEMU once per step of each plan. Without the Desktop, the slots stay small.
run_image() {
    if ! timeout --kill-after=15s 25m nix-build test/initramfs/nix/openshell-compat.nix \
        -A ab -o "$work/image-ab" >> "$artifacts/native-image.log" 2>&1; then
        record_check native-image failed
        return 1
    fi
    if OVMF_DIR="$ovmf_dir" timeout --kill-after=15s 150m bash "$image_sources/boot-test.sh" \
        "$work/image-ab/bin/openshell-ab" "$CARGO_TARGET_DIR/osdk/asterinas-osdk-bin.elf" \
        "$work/compat-initramfs" "$artifacts/native-image" >> "$artifacts/native-image.log" 2>&1; then
        record_check native-image passed
    else
        tail -n 40 "$artifacts/native-image.log" >&2
        record_check native-image failed
        return 1
    fi
}

if [[ "$action" == compat || "$action" == sandbox ]] || uses_bundle || uses_graphics; then
    prepare_compat
    compat_status=0
    run_compat || compat_status=1
    if ! uses_graphics; then
        run_image || compat_status=1
    fi
    exit $((control_status || compat_status))
fi

smoke_status=0
run_boot upstream-1 || smoke_status=1
run_boot upstream-2 || smoke_status=1
if as --64 -o "$work/static-exit.o" "$probes/static-exit.S" &&
    ld -m elf_x86_64 -static -o "$work/static-exit" "$work/static-exit.o" &&
    install -m 0755 "$work/static-exit" test/initramfs/etc/openshell-native-static-exit &&
    install -m 0755 "$probes/boot.sh" test/initramfs/src/boot_hello.sh; then
    run_boot native-cli OPENSHELL_NATIVE_PTY_OK OPENSHELL_NATIVE_STATIC_ELF_OK OPENSHELL_NATIVE_BLOCK_IO_OK || smoke_status=1
else
    record_check native-cli failed
    smoke_status=1
fi

if [[ "$action" == baseline ]]; then
    git restore -- test/initramfs/src/boot_hello.sh
    rm -f -- test/initramfs/etc/openshell-native-static-exit
    run_compat || smoke_status=1
    run_image || smoke_status=1
fi
exit $((control_status || smoke_status))
