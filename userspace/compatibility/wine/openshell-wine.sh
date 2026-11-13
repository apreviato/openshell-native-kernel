#!/bin/sh
# OpenShell's Wine runner (OPENSHELL_WINE.md, phases W2 and W4): the Desktop
# opens Windows programs with it ("Open with"), through Xpra, as it opens
# Linux applications. Programs run in the foreground, so their Xpra session
# lasts as long as they do.
#
#   openshell-wine [--prefix <id>] <program> [arguments]   run, in prefix <id> (default)
#   openshell-wine --install <setup.exe|setup.msi>         install into a new prefix of its own
#   openshell-wine --configure <id|prefix folder>          winecfg
#   openshell-wine --backup <id>                           <backups>/<id>-<date>.tar.gz
#   openshell-wine --restore <backup.tar.gz>               the prefix again, as backed up
#   openshell-wine --remove <id>                           the prefix and its shortcuts
#
# - Prefixes are OpenShell's, one per installed application, in the person's
#   data (never the system slots, which an update or rollback replaces):
#   $XDG_DATA_HOME/openshell/wine/prefixes/<id>. Each records the Wine that
#   last ran it (openshell-prefix.conf). A newer Wine backs the prefix up
#   before it updates it, so a rollback of the system can restore it.
# - Temporary files go to the cache ($XDG_CACHE_HOME/openshell/wine/<id>).
# - Prefixes are set up without Wine Mono and Gecko, which Wine would offer to
#   download. A .NET program (its PE header names a CLR runtime) gets Wine
#   Mono in its prefix, from the Wine package (phase W5), on first use.
# - Drive Z:, which Wine maps to the whole filesystem, is removed: programs
#   see C: only (OPENSHELL_WINE.md, section 6). The program or installer the
#   person opened reaches its own folder only, as drive P:.
# - Installers' shortcuts become Desktop applications (winemenubuilder);
#   their file associations do not: Windows programs never become the default
#   for Linux files.
set -eu

data=${XDG_DATA_HOME:-$HOME/.local/share}/openshell/wine
prefixes=$data/prefixes
backups=$data/backups
cache=${XDG_CACHE_HOME:-$HOME/.cache}/openshell/wine
: "${WINEDEBUG:=-all}"
export WINEDEBUG

fail() { echo "openshell-wine: $*" >&2; exit 2; }
valid_id() { printf '%s' "$1" | grep -Eq '^[a-z0-9][a-z0-9-]{0,63}$'; }
version() { wine --version 2>/dev/null || echo unknown; }
setting() { sed -n "s/^$1=//p" "$WINEPREFIX/openshell-prefix.conf" 2>/dev/null | head -n 1; }

# Reads an unsigned little-endian number of $2 bytes at offset $1 of file $3.
number_at() { od -An -tu"$2" -j "$1" -N "$2" "$3" 2>/dev/null | tr -d ' \n'; }

# Whether $1 is a .NET program or installer: a PE file whose CLR runtime
# header (data directory 14) is set.
dotnet_program() {
    pe=$(number_at 60 4 "$1")
    [ -n "$pe" ] && [ "$(od -An -c -j "$pe" -N 2 "$1" 2>/dev/null | tr -d ' \n')" = PE ] || return 1
    optional=$((pe + 24))
    case $(number_at "$optional" 2 "$1") in
    267) directories=$((optional + 96)) ;;  # PE32
    523) directories=$((optional + 112)) ;; # PE32+
    *) return 1 ;;
    esac
    clr=$(number_at $((directories + 14 * 8)) 4 "$1")
    [ -n "$clr" ] && [ "$clr" -ne 0 ]
}

# Records a setting of the prefix (key=value lines).
record() {
    conf=$WINEPREFIX/openshell-prefix.conf
    touch "$conf"
    grep -v "^$1=" "$conf" > "$conf.new" || true
    printf '%s=%s\n' "$1" "$2" >> "$conf.new"
    mv "$conf.new" "$conf"
}

