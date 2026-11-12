#!/bin/sh
# The A/B transaction end to end on a scratch directory: install, a confirmed
# update, an update that never confirms and falls back, a corrupted release
# and a destroyed selector. POSIX sh, for the guest's BusyBox.
# Usage: sh distro/ab/smoke.sh <openshell-ab>; TMPDIR chooses the disk.
set -eu

ab=$1
work=$(mktemp -d "${TMPDIR:-/tmp}/openshell-ab-XXXXXX")
trap 'rm -rf "$work"' EXIT
disk="$work/disk"

release() {
    name=$1
    dir="$work/$1"
    mkdir -p "$dir"
    printf 'kernel %s\n' "$name" > "$dir/kernel"
    printf 'initramfs %s\n' "$name" > "$dir/initramfs"
    {
        printf 'release=%s\narch=x86_64\nschema=1..2\n' "$name"
        for file in kernel initramfs; do
            printf 'file=%s %s %s\n' "$file" "$(stat -c %s "$dir/$file")" "$(sha256sum "$dir/$file" | cut -d' ' -f1)"
        done
    } > "$dir/MANIFEST"
}

expect() {
    wanted=$1
    actual=$("$ab" "$disk" boot)
    if [ "$actual" != "$wanted" ]; then
        printf 'Expected boot %s, got %s\n' "$wanted" "$actual" >&2
        exit 1
    fi
}

"$ab" "$disk" install 2026.11.0
expect a

# A confirmed update.
release 2026.12.0
"$ab" "$disk" stage "$work/2026.12.0" "$work/2026.12.0/MANIFEST" 2
"$ab" "$disk" trial
expect b
"$ab" "$disk" confirm b
expect b

# An update that never confirms: three attempts, then the confirmed slot.
release 2027.01.0
"$ab" "$disk" stage "$work/2027.01.0" "$work/2027.01.0/MANIFEST" 2
"$ab" "$disk" trial
expect a
expect a
expect a
expect b
"$ab" "$disk" status | grep -q '^a: Bad 2027.01.0$'

# A release whose files do not match its manifest is never staged.
release 2027.02.0
printf 'tampered\n' >> "$work/2027.02.0/kernel"
if "$ab" "$disk" stage "$work/2027.02.0" "$work/2027.02.0/MANIFEST" 2 2>/dev/null; then
    echo 'A tampered release was staged' >&2
    exit 1
fi
"$ab" "$disk" status | grep -q '^a: Empty -$'
expect b

# A release for data the computer does not have is refused.
release 2027.03.0
if "$ab" "$disk" stage "$work/2027.03.0" "$work/2027.03.0/MANIFEST" 3 2>/dev/null; then
    echo 'An incompatible release was staged' >&2
    exit 1
fi

# One damaged selector copy is survived; two mean recovery.
newest=$(ls -t "$disk"/selector.* | head -1)
printf 'garbage' > "$newest"
expect b
printf 'garbage' > "$disk/selector.0"
printf 'garbage' > "$disk/selector.1"
expect recovery
echo OPENSHELL_NATIVE_AB_OK
