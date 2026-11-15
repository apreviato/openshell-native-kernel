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
#
# The live ISO's boot menu is OpenShell Live's: start the live system,
# install it, update an installed one (keep files), and more options. With
# OPENSHELL_MEDIA_PAYLOAD (the payload tools/qemu/run.sh puts in the
# initramfs) and RELEASE_AB, the medium's release gets its signed manifest
# for the updater. The menu has its look (menu-theme.txt, menu.png) in the
# signed GRUB's own font; OPENSHELL_GRUB_FONT (GRUB's unicode.pf2) is a copy
# on the ESP for a GRUB without one.
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
        # The medium's own release, which its installer and its updater put on
        # disks: the payload's signed kernel and boot.cfg (tools/qemu/run.sh,
        # inside the initramfs) with this initramfs. Its signed manifest goes
        # on the kernel's command line, which the updater reads
        # (medium.sh): the manifest covers the initramfs, so it cannot be
        # inside it. Without the payload or the release key there is nothing
        # an installed system could check, and no Update entry.
        payload=${OPENSHELL_MEDIA_PAYLOAD:-}
        manifest_args=
        if [[ -n "$payload" && -n "${RELEASE_AB:-}" ]]; then
            mkdir -p "$work/release"
            cp "$payload/kernel" "$payload/boot.cfg" "$work/release/"
            ln -s "$(readlink -f "$desktop")" "$work/release/initramfs.cpio.gz"
            channel_manifest "$work/release" "$RELEASE_AB" "$SB_KEYS" "$version"
            manifest_args="openshell.manifest=$(base64 -w0 "$work/release/MANIFEST") openshell.manifest_sig=$(base64 -w0 "$work/release/MANIFEST.sig")"
        fi
        # Each boot entry says what the system starts (openshell_mode,
        # installed-session.sh): the live Desktop, the installer or the updater.
        RELEASE_EXTRA_ARGS="$manifest_args" release_files "$tree" "$kernel" "$desktop" "$version" "${RELEASE_SESSION[@]}" '$openshell_mode'
        # The same signed kernel as the one installed systems get.
        if [[ -n "$payload" ]]; then
            cp "$payload/kernel" "$tree/kernel"
        fi
        # Debian's removable-media GRUB finds the disc by /.disk/info, as on
        # Debian's own, and reads /boot/grub/grub.cfg there.
        mkdir -p "$tree/.disk" "$tree/boot/grub"
        echo "OpenShell Native $version amd64" > "$tree/.disk/info"
        # The ESP: the signed chain, and the certificate MokManager enrolls.
        esp="$work/esp"
        sb_esp_files "$esp" "$SB_KEYS" disc
        # The menu's look, as OpenShell Live's: its background (menu.png,
        # make-menu.py) and theme with the release, in GRUB's Unifont: the
        # signed GRUB's own (its memdisk, the only one Secure Boot allows),
        # else this copy (OPENSHELL_GRUB_FONT, run.sh).
        mkdir -p "$esp/EFI/BOOT/theme"
        cp "$here/menu.png" "$esp/EFI/BOOT/theme/"
        tr -d '\r' < "$here/menu-theme.txt" | sed "s|@VERSION@|amd64 · $version · $(date -u +%Y-%m-%d)|" > "$esp/EFI/BOOT/theme/theme.txt"
        if [[ -f "${OPENSHELL_GRUB_FONT:-}" ]]; then
            cp "$OPENSHELL_GRUB_FONT" "$esp/EFI/BOOT/theme/unicode.pf2"
        fi
        update_entry=
        if [[ -n "$manifest_args" ]]; then
            update_entry="menuentry 'Update installed OpenShell Native (keep files)' --hotkey=u --id openshell-update {
    set openshell_mode=update
    openshell_start
}"
        fi
        cat > "$work/grub.cfg" <<'EOF'
# The live medium's boot menu, as OpenShell Live's (openshelllive hooks
# 9000-boot-menu and 9100-update-menu): start the live system, install it,
# or update an installed one, the rest under "More options"; the first after
# five seconds. Each entry says what the system starts (openshell_mode,
# installed-session.sh) and boots this medium's release (boot.cfg).
set timeout=5
set timeout_style=menu
set default=openshell-live
# GRUB's lines also go to the first serial port, which the tests read, where
# there is one: a computer without it (most notebooks) would otherwise show
# "serial port 'com0' isn't found" and lose the splash.
set openshell_serial=
if serial --unit=0 --speed=115200; then
    set openshell_serial=serial
