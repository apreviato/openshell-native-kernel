#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 || $# -gt 3 ]]; then
    printf 'Usage: bash tools/desktop/cef-baseline.sh <cef-sdk> <desktop-bundle> [output-dir]\n' >&2
    exit 2
fi
if [[ $(uname -sm) != 'Linux x86_64' || $(id -u) == 0 || -z ${DISPLAY:-} ]]; then
    printf 'Use an unprivileged Linux x86-64 X11 session with a window manager.\n' >&2
    exit 1
fi

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
sdk=$(cd -- "$1" && pwd -P)
bundle=$(cd -- "$2" && pwd -P)
output=${3:-"$root/out"}
mkdir -p -- "$output"
artifacts=$(mktemp -d "$(cd -- "$output" && pwd -P)/cef-baseline-XXXXXX")
printf 'Artifacts: %s\n' "$artifacts"
result=0

check() {
    local name=$1
    shift
    if "$@" 2>&1 | tee "$artifacts/$name.log"; then
        printf '%s\tpassed\n' "$name" >> "$artifacts/checks.tsv"
    else
        printf '%s\tfailed\n' "$name" >> "$artifacts/checks.tsv"
        result=1
        return 1
    fi
}

build_sample() {
    cmake -S "$sdk" -B "$artifacts/sample" -DCMAKE_BUILD_TYPE=Release -DUSE_SANDBOX=ON &&
        cmake --build "$artifacts/sample" --config Release --target cefsimple --parallel
}

record_environment() {
    uname -smr && node --version && rustc --version &&
    grep -m1 '^#define CEF_VERSION ' "$sdk/include/cef_version.h" &&
    sha256sum "$sdk/Release/libcef.so"
}

check hostd node "$root/tools/desktop/hostd-smoke.mjs" "$bundle" "$artifacts" || true
check environment record_environment || exit "$result"
if check build-sample build_sample; then
    sample=$(find "$artifacts/sample" -type f -name cefsimple -perm -u=x -print -quit)
    check sample node "$root/tools/desktop/cef-smoke.mjs" sample "$sample" "$artifacts" || true
fi
if check build-host bash "$root/tools/desktop/build-cef.sh" "$sdk" "$artifacts/native"; then
    host="$artifacts/native/host/runtime/openshell-cef-host"
    check host node "$root/tools/desktop/cef-smoke.mjs" host "$host" "$artifacts" || true
    check isolation node "$root/tools/desktop/cef-smoke.mjs" isolation "$host" "$artifacts" || true
fi
cat "$artifacts/checks.tsv"
exit "$result"
