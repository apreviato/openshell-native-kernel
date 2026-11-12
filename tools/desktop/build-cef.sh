#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
    printf 'Usage: bash tools/desktop/build-cef.sh <cef-sdk> [output-dir]\n' >&2
    exit 2
fi

root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
sdk=$(cd -- "$1" && pwd -P)
output=${2:-"$root/out/cef-build"}
mkdir -p -- "$output"
output=$(cd -- "$output" && pwd -P)

cargo build --locked --release --manifest-path "$root/userspace/shell/Cargo.toml" \
    --target-dir "$output/rust"
cmake -S "$root/userspace/shell/cef" -B "$output/host" \
    -DCEF_ROOT="$sdk" -DCMAKE_BUILD_TYPE=Release -DUSE_SANDBOX=ON \
    -DOPENSHELL_CEF_POLICY_LIBRARY="$output/rust/release/libopenshell_shell.a"
cmake --build "$output/host" --parallel

{
    grep -m1 '^#define CEF_VERSION ' "$sdk/include/cef_version.h"
    (cd -- "$output/host/runtime" && sha256sum openshell-cef-host libcef.so)
    (cd -- "$output/rust/release" && sha256sum openshell-shell)
} > "$output/build-info.txt"

printf 'CEF host: %s/host/runtime/openshell-cef-host\n' "$output"
printf 'Rust shell: %s/rust/release/openshell-shell\n' "$output"