use_prefix() {
    valid_id "$1" || fail "not a prefix name: $1"
    id=$1
    WINEPREFIX=$prefixes/$id
    export WINEPREFIX
}

# Keep the backup path separate from the program path in the caller.
backup() (
    mkdir -p "$backups"
    wineserver -w 2>/dev/null || true
    file=$backups/$1-${2:-$(date -u +%Y%m%dT%H%M%SZ)}.tar.gz
    tar -czf "$file.partial" -C "$prefixes" "$1"
    mv "$file.partial" "$file"
    echo "$file"
)

# What Wine leaves out of the prefix: Gecko always, Wine Mono unless a .NET
# program needs it.
overrides() {
    if [ "$(setting mono)" = yes ]; then echo "mshtml="; else echo "mscoree,mshtml="; fi
}

# The prefix, created on first use and brought to this Wine.
prepare() {
    current=$(version)
    if [ ! -f "$WINEPREFIX/system.reg" ]; then
        mkdir -p "$WINEPREFIX"
        # Without the display: wineboot's progress window would come and go
        # before the program's, and the Desktop ends a session whose windows all closed.
        # Nor winemenubuilder, which would make Wine's own file associations
        # Desktop entries; the registry then turns them off for good, and
        # installers' shortcuts still become entries.
        env -u DISPLAY WINEDLLOVERRIDES="mscoree,mshtml,winemenubuilder.exe=" wineboot -i
        env -u DISPLAY WINEDLLOVERRIDES="winemenubuilder.exe=" \
            wine reg add 'HKCU\Software\Wine\FileOpenAssociations' /v Enable /d N /f > /dev/null
        wineserver -w
        record id "$id"
        record created "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        record wine "$current"
    elif [ "$(setting wine)" != "$current" ]; then
        # Another Wine ran it last: a backup first, then the update.
        backup "$id" "$(setting wine | tr -c 'a-z0-9.\n' '-')" > /dev/null
        env -u DISPLAY WINEDLLOVERRIDES="$(overrides)" wineboot -u
        wineserver -w
        record wine "$current"
    fi
    # A .NET program: Wine Mono in this prefix, once, from the Wine package.
    if [ -n "${file:-}" ] && [ "$(setting mono)" != yes ] && dotnet_program "$file"; then
        record mono yes
        env -u DISPLAY WINEDLLOVERRIDES="$(overrides)" wineboot -u
        wineserver -w
    fi
    rm -f "$WINEPREFIX/dosdevices/z:"
    # Temporary files out of the prefix, which backups keep.
    # The cache may be gone (removed, or a restored backup): MSI and others
    # cannot open anything without a temporary folder.
    mkdir -p "$cache/$id/temp"
    for temp in "$WINEPREFIX"/drive_c/users/*/AppData/Local/Temp; do
        [ -d "$temp" ] && [ ! -L "$temp" ] || continue
        rm -rf "$temp"
        ln -s "$cache/$id/temp" "$temp"
    done
    # Wine runs in a folder it has a drive for: without Z:, it cannot name
    # any other (and the shell's "open" fails on it).
    cd "$WINEPREFIX/drive_c"
}

# The file the person opened (an absolute path) becomes P:\<name>, and its
# folder the one Wine runs in; prints its Windows path.
map_folder() {
    ln -sfn "$(dirname "$1")" "$WINEPREFIX/dosdevices/p:"
    printf 'p:\\%s' "$(basename "$1")"
}

case ${1:-} in
--install)
    [ "$#" -eq 2 ] && [ -f "$2" ] || fail "--install takes the installer"
    file=$(realpath "$2")
    # A prefix named after the installer: setup-tool.exe -> setup-tool, setup-tool-2, ...
    base=$(basename "$2" | sed 's/\.[^.]*$//' | tr 'A-Z' 'a-z' | tr -cs 'a-z0-9' '-' | sed 's/^-*//; s/-*$//' | cut -c1-48)
    [ -n "$base" ] || base=app
    name=$base n=1
    while [ -e "$prefixes/$name" ]; do n=$((n + 1)); name=$base-$n; done
    use_prefix "$name"
    prepare
    record installer "$(basename "$2" | tr -d '\n')"
    record install started
    installer=$(map_folder "$file")
    cd "$(dirname "$file")"
    status=0
    case $2 in
    *.msi | *.MSI) wine msiexec /i "$installer" >> "$WINEPREFIX/openshell-install.log" 2>&1 || status=$? ;;
    *) wine "$installer" >> "$WINEPREFIX/openshell-install.log" 2>&1 || status=$? ;;
    esac
    # An installer may hand over to other processes: done when they all are.
    wineserver -w
    rm -f "$WINEPREFIX/dosdevices/p:"
    if [ "$status" -eq 0 ]; then record install done; else record install "failed $status"; fi
    echo "$id"
    exit "$status"
    ;;
