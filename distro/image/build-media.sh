#!/usr/bin/env bash
# Package the pinned kernel and Desktop closure as a UEFI live ISO or a GPT image.
set -euo pipefail

action=$1 version=$2 kernel=$3 desktop=$4 recovery=$5 output=$6
if [[ ! "$version" =~ ^[a-zA-Z0-9][a-zA-Z0-9._+-]{0,79}$ ]]; then
    echo "Invalid Desktop release version: $version" >&2
    exit 2
fi

here=$(cd "$(dirname "$0")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

boot_config() {
    local directory=$1 release=$2 command=$3
    cat > "$directory/boot.cfg" <<EOF
multiboot2 (\$root)/kernel SHELL=/bin/sh LOGNAME=root HOME=/ USER=root PATH=/bin:/usr/bin loglevel=error earlycon console=hvc0 openshell.slot=\$openshell_slot openshell.release=$release openshell.data=\$openshell_data_uuid -- $command
module2 --nounzip (\$root)/initramfs.cpio.gz
EOF
}

payload() {
    local directory=$1 initramfs=$2
    mkdir -p "$directory"
    cp "$kernel" "$directory/kernel"
    cp "$initramfs" "$directory/initramfs.cpio.gz"
}

case "$action" in
    live-iso)
        name="openshell-native-live-$version-amd64.iso"
        tree="$work/iso"
        payload "$tree" "$desktop"
        boot_config "$tree" "$version" 'sh -l /opt/openshell-compat/installed-session.sh live'
        mkdir -p "$tree/boot/grub"
        cat > "$tree/boot/grub/grub.cfg" <<'EOF'
set timeout=5
set default=0
serial --unit=0 --speed=115200
terminal_output console serial
menuentry 'Try OpenShell Native' {
    echo 'OpenShell Native: starting live session'
    source ($root)/boot.cfg
    boot
}
EOF
        grub=$(command -v grub-mkrescue || echo /usr/local/grub/bin/grub-mkrescue)
        grub_dir=$(cd "$(dirname "$grub")/../lib/grub/x86_64-efi" && pwd)
        "$grub" -d "$grub_dir" -o "$work/$name" "$tree"
        xorriso -indev "$work/$name" -report_el_torito plain -report_system_area plain > "$work/boot-report" 2>&1
        grep -Eq 'El Torito boot img.*UEFI' "$work/boot-report"
        grep -Eq 'GPT partname local.*EFI boot partition|MBR partition +:.*0xef' "$work/boot-report"
        ;;
    install-image)
        name="openshell-native-install-$version-amd64.img"
        mkdir -p "$work/disk"
        ab=${OPENSHELL_AB:?expected the openshell-ab build path}
        "$ab" "$work/disk" install "$version"
        payload "$work/disk/slot-a" "$desktop"
        boot_config "$work/disk/slot-a" "$version" 'sh -l /opt/openshell-compat/installed-session.sh'
        payload "$work/recovery" "$recovery"
        boot_config "$work/recovery" recovery 'sh -l'
        OPENSHELL_IMAGE_DATA_MIB=${OPENSHELL_IMAGE_DATA_MIB:-2048} \
            bash "$here/mkimage.sh" "$work/disk" "$work/recovery" "$work/$name"
        ;;
    *) echo "Unknown media type: $action" >&2; exit 2 ;;
esac

mv "$work/$name" "$output/$name"
(cd "$output" && sha256sum "$name" > "$name.sha256")
echo "OpenShell Native media: $output/$name"
