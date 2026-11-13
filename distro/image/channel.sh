#!/usr/bin/env bash
# OpenShell Native's release channel: what Settings ▸ Updates downloads on an
# installed system (openshell repo, hostd/services/updates.ts, for Native).
# The Desktop reads <channel>/latest.json, signed like OpenShell Live's, and
# downloads the release archive it names; the next restart writes the release
# into the inactive system slot with `openshell-ab update-mounted`
# (userspace/compatibility/probes/installed-session.sh), which checks the
# archive's own signed manifest again, and boots it on trial.
#
# One Ed25519 key signs both manifests: OpenShell Native's release key, kept
# with the Secure Boot key (.secureboot/), whose public half every image
# carries (/opt/openshell-media/release-keys).
#
# Sourced by run.sh and build-media.sh:
#   channel_key <key-folder> <openshell-ab>     openshell-native-release.{key,pem}, made once
#   channel_release <out> <openshell-ab> <key-folder> <version> <built-at> <signed-kernel> <initramfs> <boot.cfg> [<iso>]
#                                              <out>/latest.json(.sig) and <out>/openshell-native-<version>-amd64.tar
#
# The archive holds MANIFEST and MANIFEST.sig (openshell-ab's format), kernel,
# initramfs.cpio.gz and boot.cfg. Publish the folder at
# <portal>/downloads/native/.

CHANNEL_KEY_NAME=openshell-native-release
# The data schema the releases run with (installed-session.sh, CHANNEL_DATA_SCHEMA there).
CHANNEL_SCHEMA=1..1

channel_key() {
    local dir=$1 ab=$2
    mkdir -p "$dir"
    if [[ ! -f "$dir/$CHANNEL_KEY_NAME.key" ]]; then
        "$ab" keygen "$dir/$CHANNEL_KEY_NAME.key" "$dir/$CHANNEL_KEY_NAME.pem"
        chmod 600 "$dir/$CHANNEL_KEY_NAME.key"
    fi
}

channel_release() {
    local out=$1 ab=$2 keys=$3 version=$4 built=$5 kernel=$6 initramfs=$7 boot=$8 iso=${9:-}
    local name="openshell-native-$version-amd64.tar" stage file
    stage=$(mktemp -d)
    cp "$kernel" "$stage/kernel"
    cp "$initramfs" "$stage/initramfs.cpio.gz"
    cp "$boot" "$stage/boot.cfg"
    {
        printf 'release=%s\narch=x86_64\nschema=%s\n' "$version" "$CHANNEL_SCHEMA"
        for file in kernel initramfs.cpio.gz boot.cfg; do
            printf 'file=%s %s %s\n' "$file" "$(stat -c %s "$stage/$file")" "$(sha256sum "$stage/$file" | cut -d' ' -f1)"
        done
    } > "$stage/MANIFEST"
    "$ab" sign "$keys/$CHANNEL_KEY_NAME.key" "$stage/MANIFEST"
    mkdir -p "$out"
    tar -C "$stage" -cf "$out/$name" MANIFEST MANIFEST.sig kernel initramfs.cpio.gz boot.cfg
    rm -rf "$stage"
    local iso_entry=
    if [[ -n "$iso" ]]; then
        iso_entry=$(printf ',\n  "iso": { "url": "%s", "size": %s, "sha256": "%s" }' \
            "$(basename "$iso")" "$(stat -c %s "$iso")" "$(sha256sum "$iso" | cut -d' ' -f1)")
    fi
    # The Desktop's manifest (updates.ts parseManifest): its release is the archive's MANIFEST.
    cat > "$out/latest.json" <<EOF
{
  "schema": 1,
  "product": "openshell-native",
  "version": "$version",
  "builtAt": "$built",
  "publishedAt": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "architecture": "amd64",
  "release": "$(tar -xOf "$out/$name" MANIFEST | sha256sum | cut -d' ' -f1)",
  "update": { "url": "$name", "size": $(stat -c %s "$out/$name"), "sha256": "$(sha256sum "$out/$name" | cut -d' ' -f1)" }$iso_entry
}
EOF
    "$ab" sign "$keys/$CHANNEL_KEY_NAME.key" "$out/latest.json"
}
