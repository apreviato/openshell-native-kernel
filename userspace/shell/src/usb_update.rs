//! Updating an installed OpenShell Native from the live medium: the boot
//! menu's "Update installed OpenShell Native (keep files)", as on OpenShell
//! Live. The updater (distro/image/update.sh) writes a disk, so it runs as
//! root: the supervisor starts it when the setup screen asks on the system
//! socket (install.rs), a line each way:
//!
//! ```text
//! usb-update-list              → "systems <device>=<bytes>,<release>,<previous> …" (none: "systems";
//!                                <previous> is "-" when nothing ran before)
//! usb-update <device>          → "started", "busy" or "invalid"
//! usb-update-restore <device>  → the same, putting the previous release back
//! usb-update-status            → "idle", "running <percent> <message>", "done" or "failed <message>"
//! ```
//!
//! Without an updater (any session but the medium's update screen) these
//! answer "unavailable".

use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use crate::install::{follow, valid_device, InstallState};
use crate::session::CommandLine;

/// What the updater's requests start with.
pub const USB_UPDATE_PREFIX: &str = "usb-update";

pub fn is_request(line: &str) -> bool {
    line.split(' ').next().is_some_and(|verb| verb == USB_UPDATE_PREFIX || verb.starts_with("usb-update-"))
}

/// A release's name, as openshell-ab keeps them.
fn valid_release(release: &str) -> bool {
    release == "-"
        || (!release.is_empty()
            && release.len() <= 64
            && release.bytes().all(|byte| byte.is_ascii_alphanumeric() || b".-_+".contains(&byte)))
}

/// The systems line from `update.sh list` ("<device> <bytes> <release> <previous>" per line).
pub fn systems_line(listing: &str) -> String {
    let systems: Vec<String> = listing
        .lines()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            let [device, bytes, release, previous] = words[..] else { return None };
            (valid_device(device) && bytes.parse::<u64>().is_ok() && valid_release(release) && release != "-" && valid_release(previous))
                .then(|| format!("{device}={bytes},{release},{previous}"))
        })
        .collect();
    if systems.is_empty() {
        "systems".into()
    } else {
        format!("systems {}", systems.join(" "))
    }
}

pub struct UsbUpdater {
    command: CommandLine,
    state: Arc<Mutex<InstallState>>,
}

impl UsbUpdater {
    pub fn new(command: CommandLine) -> Self {
        Self { command, state: Arc::new(Mutex::new(InstallState::Idle)) }
    }

    fn run(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.command[0]);
        command
            .args(&self.command[1..])
            .args(args)
            .env_clear()
            .env("PATH", "/opt/openshell-compat/bin:/bin:/usr/bin")
            .stdin(Stdio::null());
        command
    }

    /// The reply to one request line (`is_request`).
    pub fn answer(&self, line: &str) -> String {
        let state = self.state.lock().unwrap().clone();
        match line.split_once(' ').unwrap_or((line, "")) {
            ("usb-update-list", "") => match self.run(&["list"]).stderr(Stdio::null()).output() {
                Ok(output) => systems_line(&String::from_utf8_lossy(&output.stdout)),
                Err(_) => "systems".into(),
            },
            ("usb-update-status", "") => state.line(),
            (verb @ ("usb-update" | "usb-update-restore"), device) if valid_device(device) => {
                if matches!(state, InstallState::Running { .. }) {
                    return "busy".into();
                }
                let action = if verb == "usb-update" { "update" } else { "restore" };
                follow(self.run(&[action, device]), &self.state, "the updater")
            }
            _ => "invalid".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn lists_installed_systems() {
        let listing = "/dev/nvme0n1 512110190592 2026.11.0 -\n/dev/sda 64000000000 2.2.0 2.1.0\n/dev/sda1 10 x -\n/dev/vdb 10 - -\njunk\n";
        assert_eq!(systems_line(listing), "systems /dev/nvme0n1=512110190592,2026.11.0,- /dev/sda=64000000000,2.2.0,2.1.0");
        assert_eq!(systems_line(""), "systems");
        assert!(is_request("usb-update /dev/vda") && is_request("usb-update-status") && !is_request("update-status"));
    }

    #[test]
    fn runs_the_updater_and_reports_its_end() {
        let folder = std::env::temp_dir().join(format!("openshell-usb-update-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let script = folder.join("update.sh");
        std::fs::write(
            &script,
            "case $1 in list) echo '/dev/vdb 20000000000 2.2.0 2.1.0';; update) echo 'PROGRESS 50 Writing'; echo DONE;; restore) echo 'FAILED no previous release' >&2; exit 1;; esac\n",
        )
        .unwrap();
        let updater = UsbUpdater::new(vec!["/bin/sh".into(), script.to_string_lossy().into_owned()]);
        assert_eq!(updater.answer("usb-update-list"), "systems /dev/vdb=20000000000,2.2.0,2.1.0");
        assert_eq!(updater.answer("usb-update /dev/vdb1"), "invalid");
        let wait_end = |updater: &UsbUpdater| {
            for _ in 0..100 {
                let status = updater.answer("usb-update-status");
                if !status.starts_with("running") {
                    return status;
                }
                thread::sleep(Duration::from_millis(20));
            }
            updater.answer("usb-update-status")
        };
        assert_eq!(updater.answer("usb-update /dev/vdb"), "started");
        assert_eq!(wait_end(&updater), "done");
        assert_eq!(updater.answer("usb-update-restore /dev/vdb"), "started");
        assert_eq!(wait_end(&updater), "failed no previous release");
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
