//! Sound cards that come and go (a USB headset plugged in, taken out), as
//! PulseAudio follows them on OpenShell Live through udev: the session's
//! sound server loads each card the kernel has (`/dev/snd/controlC<N>`)
//! with `module-alsa-card` and unloads the ones that went. The kernel says
//! when a card comes or goes (a `sound` uevent, NETLINK_KOBJECT_UEVENT);
//! each one, and the start, compares what there is with what PulseAudio
//! has (`pactl list short modules`), so a missed event costs nothing.
//!
//! Card 0 may be loaded by the sound server's own script already (with its
//! own arguments); it counts as loaded like any other.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Read;
use std::os::fd::FromRawFd;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

/// The kernel's ALSA control devices, by card number.
pub const SOUND_DEVICES: &str = "/dev/snd";
/// The module that opens a card, and the argument that names it.
pub const CARD_MODULE: &str = "module-alsa-card";
const CARD_ARGUMENT: &str = "device_id=";

/// The cards present: the numbers of `controlC<N>` in the device folder.
pub fn present_cards(folder: &Path) -> Vec<u32> {
    let mut cards: Vec<u32> = fs::read_dir(folder)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().to_str()?.strip_prefix("controlC")?.parse().ok())
                .collect()
        })
        .unwrap_or_default();
    cards.sort_unstable();
    cards.dedup();
    cards
}

/// The cards PulseAudio has: (module index, card) of each `module-alsa-card`
/// line of `pactl list short modules` (index, name, arguments, by tabs).
pub fn loaded_cards(listing: &str) -> Vec<(u32, u32)> {
    listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let index = fields.next()?.trim().parse().ok()?;
            if fields.next()?.trim() != CARD_MODULE {
                return None;
            }
            let card = fields
                .next()?
                .split_whitespace()
                .find_map(|argument| argument.strip_prefix(CARD_ARGUMENT))?
                .trim_matches('"')
                .parse()
                .ok()?;
            Some((index, card))
        })
        .collect()
}

/// What to do: the cards to load, and the modules to unload (their cards
/// went, or a card is loaded twice).
pub fn plan(present: &[u32], loaded: &[(u32, u32)]) -> (Vec<u32>, Vec<u32>) {
    let load = present.iter().copied().filter(|card| !loaded.iter().any(|(_, other)| other == card)).collect();
    let mut seen = Vec::new();
    let mut unload = Vec::new();
    for &(module, card) in loaded {
        if !present.contains(&card) || seen.contains(&card) {
            unload.push(module);
        } else {
            seen.push(card);
        }
    }
    (load, unload)
}

/// Whether a uevent (`ACTION@DEVPATH` then `KEY=VALUE`, NUL-separated) says
/// a sound card's control device came or went.
pub fn is_card_event(message: &[u8]) -> bool {
    let mut fields = message.split(|byte| *byte == 0).filter_map(|field| std::str::from_utf8(field).ok());
    let mut action = false;
    let mut sound = false;
    let mut control = false;
    for field in &mut fields {
        match field.split_once('=') {
            Some(("ACTION", value)) => action = value == "add" || value == "remove",
            Some(("SUBSYSTEM", value)) => sound = value == "sound",
            Some(("DEVNAME", value)) => control = value.trim_start_matches("/dev/").starts_with("snd/controlC"),
            _ => {}
        }
    }
    action && sound && control
}

/// The arguments a hot-plugged card is loaded with: PulseAudio chooses its
/// scheduling (a USB card's packets make it a "batch" device).
pub fn load_arguments(card: u32) -> Vec<String> {
    vec!["load-module".into(), CARD_MODULE.into(), format!("{CARD_ARGUMENT}{card}")]
}

extern "C" {
    fn socket(domain: i32, kind: i32, protocol: i32) -> i32;
    fn bind(fd: i32, address: *const NetlinkAddress, length: u32) -> i32;
}

const AF_NETLINK: i32 = 16;
const SOCK_DGRAM: i32 = 2;
const SOCK_CLOEXEC: i32 = 0o2_000_000;
const NETLINK_KOBJECT_UEVENT: i32 = 15;
/// The kernel's own uevents (udev's are group 2).
const KERNEL_EVENTS: u32 = 1;

#[repr(C)]
struct NetlinkAddress {
    family: u16,
    pad: u16,
    pid: u32,
    groups: u32,
}

