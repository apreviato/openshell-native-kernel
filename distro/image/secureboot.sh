#!/usr/bin/env bash
# The UEFI Secure Boot chain of OpenShell Native media, as OpenShell Live's:
#
#   firmware ─ shim (Debian's, Microsoft-signed) ─ GRUB (Debian-signed)
#            ─ Asterinas (signed with OpenShell Native's key)
#
# shim and GRUB come from Debian, pinned in secureboot.lock. Asterinas cannot
# carry Debian's signature: GRUB has shim check it against the Machine Owner
# Keys, where OpenShell Native's certificate is enrolled once, with the
# MokManager on the same media (EFI/BOOT/mmx64.efi, from GRUB's menu or when
# shim offers it). Debian's GRUB has no multiboot2 under Secure Boot, so the
# kernel boots through the Linux EFI handover (BOOT_PROTOCOL=linux-efi-handover64).
#
# Sourced by build-media.sh and mkimage.sh:
#   sb_fetch <folder>                    shimx64.efi, mmx64.efi, grub-disc.efi, grub-disk.efi there
#   sb_key <folder>                      openshell-native.{key,crt,cer}, made once
#   sb_sign <key-folder> <in> <out>      a signed copy of a PE image (the kernel)
#   sb_esp_files <folder> <key-folder> <disc|disk>
#                                        the ESP's boot files, with the certificate:
#                                        a disc's GRUB reads /boot/grub/grub.cfg of the
#                                        filesystem with /.disk/info (a live ISO), a disk's
#                                        /EFI/debian/grub.cfg of its own ESP, as Debian's do;
#                                        both source EFI/BOOT/splash.cfg, the boot splash
#
# SB_CACHE keeps the downloaded packages; SBSIGN names sbsign.

sb_here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
SB_MIRROR=${SB_MIRROR:-https://deb.debian.org/debian}
SB_KEY_NAME=openshell-native

sb_fetch() {
    local out=$1 cache=${SB_CACHE:-${TMPDIR:-/tmp}/openshell-secureboot-cache} name version path sum deb
    mkdir -p "$out" "$cache"
    local extract
    extract=$(mktemp -d)
    while read -r name version path sum; do
        [[ -z "$name" || "$name" == \#* ]] && continue
        deb="$cache/$(basename "$path")"
        if [[ ! -f "$deb" ]] || ! echo "$sum  $deb" | sha256sum -c --quiet - 2>/dev/null; then
            curl -fsSL --retry 3 -o "$deb.partial" "$SB_MIRROR/$path"
            mv "$deb.partial" "$deb"
        fi
        if ! echo "$sum  $deb" | sha256sum -c --quiet -; then
            echo "secureboot: $name $version does not match secureboot.lock" >&2
            rm -rf "$extract"
            return 1
        fi
        dpkg-deb -x "$deb" "$extract"
    done < "$sb_here/secureboot.lock"
    cp "$extract/usr/lib/shim/shimx64.efi.signed" "$out/shimx64.efi"
    cp "$extract/usr/lib/shim/mmx64.efi.signed" "$out/mmx64.efi"
    cp "$extract/usr/lib/grub/x86_64-efi-signed/gcdx64.efi.signed" "$out/grub-disc.efi"
    cp "$extract/usr/lib/grub/x86_64-efi-signed/grubx64.efi.signed" "$out/grub-disk.efi"
    rm -rf "$extract"
}

sb_key() {
    local dir=$1
    mkdir -p "$dir"
    if [[ ! -f "$dir/$SB_KEY_NAME.key" ]]; then
        openssl req -new -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
            -subj "/CN=OpenShell Native kernel signing/" \
            -keyout "$dir/$SB_KEY_NAME.key" -out "$dir/$SB_KEY_NAME.crt" 2> /dev/null
        chmod 600 "$dir/$SB_KEY_NAME.key"
    fi
    # MokManager enrolls DER certificates.
    openssl x509 -in "$dir/$SB_KEY_NAME.crt" -outform DER -out "$dir/$SB_KEY_NAME.cer"
}

sb_sign() {
    local dir=$1 in=$2 out=$3
    "${SBSIGN:-sbsign}" --key "$dir/$SB_KEY_NAME.key" --cert "$dir/$SB_KEY_NAME.crt" --output "$out" "$in" > /dev/null
}

# EFI/BOOT/{BOOTX64.EFI (shim), grubx64.efi, mmx64.efi}, the boot splash
# (splash.cfg, splash.png) and the certificate to enroll, at the ESP's top.
# shim starts the grubx64.efi next to it.
sb_esp_files() {
    local out=$1 keys=$2 kind=$3 chain
    chain=$(mktemp -d)
    sb_fetch "$chain"
    mkdir -p "$out/EFI/BOOT"
    cp "$chain/shimx64.efi" "$out/EFI/BOOT/BOOTX64.EFI"
    cp "$chain/grub-$kind.efi" "$out/EFI/BOOT/grubx64.efi"
    cp "$chain/mmx64.efi" "$out/EFI/BOOT/mmx64.efi"
    cp "$sb_here/splash.cfg" "$sb_here/splash.png" "$out/EFI/BOOT/"
    cp "$keys/$SB_KEY_NAME.cer" "$out/ENROLL_THIS_KEY_IN_MOKMANAGER.cer"
    rm -rf "$chain"
}