fi
export openshell_serial
terminal_output console $openshell_serial
search --no-floppy --file --set=openshell_esp /EFI/BOOT/mmx64.efi
export openshell_esp
source ($openshell_esp)/EFI/BOOT/splash.cfg
# The menu's font: Unifont from the signed GRUB's own memdisk. Debian puts it
# there because, with Secure Boot on, GRUB refuses font files from disk
# ("prohibited by secure boot policy") and would show its plain text menu.
# A GRUB without it reads the copy on the ESP.
set openshell_font=
if loadfont unicode; then
    set openshell_font=memdisk
elif loadfont ($openshell_esp)/EFI/BOOT/theme/unicode.pf2; then
    set openshell_font=esp
fi
if [ -n "$openshell_font" ]; then
    set gfxmode=auto
    if terminal_output gfxterm $openshell_serial; then
        set theme=($openshell_esp)/EFI/BOOT/theme/theme.txt
        export theme
    fi
fi

# The boot splash, then the system.
function openshell_start {
    openshell_splash
    echo "OpenShell Native: starting ($openshell_mode)"
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

menuentry 'Start OpenShell Native Live' --hotkey=l --id openshell-live {
    set openshell_mode=live
    openshell_start
}
menuentry 'Install OpenShell Native on this computer' --hotkey=i --id openshell-install {
    set openshell_mode=install
    openshell_start
}
@UPDATE@
submenu 'More options' --hotkey=o --id openshell-more {
    menuentry 'Enroll the OpenShell Native key (Secure Boot, once)' --hotkey=e --id openshell-enroll {
        echo 'In MokManager: Enroll key from disk, then ENROLL_THIS_KEY_IN_MOKMANAGER.cer.'
        sleep 3
        set root=$openshell_esp
        chainloader ($openshell_esp)/EFI/BOOT/mmx64.efi
        boot
    }
    menuentry 'UEFI firmware settings' --hotkey=f --id openshell-firmware {
        fwsetup
    }
    menuentry 'Restart' --hotkey=r --id openshell-restart {
        reboot
    }
    menuentry 'Shut down' --hotkey=s --id openshell-halt {
        halt
    }
}
EOF
        OPENSHELL_UPDATE_ENTRY=$update_entry awk '
            $0 == "@UPDATE@" { if (ENVIRON["OPENSHELL_UPDATE_ENTRY"] != "") print ENVIRON["OPENSHELL_UPDATE_ENTRY"]; next }
            { print }' "$work/grub.cfg" > "$tree/boot/grub/grub.cfg"
        # The signed chain, the splash, the menu's theme and its font.
        mkfs.vfat -C -n OSN-ESP "$work/efi.img" $((16 * 1024)) > /dev/null
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
    # What an installed system boots: the session without "live". With the
    # payload, the very files the medium installs and updates from, so its
    # channel release and its USB update are one release.
    if [[ -n "${OPENSHELL_MEDIA_PAYLOAD:-}" ]]; then
        cp "$OPENSHELL_MEDIA_PAYLOAD/boot.cfg" "$work/boot.cfg"
        cp "$OPENSHELL_MEDIA_PAYLOAD/kernel" "$work/kernel.signed"
    else
        release_boot_cfg "$version" "${RELEASE_SESSION[@]}" > "$work/boot.cfg"
        sb_sign "$SB_KEYS" "$kernel" "$work/kernel.signed"
    fi
    channel_release "$output/native-channel" "$RELEASE_AB" "$SB_KEYS" "$version" \
        "${OPENSHELL_BUILT_AT:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}" "$work/kernel.signed" "$desktop" "$work/boot.cfg" "$output/$name"
    echo "OpenShell Native release channel: $output/native-channel"
fi
cp "$SB_KEYS/openshell-native.cer" "$output/ENROLL_THIS_KEY_IN_MOKMANAGER.cer"
echo "OpenShell Native media: $output/$name"
