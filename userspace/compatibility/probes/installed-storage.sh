# Shared by installed boot and the disk-image tests.

installed_data_device() {
    uuid=$(printf '%s' "$1" | tr -d '-')
    case "$uuid" in ''|*[!0-9a-f]*) return 1 ;; esac
    [ "${#uuid}" = 32 ] || return 1

    found=
    for candidate in /dev/*5; do
        [ -b "$candidate" ] || continue
        # ext2 superblock UUID (1024-byte offset plus 104 bytes).
        actual=$(od -An -v -tx1 -j 1128 -N 16 "$candidate" 2>/dev/null | tr -d ' \n')
        [ "$actual" = "$uuid" ] || continue
        if [ -n "$found" ]; then
            echo "OpenShell Native: multiple devices have data UUID $1" >&2
            return 1
        fi
        found=$candidate
    done
    if [ -z "$found" ]; then
        echo "OpenShell Native: no data partition with UUID $1" >&2
        return 1
    fi
    printf '%s\n' "$found"
}
