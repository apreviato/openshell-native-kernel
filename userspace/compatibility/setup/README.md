# Live media setup screen

The boot menu of the OpenShell Native stick is OpenShell Live's
(`distro/image/build-media.sh`):

| Entry | What it starts |
| --- | --- |
| Start OpenShell Native Live | The live Desktop, as before. It is the default, after five seconds. |
| Install OpenShell Native on this computer | This screen, in `install` mode: time zone, keyboard, disk, account, summary and the copy, as Calamares asks them on Live. |
| Update installed OpenShell Native (keep files) | This screen, in `update` mode: the installations found on the disks, the update from the stick or the return to the previous release. It only appears on media made with the release key. |
| More options | The Secure Boot key (MokManager), the UEFI firmware settings, restart and shut down. |

The two new entries do not start the Desktop. `installed-session.sh` brings up only X, Openbox and this screen, without any session service. "Try OpenShell Native", on the screen, leaves it and starts the live Desktop.

| File | What it is |
| --- | --- |
| `openshell_setup.py` | The screen, in GTK, with the colours of Live's updater (`update_gui.py`), in steps: on the left the wordmark and the steps (done, current and upcoming), in the middle one card per step with Back and Continue (Enter continues, Esc goes back). Time zones and keyboards in searchable lists (typing over the list searches), the local time of the chosen zone, the layout applied when chosen and a box to test it, the disks as cards with their type (NVMe, SATA, USB), the summary with "Change" on each item and the progress as a percentage. It runs as the session user under the supervisor (`userspace/shell`), which does the work as root and powers the computer on and off. |
| `setup_model.py` | What the screen knows without a window: the supervisor's answers (`install.rs`, `usb_update.rs`, `power.rs`); each disk's type; the time zones from `/usr/share/zoneinfo/zone1970.tab` and the layouts from `/usr/share/X11/xkb/rules/base.lst`, the same files Settings uses; and the ordering between releases. |
| `test_setup_model.py` | The model's tests (`python3 -m unittest test_setup_model`), without GTK. |

The work is done by two programs on the media, which the supervisor runs as root:

- `openshell-native-install` (`distro/image/install.sh`): erases the chosen disk and writes the system from memory onto it. First, when the media has a manifest, it checks that what the computer read from the stick is what was generated (`medium.sh`, `release_intact`).
- `openshell-native-update` (`distro/image/update.sh`): writes the media's release to the installed system's slot that is not in use (`openshell-ab update-offline`), which checks the signed manifest and every file written. On the next start, the new release runs on trial. The session confirms it, and three unsuccessful starts go back to the previous release. Data, the account and settings stay as they were. "Put back the previous release" puts the release from before the last update on trial (`openshell-ab restore`).

The release's signed manifest goes on the media kernel's command line (`openshell.manifest`, `openshell.manifest_sig`), not in the initramfs, because it covers the initramfs itself.
