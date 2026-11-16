# Wine on OpenShell Native — compatibility matrix

Status of the phases of the [Wine plan](../../../OPENSHELL_WINE.md). The `wine` action of `tools/build/native.mjs` builds the Windows apps profile on top of the Linux apps one and runs the checks below on the control Linux and in the Asterinas guest.

## Pinned versions

| Piece | Version | Source |
| --- | --- | --- |
| Wine | 11.0, 64-bit with WoW64 (`wineWow64Packages.stable`) | nixpkgs pinned in `distro/nixpkgs.nix` |
| Kernel | Asterinas v0.19.0 (`c4b7ed5`) with patches 0001 to 0017 | [kernel/patches](../../../kernel/patches/README.md) |
| CEF | 147.0.10 | pinned nixpkgs |
| Xpra and HTML5 client | 6.4 and 17.1 | pinned nixpkgs |

Wine Mono and Gecko are not part of the default prefix: it is created without them (`WINEDLLOVERRIDES=mscoree,mshtml=`). A .NET program gets Wine Mono in its own prefix (W5); programs that embed the Windows HTML engine (Gecko) remain out of scope.

## Running

- [openshell-wine.sh](openshell-wine.sh) is the Wine profile's runner. Opening a single file uses the `default` prefix, and the install flow creates one prefix per app in `~/.local/share/openshell/wine/prefixes`. The prefix is created without a display with `wineboot`. The runner removes the Z: mapping and maps the opened file's folder as P:. The process runs with the session user's permissions, including for accesses outside the drive mappings.
- The Desktop recognises `.exe` and `.msi` (`apps/desktop/src/host/openWith.ts`, in the `openshell` repository). Opening one of these files from Files or with the Terminal's `open` shows the chooser, with Wine recommended; running is always an explicit choice.

## Checks

| Check | How | Control Linux | Asterinas |
| --- | --- | --- | --- |
| `wine --version`, `wineboot -i` in an isolated prefix | [wine-smoke.mjs](../../../tools/desktop/wine-smoke.mjs) | Passes: prefix in 13 s | Pending |
| Notepad opens on X11 and closes with Alt+F4 | `wine-smoke.mjs` | Passes: window in 0.8 s | Pending |
| Nothing from the prefix is left behind, `wineserver` included | `wine-smoke.mjs` | Passes | Pending |
| An `.exe` opened from the Terminal chooses Wine and opens in a Desktop window (Xpra) | `linux-apps` phase of the [shell smoke](../../../tools/desktop/shell-smoke.mjs), with `OPENSHELL_SMOKE_WINE=1` | Passes | Pending |
| The OpenShell prefix has no Z: drive | same | Passes | Pending |
| Closing the window ends Wine | same | Passes | Pending |
| W4: an MSI installer ([probe.wxs](probe.wxs)) installs into its own prefix, with a Desktop shortcut and without Wine's file associations | `wine-smoke.mjs`, with `OPENSHELL_WINE_PROBE_MSI` | Passes | Pending |
| W4: the program opens from the shortcut, as the Desktop opens it, and its settings survive between runs | same | Passes | Pending |
| W4: backup, removal (with the shortcut) and restore of the prefix, with its settings | same | Passes | Pending |
| W4: another Wine ran the prefix last: backup before the update, and the recorded version | same | Passes | Pending |
| W4: uninstall through the MSI, and removal of the prefix with no processes left | same | Passes | Pending |

## Phase status

| Phase | Status |
| --- | --- |
| W0 · inventory and baseline | Pinned versions and checks on the control Linux. |
| W1 · Wine in userspace | Passes on the control Linux; the guest runs the same check in the `wine` action. |
| W2 · opening `.exe` | `.exe` and `.msi` association, foreground runner, cleanup by `hostd` on close (the whole Xpra session). |
| W3 · windows on the Desktop | Through the Linux apps' Xpra path: the program's window is an OpenShell window, with the Linux apps' dock, focus, keyboard and clipboard. |
| W4 · per-app prefixes, installation | The runner and a Settings panel offer per-installation prefixes, logs, backup and restore. The `wine-smoke` W4 gate passes on the control Linux. On Asterinas, it runs in the `wine` action and is still pending. Prefixes live in the person's data (`XDG_DATA_HOME`), outside the slots an update or a rollback replaces. |
| W5 · advanced compatibility | In progress. .NET programs: the runner recognises the PE's CLR header (PE32 and PE32+) and installs the Wine Mono from Wine's package only in that program's prefix, on its first run; the other prefixes stay without it. Still missing: the `wine-smoke` checks (Win32 through WoW64, .NET), Gecko and the VC runtimes on demand, and the launch and RAM measurements. |
