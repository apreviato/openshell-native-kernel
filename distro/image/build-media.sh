#!/usr/bin/env bash
# Package the pinned kernel and Desktop closure as a UEFI live ISO or a GPT image.
#
#   build-media.sh <live-iso|install-image> <version> <kernel> <desktop-initramfs> <recovery-initramfs> <output-dir>
#
# The kernel is Asterinas built with BOOT_PROTOCOL=linux-efi-handover64. Both
# media boot as OpenShell Live's do, Secure Boot included: Debian's shim and
# GRUB, and the kernel signed with OpenShell Native's key (secureboot.sh),
# whose certificate the computer enrolls once in MokManager. SB_KEYS names
# the folder of that key, which also holds the release key: with RELEASE_AB
# (openshell-ab) set, the live ISO comes with its release channel
# (channel.sh) in <output>/native-channel, for installed systems to update to.
# OPENSHELL_BUILT_AT is the Desktop bundle's build time.
set -euo pipefail

action=$1 version=$2 kernel=$3 desktop=$4 recovery=$5 output=$6
if [[ ! "$version" =~ ^[a-zA-Z0-9][a-zA-Z0-9._+-]{0,79}$ ]]; then
    echo "Invalid Desktop release version: $version" >&2
    exit 2
fi

here=$(cd "$(dirname "$0")" && pwd)
source "$here/release.sh"
source "$here/channel.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

case "$action" in
    live-iso)
        name="openshell-native-live-$version-amd64.iso"
        tree="$work/iso"
        release_files "$tree" "$kernel" "$desktop" "$version" "${RELEASE_SESSION[@]}" live
        # Debian's removable-media GRUB finds the disc by /.disk/info, as on
        # Debian's own, and reads /boot/grub/grub.cfg there.
        mkdir -p "$tree/.disk" "$tree/boot/grub"
        echo "OpenShell Native $version amd64" > "$tree/.disk/info"
        # The ESP: the signed chain, and the certificate MokManager enrolls.
        esp="$work/esp"
        sb_esp_files "$esp" "$SB_KEYS" disc
        cat > "$tree/boot/grub/grub.cfg" <<'EOF'
set timeout=5
set default=0
serial --unit=0 --speed=115200
terminal_output console serial
search --no-floppy --file --set=openshell_esp /EFI/BOOT/mmx64.efi
export openshell_esp
source ($openshell_esp)/EFI/BOOT/splash.cfg
menuentry 'Try OpenShell Native' {
    openshell_splash
    echo 'OpenShell Native: starting live session'
    if ! source ($root)/boot.cfg; then
        # Secure Boot refused the kernel: its key is not enrolled on this computer yet.
        openshell_text
        echo 'Secure Boot does not know OpenShell Native yet. MokManager opens now:'
        echo 'Enroll key from disk > ENROLL_THIS_KEY_IN_MOKMANAGER.cer > Continue > Yes, then Reboot.'
        sleep 8
        set root=$openshell_esp
        chainloader ($openshell_esp)/EFI/BOOT/mmx64.efi
    fi
    boot
}
menuentry 'Enroll the OpenShell Native key (Secure Boot, once)' {
    echo 'In MokManager: Enroll key from disk, then ENROLL_THIS_KEY_IN_MOKMANAGER.cer.'
    sleep 3
    set root=$openshell_esp
    chainloader ($openshell_esp)/EFI/BOOT/mmx64.efi
    boot
}
EOF
        mkfs.vfat -C -n OSN-ESP "$work/efi.img" $((8 * 1024)) > /dev/null
        (cd "$esp" && mcopy -s -i "$work/efi.img" ./* ::/)
        # The certificate in the ISO, too: a stick written from it shows it in any file manager.
        cp "$esp/ENROLL_THIS_KEY_IN_MOKMANAGER.cer" "$tree/"
        # UEFI only: El Torito with the ESP image, which is also a GPT partition on a USB stick.
        mkdir -p "$tree/boot"
        cp "$work/efi.img" "$tree/boot/efi.img"
        xorriso -as mkisofs -r -J -V OPENSHELL_NATIVE -o "$work/$name" \
            -e boot/efi.img -no-emul-boot -isohybrid-gpt-basdat \
            -append_partition 2 0xef "$work/efi.img" "$tree" 2> "$work/xorriso.log" || { cat "$work/xorriso.log" >&2; exit 1; }
        xorriso -indev "$work/$name" -report_el_torito plain -report_system_area plain > "$work/boot-report" 2>&1
        grep -Eq 'El Torito boot img.*UEFI' "$work/boot-report"
        grep -Eq 'GPT partname local.*|MBR partition +:.*0xef' "$work/boot-report"
        ;;
    install-image)
        name="openshell-native-install-$version-amd64.img"
        mkdir -p "$work/disk"
        ab=${OPENSHELL_AB:?expected the openshell-ab build path}
        "$ab" "$work/disk" install "$version"
        release_files "$work/disk/slot-a" "$kernel" "$desktop" "$version" "${RELEASE_SESSION[@]}"
        release_files "$work/recovery" "$kernel" "$recovery" recovery "${RELEASE_RECOVERY[@]}"
        OPENSHELL_IMAGE_DATA_MIB=${OPENSHELL_IMAGE_DATA_MIB:-2048} \
            bash "$here/mkimage.sh" "$work/disk" "$work/recovery" "$work/$name"
        ;;
    *) echo "Unknown media type: $action" >&2; exit 2 ;;
esac

mv "$work/$name" "$output/$name"
(cd "$output" && sha256sum "$name" > "$name.sha256")
if [[ "$action" == live-iso && -n "${RELEASE_AB:-}" ]]; then
    # What an installed system boots: the session without "live".
    release_boot_cfg "$version" "${RELEASE_SESSION[@]}" > "$work/boot.cfg"
    sb_sign "$SB_KEYS" "$kernel" "$work/kernel.signed"
    channel_release "$output/native-channel" "$RELEASE_AB" "$SB_KEYS" "$version" \
        "${OPENSHELL_BUILT_AT:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}" "$work/kernel.signed" "$desktop" "$work/boot.cfg" "$output/$name"
    echo "OpenShell Native release channel: $output/native-channel"
fi
cp "$SB_KEYS/openshell-native.cer" "$output/ENROLL_THIS_KEY_IN_MOKMANAGER.cer"
echo "OpenShell Native media: $output/$name"
