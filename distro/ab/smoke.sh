#!/bin/sh
# The A/B transaction end to end on a scratch directory: install, a confirmed
# update, an update that never confirms and falls back, a corrupted release,
# unsigned and wrongly signed manifests, and a destroyed selector. POSIX sh,
# for the guest's BusyBox.
# Usage: sh distro/ab/smoke.sh <openshell-ab>; TMPDIR chooses the disk.
set -eu

ab=$1
work=$(mktemp -d "${TMPDIR:-/tmp}/openshell-ab-XXXXXX")
trap 'rm -rf "$work"' EXIT
disk="$work/disk"
keys="$work/keys"
mkdir -p "$keys"
"$ab" keygen "$work/release.key" "$keys/release.pem"
"$ab" keygen "$work/other.key" "$work/other.pem"

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
    "$ab" sign "$work/release.key" "$dir/MANIFEST"
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
"$ab" "$disk" stage "$work/2026.12.0" "$work/2026.12.0/MANIFEST" 2 "$keys"
"$ab" "$disk" trial
expect b
if "$ab" "$disk" confirm b 2026.11.0 2>/dev/null; then
    echo 'Confirmed a slot with a different release' >&2
    exit 1
fi
"$ab" "$disk" status | grep -Fxq 'b: Trial(2) 2026.12.0'
"$ab" "$disk" confirm b 2026.12.0
expect b

# An update that never confirms: three attempts, then the confirmed slot.
release 2027.01.0
"$ab" "$disk" stage "$work/2027.01.0" "$work/2027.01.0/MANIFEST" 2 "$keys"
"$ab" "$disk" trial
expect a
expect a
expect a
expect b
"$ab" "$disk" status | grep -q '^a: Bad 2027.01.0$'

# A release whose files do not match its manifest is never staged.
release 2027.02.0
printf 'tampered\n' >> "$work/2027.02.0/kernel"
if "$ab" "$disk" stage "$work/2027.02.0" "$work/2027.02.0/MANIFEST" 2 "$keys" 2>/dev/null; then
    echo 'A tampered release was staged' >&2
    exit 1
fi
"$ab" "$disk" status | grep -q '^a: Empty -$'
expect b

# A release for data the computer does not have is refused.
release 2027.03.0
if "$ab" "$disk" stage "$work/2027.03.0" "$work/2027.03.0/MANIFEST" 3 "$keys" 2>/dev/null; then
    echo 'An incompatible release was staged' >&2
    exit 1
fi

# A manifest changed after signing, or signed by a key the computer does not trust, is never read.
release 2027.04.0
printf 'release=2027.04.1\n' > "$work/2027.04.0/MANIFEST.tmp"
tail -n +2 "$work/2027.04.0/MANIFEST" >> "$work/2027.04.0/MANIFEST.tmp"
mv "$work/2027.04.0/MANIFEST.tmp" "$work/2027.04.0/MANIFEST"
if "$ab" "$disk" stage "$work/2027.04.0" "$work/2027.04.0/MANIFEST" 2 "$keys" 2>/dev/null; then
    echo 'A manifest changed after signing was staged' >&2
    exit 1
fi
release 2027.05.0
"$ab" sign "$work/other.key" "$work/2027.05.0/MANIFEST"
if "$ab" "$disk" stage "$work/2027.05.0" "$work/2027.05.0/MANIFEST" 2 "$keys" 2>/dev/null; then
    echo 'A manifest signed by an untrusted key was staged' >&2
    exit 1
fi
rm "$work/2027.05.0/MANIFEST.sig"
if "$ab" "$disk" stage "$work/2027.05.0" "$work/2027.05.0/MANIFEST" 2 "$keys" 2>/dev/null; then
    echo 'An unsigned manifest was staged' >&2
    exit 1
fi
"$ab" "$disk" status | grep -q '^a: Empty -$'

# Through GRUB (distro/image/grub.cfg): it takes an attempt from the trial in
# grubenv, and the booted system records it. What GRUB does, in sed.
grub_boot() {
    env="$disk/grubenv"
    trial=$(sed -n 's/^openshell_trial=//p' "$env")
    if [ -n "$trial" ] && grep -q '^openshell_tries=1' "$env"; then
        sed -i 's/^openshell_tries=1/openshell_tries=/' "$env"
        printf '#' >> "$env"
        echo "$trial"
    else
        sed -n 's/^openshell_fallback=//p' "$env"
    fi
    if [ "$(wc -c < "$env")" -ne 1024 ]; then
        echo 'grubenv is not a 1024-byte block' >&2
        exit 1
    fi
}
expect_grub() {
    booted=$(grub_boot)
    if [ "$booted" != "$1" ]; then
        printf 'Expected GRUB to boot %s, got %s\n' "$1" "$booted" >&2
        exit 1
    fi
    "$ab" "$disk" booted "$booted"
}
release 2027.06.0
"$ab" "$disk" stage "$work/2027.06.0" "$work/2027.06.0/MANIFEST" 2 "$keys"
"$ab" "$disk" trial
grep -q '^openshell_trial=a$' "$disk/grubenv"
for left in 2 1 0; do
    expect_grub a
    "$ab" "$disk" status | grep -q "^a: Trial($left) 2027.06.0$"
done
expect_grub b
"$ab" "$disk" status | grep -q '^a: Bad 2027.06.0$'
if grep -q '^openshell_trial=' "$disk/grubenv"; then
    echo 'grubenv still offers the failed trial' >&2
    exit 1
fi
release 2027.07.0
"$ab" "$disk" stage "$work/2027.07.0" "$work/2027.07.0/MANIFEST" 2 "$keys"
"$ab" "$disk" trial
expect_grub a
"$ab" "$disk" confirm a
grep -q '^openshell_fallback=a$' "$disk/grubenv"
expect_grub a
expect a

# Restoring the previous release (the USB stick's updater): the good
# release the last update replaced goes on trial, boots and is confirmed.
main_disk=$disk
disk="$work/restore"
"$ab" "$disk" install 2026.11.0
if "$ab" "$disk" restore 2>/dev/null; then
    echo 'Restored a release a fresh installation never had' >&2
    exit 1
fi
"$ab" "$disk" stage "$work/2026.12.0" "$work/2026.12.0/MANIFEST" 2 "$keys"
"$ab" "$disk" trial
expect b
"$ab" "$disk" confirm b 2026.12.0
"$ab" "$disk" restore | grep -Fxq 'restoring 2026.11.0 in slot a'
expect a
"$ab" "$disk" confirm a 2026.11.0
expect a
"$ab" "$disk" status | grep -q '^b: Good 2026.12.0$'
# update-offline writes only a mounted system partition of an OpenShell disk.
mkdir -p "$work/not-a-partition"
if "$ab" "$disk" update-offline "$work/2026.12.0" "$work/2026.12.0/MANIFEST" 2 "$keys" "$work/not-a-partition" 2>/dev/null; then
    echo 'update-offline wrote a folder that is no system partition' >&2
    exit 1
fi
disk=$main_disk

# One damaged selector copy is survived; two mean recovery.
newest=$(ls -t "$disk"/selector.* | head -1)
printf 'garbage' > "$newest"
# The older copy has slot a on trial, before its confirmation.
expect a
printf 'garbage' > "$disk/selector.0"
printf 'garbage' > "$disk/selector.1"
expect recovery
echo OPENSHELL_NATIVE_AB_OK