/// A socket that hears the kernel's uevents, as udev does.
fn uevents() -> Option<File> {
    let fd = unsafe { socket(AF_NETLINK, SOCK_DGRAM | SOCK_CLOEXEC, NETLINK_KOBJECT_UEVENT) };
    if fd < 0 {
        return None;
    }
    // Owned at once: closed on every way out.
    let file = unsafe { File::from_raw_fd(fd) };
    let address = NetlinkAddress { family: AF_NETLINK as u16, pad: 0, pid: 0, groups: KERNEL_EVENTS };
    if unsafe { bind(fd, &address, std::mem::size_of::<NetlinkAddress>() as u32) } != 0 {
        return None;
    }
    Some(file)
}

/// `pactl` on the session's server, its output when it succeeds.
fn pactl(program: &OsStr, server: &OsStr, arguments: &[String]) -> Option<String> {
    let output = Command::new(program)
        .args(arguments)
        .env_clear()
        .env("PULSE_SERVER", server)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Makes the sound server have the cards the kernel has.
pub fn sync_cards(program: &OsStr, server: &OsStr, devices: &Path) {
    let Some(listing) = pactl(program, server, &["list".into(), "short".into(), "modules".into()]) else { return };
    let (load, unload) = plan(&present_cards(devices), &loaded_cards(&listing));
    for module in unload {
        let _ = pactl(program, server, &["unload-module".into(), module.to_string()]);
    }
    for card in load {
        let _ = pactl(program, server, &load_arguments(card));
    }
}

/// Follows the cards until `stop`: at the start, at each card the kernel
/// adds or removes and, without uevents, every few seconds.
pub fn follow_cards(program: &OsStr, server: &OsStr, stop: &AtomicBool) {
    let devices = Path::new(SOUND_DEVICES);
    let mut events = uevents();
    let mut buffer = vec![0u8; 8192];
    while !stop.load(Ordering::Relaxed) {
        sync_cards(program, server, devices);
        match events.as_mut() {
            Some(socket) => loop {
                match socket.read(&mut buffer) {
                    Ok(length) if is_card_event(&buffer[..length]) => break,
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => {
                        events = None;
                        break;
                    }
                }
            },
            None => thread::sleep(Duration::from_secs(3)),
        }
        // A card's devices come together; one look covers them.
        thread::sleep(Duration::from_millis(200));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_pulseaudio_has() {
        let listing = "0\tmodule-device-restore\t\t\n6\tmodule-alsa-card\tdevice_id=0 tsched=1\t\n9\tmodule-alsa-card\tdevice_id=\"2\"\t\n11\tmodule-null-sink\t\t\n";
        assert_eq!(loaded_cards(listing), vec![(6, 0), (9, 2)]);
    }

    #[test]
    fn loads_new_cards_and_unloads_gone_ones() {
        assert_eq!(plan(&[0, 1], &[(6, 0)]), (vec![1], vec![]));
        assert_eq!(plan(&[0], &[(6, 0), (9, 1)]), (vec![], vec![9]));
        assert_eq!(plan(&[1], &[(6, 1), (7, 1)]), (vec![], vec![7]));
        assert_eq!(plan(&[], &[]), (vec![], vec![]));
    }

    #[test]
    fn hears_card_events_only() {
        let add = b"add@/devices/virtual/sound/card1/controlC1\0ACTION=add\0DEVPATH=/devices/virtual/sound/card1/controlC1\0SUBSYSTEM=sound\0DEVNAME=snd/controlC1\0SEQNUM=7\0";
        assert!(is_card_event(add));
        let usb = b"add@/devices/usb1/1-2\0ACTION=add\0SUBSYSTEM=usb\0DEVNAME=bus/usb/001/003\0";
        assert!(!is_card_event(usb));
        let pcm = b"add@/x\0ACTION=add\0SUBSYSTEM=sound\0DEVNAME=snd/pcmC1D0p\0";
        assert!(!is_card_event(pcm));
    }

    #[test]
    fn finds_the_present_cards() {
        let folder = std::env::temp_dir().join(format!("openshell-snd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        for name in ["controlC0", "controlC3", "pcmC0D0p", "timer"] {
            fs::write(folder.join(name), b"").unwrap();
        }
        assert_eq!(present_cards(&folder), vec![0, 3]);
        assert_eq!(present_cards(&folder.join("none")), Vec::<u32>::new());
        fs::remove_dir_all(&folder).unwrap();
        assert_eq!(load_arguments(3), vec!["load-module", "module-alsa-card", "device_id=3"]);
    }
}