--configure)
    [ "$#" -eq 2 ] || fail "--configure takes a prefix"
    use_prefix "$(basename "$2")"
    [ -f "$WINEPREFIX/system.reg" ] || fail "no prefix $id"
    prepare
    exec wine winecfg
    ;;
--backup)
    [ "$#" -eq 2 ] || fail "--backup takes a prefix"
    use_prefix "$2"
    [ -d "$WINEPREFIX" ] || fail "no prefix $id"
    backup "$id"
    ;;
--restore)
    [ "$#" -eq 2 ] && [ -f "$2" ] || fail "--restore takes a backup"
    # One prefix folder, by a valid name, and nothing outside it.
    top=$(tar -tzf "$2" | cut -d/ -f1 | sort -u)
    [ "$(printf '%s\n' "$top" | wc -l)" -eq 1 ] || fail "not a prefix backup"
    use_prefix "$top"
    if tar -tzf "$2" | grep -Eq '(^/|(^|/)\.\.(/|$))'; then fail "not a prefix backup"; fi
    mkdir -p "$prefixes"
    rm -rf "$prefixes/.restoring"
    mkdir "$prefixes/.restoring"
    tar -xzof "$2" -C "$prefixes/.restoring"
    WINEPREFIX=$prefixes/$id wineserver -k 2>/dev/null || true
    rm -rf "$WINEPREFIX"
    mv "$prefixes/.restoring/$id" "$WINEPREFIX"
    rmdir "$prefixes/.restoring"
    echo "$id"
    ;;
--remove)
    [ "$#" -eq 2 ] || fail "--remove takes a prefix"
    use_prefix "$2"
    wineserver -k 2>/dev/null || true
    # Its shortcuts: the Desktop entries winemenubuilder wrote for this
    # prefix, which quote it as "WINEPREFIX=<path>" (older Wine: WINEPREFIX="<path>").
    applications=${XDG_DATA_HOME:-$HOME/.local/share}/applications
    if [ -d "$applications" ]; then
        grep -rlF -e "\"WINEPREFIX=$WINEPREFIX\"" -e "WINEPREFIX=\"$WINEPREFIX\"" "$applications" 2>/dev/null |
            while IFS= read -r entry; do rm -f "$entry"; done
    fi
    rm -rf "$WINEPREFIX" "$cache/$id"
    ;;
*)
    use_prefix default
    if [ "${1:-}" = --prefix ]; then
        [ "$#" -ge 2 ] || fail "--prefix takes a name"
        use_prefix "$2"
        shift 2
    fi
    file=
    if [ "$#" -ge 1 ] && [ -f "$1" ]; then file=$(realpath "$1"); fi
    prepare
    # The program the person opened: its own folder only, as drive P:.
    if [ -n "$file" ]; then
        program=$(map_folder "$file")
        cd "$(dirname "$file")"
        shift
        set -- "$program" "$@"
    fi
    exec wine "$@"
    ;;
esac
