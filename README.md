# OpenShell Native

[![License: MPL 2.0](https://img.shields.io/badge/License-MPL_2.0-brightgreen.svg)](LICENSE)
[![Open Source: OSI approved](https://img.shields.io/badge/Open_Source-OSI_approved-3DA639?logo=opensourceinitiative&logoColor=white)](https://opensource.org/license/mpl-2-0)
[![Kernel: Rust](https://img.shields.io/badge/kernel-Rust-B7410E?logo=rust&logoColor=white)](kernel/patches/README.md)
[![Based on Asterinas v0.19.0](https://img.shields.io/badge/based_on-Asterinas_v0.19.0-6E4AFF)](https://github.com/asterinas/asterinas)
[![Linux ABI](https://img.shields.io/badge/ABI-Linux-FCC624?logo=linux&logoColor=black)](ARCHITECTURE.md)
[![Platform: x86-64 UEFI](https://img.shields.io/badge/platform-x86--64_UEFI-555555)](#building-the-media)
[![Secure Boot: shim + MOK](https://img.shields.io/badge/Secure_Boot-shim_%2B_MOK-2E7D32)](#secure-boot)
[![Status: experimental](https://img.shields.io/badge/status-experimental-orange)](ARCHITECTURE.md#4-roadmap)

The experimental x86-64 base of OpenShell, with a Rust kernel and the Linux ABI. The [implementation plan](ARCHITECTURE.md) describes the system's base. The [CEF plan](ARCHITECTURE-CEF.md) defines the graphics integration of the Native Desktop.

## Boot and compatibility

Use Node.js 22+ and a Linux amd64 Docker engine. On Windows, use Docker Desktop in Linux mode. The first build downloads the official Asterinas image, the source code and the Rust and Nix dependencies. QEMU uses TCG in the container, with disposable virtual disks.

### Building the media

```bash
make bundle
make iso
make install-image
```

```powershell
.\make-live.ps1
.\make-live.ps1 -BuildOnly
.\make-live.ps1 -BuildOnly -InstallImage
```

On Windows, run `make-live.ps1` as administrator to create a USB stick. The script lists the eligible USB devices, asks you to confirm the number and only then starts the build. It checks the identity again before erasing the device and compares the SHA-256 read back from the stick with the image's. The partition table (the first 4 MiB) is written last: until then Windows sees no partitions on the stick, and the rest is written and verified before it mounts the EFI partition and writes to it (`System Volume Information`), which would make the verification fail. Use `-Iso <file>` to write an existing ISO with its `.sha256`, `-DiskNumber <number>` to name the device directly and `-WhatIf` to see the plan.

If the bundle already exists, use `BUNDLE=/path/to/bundle` with Make or `-Bundle <directory>` in PowerShell. Each run produces the media, the `.sha256` file and the `run.json` report in `out/native-*/`. `make iso` produces a live x86-64 UEFI ISO with temporary state. `make install-image` produces a reference GPT disk with A/B slots, recovery and persistent data. Both media include the Desktop, Xpra and Wine 11.0. PowerShell checks the SHA-256 after copying the media to the host.

Behind an antivirus or proxy that inspects HTTPS (such as Norton's Web/Mail Shield), the build container would not trust the certificates it sees. On Windows the build copies the host's trusted roots into the container, and only when the image's own bundle cannot verify `github.com` does it add the one root that signs it ([run.sh](tools/qemu/run.sh)).

#### Secure Boot

The media boot like OpenShell Live's: the firmware loads Debian's shim, signed by Microsoft, and the shim loads the GRUB signed by Debian, in the versions pinned in [secureboot.lock](distro/image/secureboot.lock). Asterinas cannot carry Debian's signature. So the kernel is signed with the OpenShell Native key, and GRUB asks the shim to check it against the machine owner's keys (MOK). With Secure Boot on, each computer enrolls the certificate once:

1. In the stick's menu, choose **More options ▸ Enroll the OpenShell Native key**. Debian's MokManager opens.
2. Choose **Enroll key from disk**, the stick's volume, `ENROLL_THIS_KEY_IN_MOKMANAGER.cer`, **Continue** and **Yes**, and restart.

Without the certificate, GRUB refuses the kernel ("bad shim signature"). In that case, any menu entry shows the steps above and opens MokManager by itself, and the installed disk does the same when it cannot start any slot. Live does not ask for this because it boots Debian's own Linux kernel, signed with the Debian key the shim already carries. Besides that key, the shim only accepts the MOK ones, and Debian does not sign other kernels. Skipping the enrollment would need an OpenShell shim with its key built in and signed by Microsoft (the shim review process, done by whoever publishes the distribution), or turning Secure Boot off. With Secure Boot off, nothing is asked. Debian's signed GRUB has no `multiboot2` under Secure Boot. So the kernel is built with `BOOT_PROTOCOL=linux-efi-handover64` and enters through GRUB's `linux`/`initrd`, also in the QEMU validations.

The key is created by the first build and kept in `.secureboot/` (outside Git); later builds reuse it, so the enrolled certificate stays valid. Keep a copy: media signed with another key ask for a new enrollment. `OPENSHELL_SECURE_BOOT_KEYS` points to another folder. The disk test ([boot-test.sh](distro/image/boot-test.sh)) boots the slot with OVMF in Secure Boot with Microsoft's keys: first without the certificate, with the kernel refused, and then with it.

#### USB stick menu

The stick opens OpenShell Live's menu, with the same design: the OPENSHELL wordmark with "NATIVE" and the version ([menu.png](distro/image/menu.png), made by [make-menu.py](distro/image/make-menu.py) like Live's `splash.svg`), the entries below, the timeout bar and GRUB's Unifont font ([menu-theme.txt](distro/image/menu-theme.txt)). The font is the one Debian's signed GRUB carries inside itself: with Secure Boot on, it refuses fonts read from disk ("prohibited by secure boot policy") and would show the text menu. The first entry starts by itself after five seconds.

| Entry | What it starts |
| --- | --- |
| **Start OpenShell Native Live** | The live Desktop, with temporary state. |
| **Install OpenShell Native on this computer** | The installer screen, without the Desktop ([Installation](#installation)). |
| **Update installed OpenShell Native (keep files)** | The screen that updates an installed system from the stick ([Updating from the USB stick](#updating-from-the-usb-stick)). It only appears on media built with the release key. |
| **More options** | Enrolling the Secure Boot key, the UEFI firmware settings, restart and shut down. |

Each entry tells the system what to start (`openshell_mode`, in [installed-session.sh](userspace/compatibility/probes/installed-session.sh)). Install and update open the [media setup screen](userspace/compatibility/setup/README.md), in the look of Live's updater, in a session of its own: X, Openbox and the screen, without the Desktop and without services. **Try OpenShell Native** leaves it for the live Desktop.

#### Loading screen

Boot shows the same loading screen as OpenShell Live: the OPENSHELL wordmark and the bar over the Desktop's near-black. GRUB draws [splash.png](distro/image/splash.png) (made by [make-splash.py](distro/image/make-splash.py) from the images of Live's Plymouth theme, in the same geometry: the wordmark and "starting the system…", without the bar) while it loads the kernel and the initramfs, and the ESP's [splash.cfg](distro/image/splash.cfg) serves the stick's menu and the installed disk. GRUB hands over the screen as it is (`gfxpayload=keep`), the EFI stub does not write to it (patch [0022](kernel/patches/README.md)) and the kernel does not clear it (patch 0023). From the start of the kernel until the session appears, the bar's segment moves back and forth as in Live's Plymouth (patches 0055 and 0066): the kernel recognises the splash on the screen, draws the bar with Plymouth's colours in its place and animates it while the initramfs is decompressed and the session starts, and X's first frames, still empty, stay off the screen. The Desktop's own loading screen (or the installer's), with the same design, takes over when it draws, and then sign-in comes. While GRUB reads the kernel and the initramfs from the stick (a minute or more on a slow stick), the screen has no bar: GRUB would not move it during the read, and a frozen bar looked stuck. GRUB's messages still go to the serial port, which the tests read.

#### Display performance

Native has no GPU driver: the screen is the firmware's framebuffer, through simpledrm. Everything is drawn on the processor, and each layer avoids work that would only make sense with a GPU:

- Chromium composites in software (`--disable-gpu-compositing`), instead of emulating a GPU with SwiftShader or llvmpipe on every frame. WebGL stays on SwiftShader.
- The Desktop declares itself GPU-less, as Live does on such machines, and leaves out background blurs, which would be recomputed on every frame under the dock and the menus.
- The kernel copies to the screen only the areas X reports as changed (patch [0025](kernel/patches/README.md)). Before, every pointer movement copied the whole screen.
- X receives the apps' images through shared memory (MIT-SHM), which the kernel now offers along with SysV shared memory (patch 0026). Before, Chromium, Xpra and GTK apps sent every image through the X socket.

#### Screen, keyboard and mouse

The media's session shows the Desktop on the computer's screen. [openshell-x](userspace/compatibility/probes/openshell-x.sh) starts Xorg with the `modesetting` driver on the kernel's `/dev/dri/card0` (simpledrm on the UEFI framebuffer, at the resolution the firmware handed over), with the `evdev` driver on each `/dev/input/event*`. Without udev or logind, the script hands the screen, the inputs and the console to the session user before starting it, as logind does with the active seat, and X runs without privileges. Without DRM, it starts Xvfb, as in the QEMU smokes. In QEMU with a UEFI framebuffer, X draws on the firmware's screen (captured with `screendump`), and keys and the PS/2 mouse reach X. The keyboard depends on patch [0019](kernel/patches/README.md). USB keyboards and mice of any brand work, plugged in before or after boot: the kernel drives the xHCI controllers and the HID class (patch [0031](kernel/patches/README.md)), with wheel, horizontal wheel, extra buttons and media keys; since X gains no devices after it starts, all of them come in through `usb_keyboard` and `usb_mouse`. Hubs, USB sticks, network and Bluetooth adapters, USB headphones and speakers also work (patches 0032 to 0053), and programs talk to other USB devices through libusb (patch 0052). The touchpad has the gestures libinput gives Live: the kernel turns on the absolute mode of Synaptics and ELAN touchpads (versions 3 and 4) and turns fingers into a pointer, tap to click (one finger: left, two: right, three: middle), natural two-finger scrolling, the clickpad's bottom-right corner as the right button and no touch that starts while typing (patch [0028](kernel/patches/README.md)). X receives a mouse with wheels, because without udev libinput does not work. Other touchpads (ALPS, older ELAN) remain a PS/2 mouse, and the kernel log says which one was found. I2C-HID touchpads (`PNP0C50`, most laptops since 2016) still have no driver: they work through the fallback PS/2 mode, without two-finger scrolling when that mode speaks neither Synaptics nor ELAN, and the kernel names the ones the ACPI tables declare (patch 0066). On them, and on any mouse, holding the right button and moving also scrolls. A PS/2 mouse with a wheel scrolls with it. Settings ▸ Cursor changes the pointer's colour and size, as on Live: the session starts the Desktop's cursor helper before it, and programs use the "OpenShell" theme. There is no 3D acceleration; the screen uses the firmware's framebuffer, with no resolution changes and no second monitor.

#### Sign-in

The installed system asks for the password before the Desktop. The `openshell-session` supervisor, as root, starts X and Openbox and then the Desktop as a greeter. It is the same bundle, on the sign-in page, as the session user. The shell only starts after the right password. The password is chosen in the installer, and there is no way to recover it; a system without a password (installed by an earlier version) creates one on first boot, on the sign-in page. The SHA-512 crypt hash (`$6$`, from BusyBox's `cryptpw`, which reads the password from a descriptor and never from its arguments) lives in `/data/openshell/account/password`, owned by root, mode 0600. The page asks through hostd, which relays to a Unix socket of the supervisor. The supervisor only answers processes of the session user (`SO_PEERCRED`) and waits 2 s before each answer to a wrong password. The live session signs in without a password, like OpenShell Live's. The [session smoke](tools/desktop/session-smoke.mjs) requires the shell to start only after sign-in, and the disk test ([image-boot.sh](userspace/compatibility/probes/image-boot.sh)) signs in to the installed Desktop through the same socket.

#### Screen lock and power

The lock is OpenShell Live's, with the same screen: XSecureLock covers the screen and grabs the keyboard and mouse, and Live's lock screen is its authentication module ([lock/](userspace/compatibility/lock/README.md)). The password is the sign-in one: `openshell-authproto` checks it with the supervisor over the system socket, with the same wait after a wrong password. The Lock button and the idle time in Settings ▸ Lock screen lock the screen; on Native, the Desktop reports to hostd the idle time X records, because there is no logind for `xss-lock`. The live session, without a password, does not lock, like Live's. The lock screen keeps the password only in locked memory, and the kernel accepts `mlock` (patch 0024).

Shutting down and restarting, from the Desktop and the lock screen, also go through the supervisor, which has no `CAP_SYS_BOOT`: it ends the session and exits with the action's code, and init flushes the data disk (`sync`, unmount) and powers off or restarts. There is no suspend.

#### Installation

**Diagnostic report.** In live mode (and on the install and update screens), one minute after the session starts and again eight minutes later, [diagnostics.sh](userspace/compatibility/probes/diagnostics.sh) writes `OSNDIAG.TXT` to the stick's own FAT partition, which Windows, macOS and a phone can read: a summary (the Wi-Fi cards by PCI class, the interfaces created, what PS/2 found and the I2C touchpads from the ACPI tables), each PCI device as `lspci -nn` shows it (the kernel logs them since patch 0066), the USB devices, the network interfaces, the Wi-Fi log and the supervisor's, X's input lines and the kernel log. It contains no passwords; the Wi-Fi log has the names of nearby networks. A copy stays in the session logs (`osndiag.txt`).

The live stick installs the system, like Calamares on Live: through the **Install OpenShell Native on this computer** menu entry, on a screen of its own in steps (time zone, keyboard, disk, account, summary and the copy, listed on the left; search in the time zone and keyboard lists; Enter continues and Esc goes back), or through **Install OpenShell** on the live Desktop, a guided setup in the same steps (location, keyboard, disk, account, summary with an explicit erase confirmation, the copy by stage and restart), which lists the disks (NVMe, SATA, USB and VirtIO, of 16 GiB or more, never the live stick itself). The supervisor runs the [installer](distro/image/install.sh) as root. It creates the reference disk's GPT table (ESP, `system_a`, `system_b`, recovery and data), writes the signed chain and the splash to the ESP, the version to slot A, already confirmed, and the same version as recovery. Before that, like Calamares, it asks for the name, the password (twice), the keyboard and the time zone; the supervisor checks each one and keeps only the password's hash, and the installer writes everything for the installed system, which opens straight on the lock screen. The live system runs entirely from memory, and the installer copies the initramfs the computer loaded, exposed in `/dev/initrd` (patch [0021](kernel/patches/README.md)). First, when the media has the signed manifest, it checks that what the computer read from the stick is what was generated: a badly read stick is not installed. The live system needs memory for the decompressed system on top of normal use; the installed system only on the first boot of each version ([Memory](#memory)). So the media leaves out Mesa (CEF uses its SwiftShader for WebGL) and the test applications. The [installer test](distro/image/install-test.sh) installs onto a QEMU NVMe and boots the disk by itself through the signed chain.

The system installs and boots on NVMe, SATA (patch 0037), USB and VirtIO disks; boot waits up to 15 s for disks that appear after the kernel (SATA and USB). RAID controllers that are not AHCI, SAS and SD cards in PCI readers have no driver yet.

#### Memory

The installed system no longer lives entirely in memory. Before, each boot decompressed the version's initramfs (2.8 GB, with Wine, CEF, Python and Node) into RAM and still kept the compressed one (1 GB), almost 4 GB before the Desktop. Now the first boot of a slot after installation or an update still runs from memory and, in the background and at low priority, copies the system from `/dev/initrd` to `/data/openshell/system-root/<slot>` ([installed-session.sh](userspace/compatibility/probes/installed-session.sh)). From then on, the slot's `boot.cfg` ([release.sh](distro/image/release.sh)) loads an initramfs of a few megabytes ([disk-root.sh](userspace/compatibility/probes/disk-root.sh), with the image's BusyBox): it mounts the data partition, makes the copy the root and puts the partition itself on `/data` (the same mount, never a second one of the same disk) and `/tmp` in memory. The kernel reads from disk what is used and caches it, like Live's squashfs.

Each copy's GRUB environment block (`<slot>.env`) says which version it is good for. GRUB marks the attempt before handing over control; the system marks the copy as good after 90 s running, when the Desktop comes up or on a clean shutdown. An attempt that did not get there, a copy of another version or none at all makes GRUB boot from memory as before, and that boot redoes the copy; after two failures, the slot stays in memory until the next update. An update invalidates the copy of the slot it writes. The copy needs 6 GiB free on the data partition (the other slot's goes first if space is short). In QEMU, booting from the copy comes up with the root on `nvme0n1p5`, the same partition on `/data` and almost all RAM free, and the three boots (good copy, redone attempt, unmarked attempt) take the expected path. The live system and recovery remain in memory.

#### System settings

Live changes the time zone with `timedatectl` and the account name with `sudo chfn`. Native has neither, and its root lives in memory. In Settings, the Desktop asks the supervisor for those changes over the system socket (`timezone <Area/City>`, `name <name>`, [settings.rs](userspace/shell/src/settings.rs)). The supervisor points `/etc/localtime` at the `tzdata` zone and changes the account's name in `/etc/passwd`. On the installed system, it keeps both in `/data/openshell/system` and applies them again on every boot, before the session. The clock is the firmware's: there is no network time and no manual time setting. Settings ▸ Display uses `xrandr` and shows simpledrm's only output. Brightness, in Settings and on the keys, is the built-in screen's backlight (`/sys/class/backlight`, patch 0030), which only root writes: the Desktop asks the supervisor (`brightness <1-100>`), which writes it and reapplies it on every boot, like Live's systemd-backlight.

#### Logs, processes and performance

Native has no journald. Settings ▸ System ▸ Logs shows, under **System**, the kernel's records (`/dev/kmsg`, which the kernel now keeps even without a serial console, patch [0029](kernel/patches/README.md)) and this boot's logs that the supervisor keeps in `session-logs/NNNN` (its own journal, X and Openbox); under **My session**, the Desktop and hostd (`apps/desktop/hostd/native/logs.ts` in the OpenShell repository). The supervisor writes the logs as root, and the session's group reads them. The Services tab says Native has no systemd services. Processes reads `/proc` when `ps` has neither `-o` nor `--sort` (BusyBox's), with the same CPU and memory calculation as `ps`. Performance uses `/proc/stat`, `/proc/meminfo`, `/proc/loadavg`, `/proc/net/dev` and `/proc/diskstats` (patch 0036), as on Live; there are no temperature sensors.

#### Parity with OpenShell Live

| Live feature | On Native |
| --- | --- |
| Boot through Debian's shim/GRUB with Secure Boot | The same; the kernel's certificate is enrolled once in MokManager (see [Secure Boot](#secure-boot)). |
| Loading screen (Plymouth) | The same, drawn by GRUB and kept until the Desktop ([Loading screen](#loading-screen)). |
| Password sign-in, screen lock, shut down and restart | The same ([Sign-in](#sign-in), [Screen lock and power](#screen-lock-and-power)); no suspend. |
| Installation through Calamares | The live Desktop's installer, with the A/B disk ([Installation](#installation)). |
| Desktop, Terminal, Files, Browser, Linux (Xpra) and Windows (Wine) apps | The same, on CEF ([CEF shell](#cef-shell)). |
| Time zone, account name, keyboard, display | Through the supervisor and the X tools ([System settings](#system-settings)); name, password, keyboard and time zone also in the installer. |
| Touchpad (libinput: tap, natural scrolling, clickpad, typing) | Synaptics and ELAN v3/v4 through the kernel; others as a mouse ([Screen, keyboard and mouse](#screen-keyboard-and-mouse)). |
| Cursor colour and size | The same, through the same Desktop helper. |
| Full keyboard, remembered Num Lock, Print Screen | The same (patch 0027, numlock.py). |
| Brightness (keys and Settings) | Intel laptop screens, through the backlight's PWM (patch 0030), kept between boots; AMD not yet. |
| USB keyboard and mouse | The same, of any brand, also plugged in after boot and behind hubs (patches 0031 and 0032). |
| Sound and volume keys | Intel HDA (Realtek, Conexant, Cirrus and other codecs through Linux's generic path), with the session's ALSA and PulseAudio: volume, mute, microphone and switching to headphones when plugged in (patch 0034). USB headphones, headsets, speakers, microphones and sound cards (USB Audio 1.0 and 2.0, patch 0053): each becomes an ALSA card when plugged in, with volume and mute under Linux's names, and PulseAudio starts playing and recording through it, as on Live; when it leaves, it goes back to the previous one. Missing: HDMI/DisplayPort, per-model quirks, the DSPs' digital microphones and USB devices with implicit feedback. |
| Logs, processes, performance | From the kernel and the supervisor, without journald ([Logs, processes and performance](#logs-processes-and-performance)). |
| Memory (squashfs root) | The installed system boots from the copy on the data partition after the first boot of each version ([Memory](#memory)). |
| Wired network | Intel (e1000/e1000e up to the I219, igb, I210/I211, I225/I226) and Realtek (RTL8111/8168/8125 and the Killer chips that are them), with DHCP addressing, and VMs' VirtIO (patches 0032, 0035 and 0036). The Network page shows interfaces, addresses, gateway, DNS and connectivity, without NetworkManager: read-only. |
| Internet through a phone and USB adapters | USB tethering from Android (RNDIS) and other devices and CDC ECM/NCM adapters, through DHCP (patch 0032). Missing: adapters with their own chip (ASIX, Realtek RTL815x). |
| SATA disks, USB sticks, USB disks and cards | SATA through AHCI (internal SSDs and HDDs, patch 0037) and USB 2 and 3 (Bulk-Only), MBR and GPT, FAT12/16/32, exFAT and NTFS (through ntfs-3g, with the kernel's FUSE, patch 0049, the way Debian opens it), USB devices plugged and unplugged while running, mounted by the Desktop as on Live (patches 0032, 0033 and 0037). NTFS was tested in QEMU (write, read, rename, delete, unmount and mount again); checking on a real Windows disk is missing. Missing: UAS mode and optical drives. |
| Local models, dictation and read-aloud | The same as Live's: the `llama-server` of llama.cpp b11960, the commit Live builds, on the CPU with the library for the processor's best instruction set (no Vulkan: Native has no GPU driver); Whisper and Piper run in the Desktop page, with the microphone and sound through PulseAudio. |
| Wi-Fi | Everything above the chip is ready and tested in QEMU with a simulated radio: the kernel speaks nl80211 (patches 0039 to 0041) and Live's `wpa_supplicant` runs unmodified (WPA2 and WPA3 in it), the session supervisor commands it with checked requests (its socket is root-only) and the Network page lists, connects, saves, forgets and turns Wi-Fi on and off as on Live. The drivers for the five main vendors are written, with scanning, association and WPA2 keys (CCMP): **Intel** BE200, BE201, BE202, AX200, AX201, AX210, AX211, AX411, 9260, 9560, 9461, 9462, 8260, 8265, 8275, 4165, 7260, 7265, 3160, 3165 and 3168 (patches 0045 and 0058), **MediaTek** MT7925, MT7921, MT7922 and MT7920 (patch 0059), **Realtek** RTL8922AE, RTL8852AE, RTL8852BE, RTL8852BTE, RTL8852CE, RTL8851BE (patch 0063), RTL8821CE, RTL8822BE, RTL8822CE and RTL8723DE (patch 0060) and the same chips in **USB adapters** (RTL8821CU/RTL8811CU, RTL8822BU, RTL8822CU and RTL8723DU; the first-generation 802.11ac RTL8812AU and RTL8821AU/RTL8811AU; the 802.11ax RTL8851BU, RTL8852AU, RTL8852BU and RTL8852CU; MediaTek MT7921AU and MT7925U; with the IDs of dozens of brands that Linux lists; one of these adapters solves the laptop whose internal card has no driver), **Qualcomm** QCA6390, WCN6855, QCA2066, QCA6698AQ, QCN9074 (patch 0064), QCA6174, QCA6164 and QCA9377 (patch 0061) and **Broadcom** BCM4350, 4356, 43570, 4358, 4366, 4371, 43602 and 54591 (FullMAC, patch 0065), on a compatibility layer for Linux's driver API (LinuxKPI, patch 0057). 40 and 80 MHz channels (160 where the chip has it) with HT, VHT and HE (the Intel 7000 to 9000 with HT40 and VHT up to 80 MHz); **802.11w** (management frame protection, required by WPA3) on every radio (BIP in the kernel, as mac80211 does, and management CCMP in software on rtw88); **TKIP** as the group cipher of mixed WPA/WPA2 networks on Realtek, MediaTek and Qualcomm; Intel firmware restarted after an error (three times an hour); **6 GHz** on those with the band (Intel AX210, AX211 and BE200, MediaTek MT7922 and MT7925, Realtek RTL8852CE and RTL8922AE, Qualcomm ath11k) and **EHT (Wi-Fi 7)** on the BE200 (up to 320 MHz), the MT7925 and the RTL8922AE; block ack and power saving on all of them; the Realtek 802.11ax chips calibrated by the host as on Linux (DACK, RX DC, IQK, DPK and TSSI, with thermal tracking; `rtw89.nocal` turns off the per-channel ones). **Hotspot**: the Network page starts an access point through Live's hostapd and shares the connection through the kernel's NAT and DHCP (patch 0062), on every family. **Not yet tested on a real card** (QEMU has no model of them; the hotspot and Wi-Fi are tested with the simulated radio): the kernel log says how far each one gets (`dmesg \| grep -iE 'iwlwifi\|mt7921\|rtw8\|ath1\|brcmfmac'`), and `iwlwifi.off`, `mt7921.off`, `rtw88.off`, `rtw89.off`, `ath10k.off`, `ath11k.off` or `brcmfmac.off` on the kernel command line turns a driver off. The cards' firmware (`probes/wireless-firmware.list`, from linux-firmware) is in the image. Missing: MLO (Wi-Fi 7 multi-link), TKIP on Intel, the RTL8922AU (Wi-Fi 7) and RTL8814AU USB adapters and switching Realtek adapters to USB 3, and aligning Realtek's TSSI measured with the chip's own packets. Cards whose Linux drivers are GPL-only (Intel 6000/2200/1000, Realtek RTL8723BE/RTL8188EE/RTL8821AE from rtlwifi, Ralink, Marvell) are not ported. |
| Firewall | Settings ▸ Firewall as on Live, without ufw: the kernel filters incoming connections (patch 0043) and the session supervisor keeps the rules and restores them on every boot. On and off, allow, deny and limit rules by port, protocol and source; what is not allowed is denied, outgoing traffic is free. Tested in QEMU with real traffic. No outbound or forwarding rules. |
| Remote access | Settings ▸ Remote access as on Live. **Screen**: `x11vnc` shares the session with the page's password, with control or view-only. **SSH**: OpenSSH's `sshd`, which the supervisor turns on and off on request; the computer's account signs in, with its password (checked the way the lock screen checks it, through PAM) or keys in `~/.ssh/authorized_keys`, with a terminal and `scp`/`sftp`; port 22 is opened in the firewall while it is on, and the computer's keys live on the data partition. Only on the installed system (the live session has no password) and only over IPv4. Tested in QEMU with outside clients (patches 0044 and 0046). |
| Printers | Settings ▸ Printers as on Live: CUPS with cups-filters' filters and Ghostscript. Network printers by address (`ipp://`, `ipps://`, `socket://`, `lpd://`), driverless (IPP Everywhere) or with CUPS's drivers; the session's user adds and removes printers without a password (CUPS only serves this computer). Tested in QEMU with simulated network printers: PDF, text (with fonts) and PCL raster, cancel, remove, restart. Network printers discovered as on Live (mDNS/DNS-SD): the system D-Bus and Avahi run with the session, and the Settings list (`lpstat -e`) shows the driverless printers advertised on the network; the computer is called `openshell` and is `openshell.local` on the network, and `.local` names resolve for every program (nss-mdns through nsncd, as on NixOS). Tested in QEMU until the printer appears and the name resolves; printing to a discovered printer is the CUPS path already tested by address, checking with a real printer is missing. **USB printers** as on Debian (patch 0052): those that speak IPP over USB (most made since 2015) join the Settings list with the network ones, served by `ipp-usb` and advertised by Avahi, and CUPS prints to them driverless; CUPS's `usb` backend reaches the others through libusb. The kernel gives USB devices to programs as Linux does (`/dev/bus/usb`, `/sys/bus/usb` and the uevents, which libusb uses without udev). Tested in QEMU with simulated USB printers: the IPP-over-USB one appears in the list and receives a PDF through a driverless queue; the `usb` backend finds the other one and prints to it; checking with a real printer is missing. |
| Bluetooth | Settings ▸ Bluetooth as on Live, with unmodified BlueZ: `bluetoothd` runs with the session on the system D-Bus and the page talks to it through `bluetoothctl`. The kernel serves the controllers (patch 0050): `AF_BLUETOOTH` sockets with Linux's management interface, USB adapters (btusb) and the firmware Intel and Realtek chips load (from linux-firmware, in the image). On and off, name, discoverable, scan, **pair, connect, disconnect and forget** (BR/EDR with Secure Simple Pairing; LE with **Secure Connections** (P-256, numeric comparison or passkey) or legacy pairing, the keys kept by `bluetoothd` on the data partition) pass in QEMU, with BlueZ's emulator on both sides and with a simulated USB adapter; **RFCOMM channels** (headset controls, serial port, OBEX), **SCO and eSCO links** (call voice, CVSD and mSBC) and **`/dev/rfkill`** (airplane mode, `rfkill block bluetooth`) too, and BlueZ's own kernel test suites (`rfcomm-tester`, `smp-tester`, `sco-tester`, `l2cap-tester`) pass on what Linux 6.18 also does. Bluetooth keyboards and mice type and point through `/dev/uhid`, as on Linux; headphones and speakers come in through the session's PulseAudio (its Bluetooth modules), **not yet tested with real headphones**. Call voice (SCO, the headset's microphone and earpiece through PulseAudio's HFP) goes through USB adapters over isochronous transfers, as on Linux (patch 0054), tested in QEMU with a simulated adapter. Missing: LE credit-based channels, LE privacy, the MediaTek, Qualcomm and Broadcom chips, and the PCIe-attached Intel ones. To check on hardware: the kernel log (`Bluetooth: hci0` lines) says whether the controller came up and which firmware it loaded. |
| System updates | Settings ▸ Updates, through Native's release channel, written to the other A/B slot on restart ([Release channel](#release-channel)). There is no "restore": a version that does not boot goes back to the previous one by itself. |

### Development probes and builds

```text
node tools/build/native.mjs doctor
node tools/build/native.mjs build
node tools/build/native.mjs smoke
node tools/build/native.mjs control
node tools/build/native.mjs compat
node tools/build/native.mjs sandbox
node tools/build/native.mjs baseline
node tools/build/native.mjs image
node tools/build/native.mjs desktop-image <openshell-desktop-bundle>
node tools/build/native.mjs graphics
node tools/build/native.mjs desktop <openshell-desktop-bundle>
node tools/build/native.mjs kiosk <openshell-desktop-bundle>
node tools/build/native.mjs apps <openshell-desktop-bundle>
node tools/build/native.mjs wine <openshell-desktop-bundle>
```

| Command | Expected result |
| --- | --- |
| `doctor` | Checks the engine and shows the expected revision and Rust version. |
| `build` | Produces the upstream diagnostic ISO for QEMU UEFI with GRUB/Multiboot2 and an initramfs. |
| `smoke` | Does two upstream control boots and one boot with PTY, static ELF and VirtIO ext2 I/O probes. |
| `control` | Runs the Node, Python and seccomp probes on the builder's Linux with the versions pinned by the same nixpkgs as the guest. |
| `compat` | Boots the initramfs with the runtimes and requires the Node and Python probes to pass. The seccomp and namespace results also appear in the log. |
| `sandbox` | Requires, besides the Node and Python probes, proof that the kernel applied seccomp filters and that an unprivileged process entered new user, PID and network namespaces. |
| `baseline` | Runs the Linux control, the launcher's Rust tests, the three smoke boots and the compatibility boot. Records the seccomp and namespace results to decide whether the graphics tests can go ahead. |
| `image` | Tests disposable GPT disks with OVMF and GRUB. Checks the update of the inactive slot from Asterinas, three attempts without confirmation and rollback, confirmation of the new slot and recovery with missing metadata or boot settings. The per-boot logs live in `out/native-*/native-image/`. |
| `desktop-image` | Starts the Desktop bundle from the slot on trial and uses the session installed in `data` to confirm the boot. A second boot checks the persisted confirmation. |
| `graphics` | Builds the Rust shell and the CEF host against the pinned nixpkgs' CEF 147 and boots an initramfs with Xvfb, Mesa, Openbox, xdotool and the host. Requires, besides the `compat` probes, Chromium's sandbox checks, the X server and the [CEF smoke](tools/desktop/cef-smoke.mjs) with the sandbox active, in `host` and `isolation` modes. The same smokes run first on the builder's Linux, with the sandbox. Uses 12 GB and 4 vCPUs in QEMU. |
| `desktop` | Adds the given OpenShell Desktop bundle to the initramfs and runs the [hostd smoke](tools/desktop/hostd-smoke.mjs) as an unprivileged user, first on the builder's Linux and then in the guest, plus the `compat` probes. |
| `kiosk` | Combines `graphics` and `desktop` and opens the given bundle in the shell. The [shell smoke](tools/desktop/shell-smoke.mjs) drives the real Desktop with X11 input: Files, Terminal, the native Browser's shortcuts and focus, zoom, download, DevTools, file selection, PDF, screenshots with the tabs, clipboard history, renderer and hostd recovery, shutdown and a second session. The [session smoke](tools/desktop/session-smoke.mjs) runs the `openshell-session` supervisor as root. Both run first on the builder's Linux. |
| `apps` | `kiosk` with the Linux apps profile: Xpra, its HTML5 client, D-Bus and one GTK and one Qt test app. The [apps smoke](tools/desktop/apps-smoke.mjs) opens the apps through `hostd`'s API and requires shutdown without orphans. The shell smoke adds the `linux-apps` phase: the apps open from the search in OpenShell windows, receive the keyboard and the window's size, show dialogs, exchange text with the clipboard history, open documents from the Terminal and Files and close. Uses 12 GB and 4 vCPUs in QEMU, with up to 120 minutes of boot. |
| `wine` | `apps` with the Windows apps profile ([plan](OPENSHELL_WINE.md)): Wine 11.0, 64-bit with WoW64, and an entry that opens `.exe` and `.msi` in the foreground. The [Wine smoke](tools/desktop/wine-smoke.mjs) creates an isolated prefix with `wineboot`, opens and closes Notepad on X11 and requires that nothing from the prefix is left behind. The shell smoke opens a `.exe` from the Terminal, chooses Wine and requires the program's window on the Desktop and Wine's end when it is closed. Uses 12 GB and 4 vCPUs in QEMU, with up to 150 minutes of boot. |

Each build or test run saves its artifacts in `out/native-*/`. `run.json` records the result, including Docker connection failures, the builder's digest and the `checks.tsv` results when available. When there is a kernel build, `toolchain.txt` records the firmware and the QEMU profile, and `SHA256SUMS` covers the diagnostic ISO. See `native-compat-console.txt` to diagnose ABI or sandbox failures in the guest. `native-seccomp` and `native-namespaces` stay `unverified` when the console does not prove that the probe actually ran.

If the Node probe fails, the console records the events received by the watcher on timeout and the results of a direct `inotify` and `epoll` check in `/ext2` and `/tmp`.

The source, the commit, the Rust version and the official image's digest are in [upstream.lock.json](kernel/config/upstream.lock.json). To test another builder digest, set `OPENSHELL_NATIVE_BUILDER_DIGEST`. `run.json` records the image actually used.

The build applies the [patch series](kernel/patches/README.md) on top of the pinned revision. The OSDK build fix uses that revision's lockfiles when building the OSDK and generating the kernel crate, which keeps nightly from selecting `unwinding` 0.2.11. The `kernel-Cargo.lock` saved in the report shows the resolution used. The other patches add seccomp filters, inotify's `rename` events and user, PID and network namespaces, required by Chromium's sandbox and by hostd, and fix what Chromium and hostd ran into along the way: ephemeral ports and PTYs for regular users, local PIDs in `wait4`, thread counts and namespace files in procfs, Unix sockets in `SCM_RIGHTS`, `MSG_DONTWAIT`, process titles and `bind` for a server restarted on the same port.

### Compatibility status

Results in the baseline's QEMU profile, with kernel v0.19.0 and the patch series. The control column shows the same probe on Linux.

| Workload | Probe | Control Linux | Native |
| --- | --- | --- | --- |
| Upstream boot, PTY, static ELF and VirtIO ext2 I/O | [boot.sh](userspace/compatibility/probes/boot.sh) | n/a | Passes |
| Python 3.13: processes, terminal, mapped memory, `SCM_RIGHTS`, VFS, UDP, clock and entropy | [python.py](userspace/compatibility/probes/python.py) | Passes | Passes |
| TCP server restarted on the same port while the previous connections close | `python.py`, `server_restart` probe | Passes | Passes with patch 0015 |
| A/B transaction on the VirtIO disk: confirmed update, 3-boot trial without confirmation and rollback, tampered release, incompatible schema and destroyed selector | [smoke.sh](distro/ab/smoke.sh) with `openshell-ab` | Passes | Passes in `/ext2` |
| Node 24: subprocesses, Worker, `fs.watch` with `rename`, loopback HTTP, DNS and TLS 1.3 against local fixtures | [node.mjs](userspace/compatibility/probes/node.mjs) | Passes | Passes with patch 0003 |
| Seccomp filters: `ERRNO`, `TRAP` with `SIGSYS`, `KILL`, strict mode, `TSYNC`, inheritance across `fork` and `execve` | [seccomp.c](userspace/compatibility/probes/seccomp.c) | Passes | Passes with patch 0002 |
| Unprivileged user, PID and network namespaces, empty root and dropped capabilities, with negative isolation cases | [namespaces.py](userspace/compatibility/probes/namespaces.py) | Passes | Passes with patches 0004 to 0006 |
| OpenShell Desktop 2.1.0's hostd as a regular user: bootstrap and cookie, UI, WebSocket authorisation, RPC, `fs.watch` with rename, a real PTY with resize and output, `exec.run` with a deadline | [hostd-smoke.mjs](tools/desktop/hostd-smoke.mjs) | Passes | Passes with patches 0007 and 0008 |
| What Chromium checks before and during the sandbox: `/proc/self/task` links, namespace files, nested user namespace, zygote in namespaces with `exec` of `/proc/self/exe`, `chroot` helper with `CLONE_VM \| CLONE_VFORK \| CLONE_SETTLS`, sockets in `SCM_RIGHTS`, netlink dump with `MSG_DONTWAIT` and process title | [chromium-userns.c](userspace/compatibility/probes/chromium-userns.c) | Passes | Passes with patches 0009 to 0014 |
| CEF 147 on X11 (Xvfb and Openbox): window, mouse, keyboard, Web Worker, WebGL, capture and renderers with seccomp applied by Chromium in their own namespaces | [cef-smoke.mjs](tools/desktop/cef-smoke.mjs) `host` mode | Passes with the sandbox[^control] | Passes with the sandbox |
| Two CEF contexts side by side: separate cookies and blocked direct, redirected and Worker requests, before and after resizing | `cef-smoke.mjs` `isolation` mode | Passes with the sandbox | Passes with the sandbox |
| The real OpenShell Desktop in the shell: Files, Terminal, Browser with a native tab, renderer and hostd recovery, a second session and the secret kept out of the logs | [shell-smoke.mjs](tools/desktop/shell-smoke.mjs) | Passes with the sandbox | Passes with the sandbox and patches 0001 to 0015 |
| Supervised session: X, Openbox and the shell as a regular user, shell restart with no remaining processes and orderly shutdown | [session-smoke.mjs](tools/desktop/session-smoke.mjs) | Passes | Passes |

[^control]: The builder of the `graphics`, `kiosk` and `apps` actions runs without Docker's seccomp profile, which blocks user namespaces. In CI, the runner must allow unprivileged user namespaces.

Both layers of Chromium's sandbox, seccomp and namespaces, are proven in the probes and in the real Chromium process: CEF's renderers run in the guest with `Seccomp: 2`, `NoNewPrivs: 1` and a filter installed by Chromium itself. Rendering is in software: the image includes Mesa in `/run/opengl-driver`, Xvfb offers GLX, and ANGLE uses llvmpipe. The smokes record the effective renderer, `ANGLE (Mesa, llvmpipe ...)`, and check a WebGL pixel read back. The host passes `ignore-gpu-blocklist`, because Chromium's blocklist refuses WebGL on llvmpipe. Xorg on `/dev/dri/card0` is the next graphics step.

The builder restricts the creation of user namespaces, so the namespace probe is not part of `control`. To run it on Linux:

```bash
docker run --rm --security-opt seccomp=unconfined --security-opt apparmor=unconfined -v "$PWD/userspace/compatibility/probes:/probes:ro" python:3.13-slim python -I /probes/namespaces.py
```

In the guest, with the Desktop ready, CEF takes about 1.1 GB and `hostd` about 100 MB, adding up the RSS of their processes, which counts shared pages more than once. On the control Linux, CEF's PSS is about 640 MB. The guest does not offer `/proc/[pid]/smaps_rollup`, and the CPU times in `/proc/[pid]/stat` do not follow Linux's unit, so those two values of the guest's report are not fit for comparison.

CI runs the probes on Linux and accepts a mode selection in the **Native baseline** workflow. To compare with OpenShell Live, record the amd64 ISO and its SHA-256 with the results.

## CEF shell

The Rust launcher starts `hostd` offline and opens the Desktop bundle in a CEF window. It restarts `hostd` on the same port when it fails and hands the new bootstrap to the CEF host through stdin, which stays open during the session. The shared Rust library validates the launcher's configuration and messages and decides which origins the Desktop and the Browser's tabs may navigate to and load. The C++ adapter calls the library and implements the CEF SDK's window and lifecycle callbacks. It also answers the Desktop page's `/_shell/` commands, which open and position the Browser's tabs as overlaid views, and offers the features Electron provides on OpenShell Live: downloads, permission and file prompts, certificate exceptions, the DevTools protocol, PDF, captures and clipboard. [Section 10 of the CEF plan](ARCHITECTURE-CEF.md#10-implementation-status) describes the bridge, the contexts, the known limits and the `openshell-session` supervisor.

On x86-64 Linux, use an already extracted CEF **Standard Distribution** SDK, CMake, a C++ compiler compatible with the SDK and Rust. To run the tests, start X11 with a window manager, use a regular user and make `xdotool` available. Chromium's sandbox must be available in the kernel and the session.

```bash
bash tools/desktop/cef-baseline.sh /path/to/cef-sdk /opt/openshell
```

The suite builds `cefsimple` and the Native host with the same SDK and runs the window, input, Worker, isolation and sandbox tests. It also runs a separate smoke with the real bundle's `hostd`. `out/cef-baseline-*/` contains the kernel and CEF identification, the logs and results per test and the X11 captures when `xwd` is available. Run the suite on a control Linux and repeat it in the Asterinas guest with the same CEF distribution.

To run only the bundle check with Node.js 22+ on a host with loopback available:

```text
node tools/desktop/hostd-smoke.mjs /opt/openshell
```

The smoke uses a disposable profile and tests exchanging the token for a cookie, loading the HTML, the WebSocket authentication and the `system.info` and `fs.readdir` RPCs. Then it exercises the services Files, Memory, Terminal and the tools depend on: `fs.watch` with an atomic rename, a real PTY with `stty`, resize and exit status, and `exec.run` with output, status and a deadline. It records the build and the hashes of `hostd` and the HTML, without including the session token, and writes hostd's stderr to `hostd.log`.

## A/B updates

[`distro/ab`](distro/ab) implements the transaction of section 6 of the [plan](ARCHITECTURE.md). The [experimental image](distro/image) uses an ESP with GRUB and four ext2 partitions, `system_a`, `system_b`, `recovery` and `data`.

- **Selector.** Records each slot's state (`empty`, `staged`, `trial:N`, `good`, `bad`) and the confirmed slot in two copies, with a generation and a CRC-32. Each change writes the older copy, so an interrupted write leaves the newer one intact.
- **Signature.** The manifest comes with `<manifest>.sig`: an Ed25519 signature, in base64, of the manifest's exact bytes, the way the OpenShell Desktop updater signs and checks its releases. `stage` takes the folder of trusted keys (PEM) and checks the signature before reading anything from the manifest. `openshell-ab keygen` and `openshell-ab sign` serve the release tools and the tests.
- **Update.** Makes the inactive slot unbootable before writing to it. Then it checks each file's size and SHA-256 against the release manifest, along with the architecture and the data schema range, and only then marks it as `staged`.
- **Update from the guest.** `openshell-ab <data>/openshell/ab update-mounted <source> <manifest> <schema> <keys> <mounted-slot>` validates the signed release and schedules the next boot on trial. The command requires the running slot to match the confirmed release and the target to be the inactive ext2 partition on the same GPT disk as the `data` partition. GRUB identifies the image's partitions by UUID. Use the installation's directory of trusted public keys.
- **Boot.** GRUB reads `grubenv` on the `data` partition and writes the countdown before handing control to a slot on trial. After three attempts without confirmation, it goes back to the confirmed slot. The boot service syncs that result with the selector. The image also proves booting recovery with the environment missing or without a usable `boot.cfg` in the slots.
- **Confirmation.** The installed session keeps its profile in `data`. The renderer sends periodic reports after the Desktop mounts its interface and completes an authenticated read and write through hostd. The supervisor requires 60 seconds of continuous reports by default and restarts the count when the reports stop or the session restarts. The confirmation command checks that the running release's identifier matches the one recorded in the slot.

### Release channel

The installed system updates through Settings ▸ Updates, like Live, but the new version is the whole system. The live ISO build also produces the channel in `out/native-*/native-channel/` ([channel.sh](distro/image/channel.sh)): `latest.json` with `.sig`, in the Desktop's format (`product: "openshell-native"`), and `openshell-native-<version>-amd64.tar`, with the signed kernel, the initramfs, `boot.cfg` and `openshell-ab`'s `MANIFEST` with its signature. Publish the folder at `<portal>/downloads/native/`. An OpenShell Native Ed25519 key signs both manifests. It is created by the first build, lives in `.secureboot/` with the Secure Boot key, and the image carries the public part in `/opt/openshell-media/release-keys`. Without it, installed systems do not accept new versions: keep a copy.

1. hostd checks the signature of `latest.json` and downloads the archive, with resume and SHA-256, to the system's inbox (`/data/openshell/updates/inbox`, owned by the session user).
2. **Install** asks the supervisor for `update-stage <file>` ([update.rs](userspace/shell/src/update.rs)). It only records the name, in a root-owned file.
3. On restart or shutdown from the Desktop, and on the next boot if the computer stopped before, init ([installed-session.sh](userspace/compatibility/probes/installed-session.sh)) unpacks the archive, mounts the inactive slot and runs `openshell-ab update-mounted` with the image's keys. That checks the `MANIFEST`'s signature and each file, writes the slot and schedules the boot on trial. The result appears in Updates (`update-status`).
4. GRUB starts the new version on trial, and the session confirms it after 60 s of good health. If it fails to start three times, GRUB goes back to the previous one.

The live stick does not update itself: the Desktop shows the link to the new ISO.

### Updating from the USB stick

As on Live, a new stick updates an already installed system without erasing anything: **Update installed OpenShell Native (keep files)** in its menu opens the update screen. It lists the installations on the disks (the installed version and the previous one), shows the stick's version and warns when it is the same or older, and updates or puts back the version from before the last update. The supervisor runs the [updater](distro/image/update.sh) as root:

1. It checks the stick's release: the signed `MANIFEST` comes on the media kernel's command line (`openshell.manifest`, because the manifest covers the initramfs itself), and each file read from memory (kernel, initramfs and `boot.cfg`) is what the manifest says ([medium.sh](distro/image/medium.sh)).
2. It mounts the installation's data partition and the slot that is not in use, and runs `openshell-ab update-offline`, which checks the signature with the media's keys, writes the slot, checks what it wrote and schedules the boot on trial.
3. On the next start, without the stick, GRUB starts the new version on trial. The session confirms it; three unsuccessful starts go back to the previous one. Data, the account, settings and applications stay as they were.

**Put back the previous release** uses `openshell-ab restore`, which puts on trial the version the inactive slot still holds. The [update test](distro/image/update-test.sh) runs the whole path in QEMU: installs from the stick, updates from the stick and boots the disk by itself on slot B.

The unit tests simulate a power cut at every write of a whole update. In every case, the next boot chooses a `good` slot or one on trial, never an incomplete slot. They also check the signature against test vector 1 of RFC 8032. The [smoke](distro/ab/smoke.sh) repeats the flow with the CLI on a directory: it refuses a manifest altered after signing, one signed by an untrusted key and one without a signature. It runs on Linux, in CI and in the guest, in `/ext2`. The [session smoke](tools/desktop/session-smoke.mjs) requires the boot confirmation, exactly once.

```bash
cargo test --manifest-path distro/ab/Cargo.toml
cargo build --release --manifest-path distro/ab/Cargo.toml
sh distro/ab/smoke.sh distro/ab/target/release/openshell-ab
```

The `update` scenario starts with only slot A installed and makes a signed release available in `data`. Asterinas mounts slot B, rejects updating the active slot and an unsigned release, writes the signed release and restarts on slot B. The `desktop-image` action uses a bundle with support for the health report. The installer for hardware is still pending.

## Integration with OpenShell

Native consumes the Desktop's Linux bundle built in the `openshell` repository by `pnpm desktop:bundle`, in `apps/desktop/dist-bundle/openshell-desktop`. The bundle must contain `BUILD.json`, `hostd/openshell-hostd.mjs` and `ui/index.html`. The `run.json` of the `desktop`, `kiosk` and `apps` actions records the version, the build date, the Electron version and the SHA-256 of hostd and of `ui/index.html`.

```bash
node tools/build/native.mjs desktop ../openshell/apps/desktop/dist-bundle/openshell-desktop
```

hostd uses the Node and Python of the same nixpkgs as the guest. The bundle's `node-pty` is optional, and hostd uses the `python-pty` backend when it is missing. In the `desktop` action, the UI is served and checked over HTTP, without a renderer. The `kiosk` action opens the same UI in the shell. The Browser uses native tabs when the bundle includes the `apps/desktop/src/host/nativeBrowser.tsx` adapter from the `openshell` repository. To validate the grouped layout and the real release of shortcuts, build the bundle with the version of the adapter that negotiates `surfaceLayout` and `shortcutPhases`.

```bash
node tools/build/native.mjs kiosk ../openshell/apps/desktop/dist-bundle/openshell-desktop
```

To build and open the real UI in the launcher:

```bash
bash tools/desktop/build-cef.sh /path/to/cef-sdk
out/cef-build/rust/release/openshell-shell /opt/openshell \
  out/cef-build/host/runtime/openshell-cef-host "$HOME/.local/state/openshell-native"
```

The bundle in `/opt/openshell` must contain `hostd/openshell-hostd.mjs` and `ui/index.html`. The launcher uses the Node included in the bundle or the Node available on the PATH. It uses local port `7788` by default. Set `OPENSHELL_NATIVE_PORT` before a profile's first boot if you need another port.

`node tools/desktop/cef-smoke.mjs <sample|host|isolation> <cef-executable>` runs a single graphics proof. The results are in `out/cef-*/cef-smoke.json`.

The `isolation` mode opens two views in the same X11 window, with independent contexts and two local test servers. The test requires input in both views before and after resizing the window. It also checks cookies and the blocking of direct, redirected and Worker-initiated requests. The proof uses side-by-side views.

`node tools/desktop/shell-smoke.mjs <openshell-shell> <desktop-bundle> <cef-host>` drives the real Desktop in a regular user's X11 session. `node tools/desktop/session-smoke.mjs <openshell-session> <openshell-shell> <desktop-bundle> <cef-host> <Xvfb> <openbox>`, run as root, starts the session through the supervisor.


## Reference Desktop with Electron on X11

In a Linux guest with Xorg already running, run it as a regular user. Pass the OpenShell Desktop bundle's folder and the Linux Electron executable that matches the bundle's `BUILD.json`.

```text
node tools/desktop/smoke.mjs /opt/openshell /opt/openshell/electron/electron
```

The test uses a disposable profile, an offline hostd and a local page with a Web Worker in the Browser. The report requires the Desktop's preload, the hostd bridge, the worker's result and evidence of seccomp filters in the renderer processes. To diagnose failures, see `native-desktop.json`, Electron's report and the log in `out/desktop-*/`.

The pinned Asterinas revision leaves the `seccomp` syscall out of the x86-64 table, and patch 0002 adds it along with the `/proc/[pid]/status` fields this test reads. The `sandbox` test requires the guest to install and apply a filter and to enter new namespaces. The result of that proof gates the kiosk integration. [Syscall table of the pinned revision](https://github.com/asterinas/asterinas/blob/c4b7ed575b7b32773475b194d2ad41fb6d03765b/kernel/core/src/syscall/arch/x86.rs)

## License

OpenShell Native is free and open-source software, licensed under the [Mozilla Public License 2.0](LICENSE), the same licence as [Asterinas](https://github.com/asterinas/asterinas), on which its kernel is based. The files the kernel patches add declare their licence in an SPDX header: MPL-2.0 for most of them, and BSD-3-Clause, BSD-3-Clause-Clear or ISC for the Wi-Fi tables and code derived from the corresponding permissively licensed Linux drivers, with a `NOTICE` file in each of those driver crates. Third-party components keep their own licences: Debian's shim and GRUB, the linux-firmware files (whose licence texts the image ships beside them) and the nixpkgs packages that make up the system.
