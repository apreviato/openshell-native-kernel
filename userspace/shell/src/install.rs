//! Installing the live system on a disk, from the live session.
//!
//! The installer (distro/image/install.sh) writes a whole disk, so it runs
//! as root: the supervisor starts it when the session user asks on the
//! supervisor's system socket (OPENSHELL_SYSTEM_SOCKET), one request per
//! connection, a line each way:
//!
//! ```text
//! install-list       → "disks <device>=<bytes> …" (none: "disks")
//! install <device>   → "started", "busy" or "invalid"
//! install-status     → "idle", "running <percent> <message>", "done" or "failed <message>"
//! ```
//!
//! The same socket checks the lock screen's password (login.rs, Phase::Unlock):
//! `state` → "unlock" and `login <password>` → "ok" or "wrong", or
//! "unavailable" without a sign-in, and `power` (power.rs). The socket is
//! always there; without an installer, the install requests answer
//! "unavailable".

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::session::CommandLine;

/// The environment variable that names the supervisor's system socket to the session.
pub const SYSTEM_SOCKET_ENV: &str = "OPENSHELL_SYSTEM_SOCKET";
/// What the installer's requests start with; without an installer (an
/// installed system) the supervisor answers them "unavailable".
pub const INSTALL_PREFIX: &str = "install";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallState {
    Idle,
    Running { percent: u8, message: String },
    Done,
    Failed(String),
}

impl InstallState {
    pub fn line(&self) -> String {
        match self {
            InstallState::Idle => "idle".into(),
            InstallState::Running { percent, message } => format!("running {percent} {message}"),
            InstallState::Done => "done".into(),
            InstallState::Failed(message) => format!("failed {message}"),
        }
    }

    /// The state after one line of the installer's output.
    pub fn after(self, line: &str) -> Self {
        if let Some(rest) = line.strip_prefix("PROGRESS ") {
            let (percent, message) = rest.split_once(' ').unwrap_or((rest, ""));
            if let Ok(percent) = percent.parse::<u8>() {
                return InstallState::Running { percent: percent.min(100), message: message.to_owned() };
            }
        }
        if line == "DONE" {
            return InstallState::Done;
        }
        self
    }
}

/// A device the installer may be asked to write: /dev/nvme…n… or /dev/vd…, nothing else.
pub fn valid_device(device: &str) -> bool {
    let Some(name) = device.strip_prefix("/dev/") else { return false };
    let nvme = name.strip_prefix("nvme").is_some_and(|rest| {
        let (controller, namespace) = rest.split_once('n').unwrap_or(("", ""));
        !controller.is_empty()
            && controller.bytes().all(|byte| byte.is_ascii_digit())
            && !namespace.is_empty()
            && namespace.bytes().all(|byte| byte.is_ascii_digit())
    });
    let virtio = name.len() == 3 && name.starts_with("vd") && name.as_bytes()[2].is_ascii_lowercase();
    nvme || virtio
}

/// The disks line from `installer list` ("<device> <bytes>" per line).
pub fn disks_line(listing: &str) -> String {
    let disks: Vec<String> = listing
        .lines()
        .filter_map(|line| {
            let (device, bytes) = line.split_once(' ')?;
            (valid_device(device) && bytes.trim().parse::<u64>().is_ok()).then(|| format!("{device}={}", bytes.trim()))
        })
        .collect();
    if disks.is_empty() {
        "disks".into()
    } else {
        format!("disks {}", disks.join(" "))
    }
}

pub struct Installer {
    command: CommandLine,
    state: Arc<Mutex<InstallState>>,
}

impl Installer {
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

    /// The reply to one request line.
    pub fn answer(&self, line: &str) -> String {
        let state = self.state.lock().unwrap().clone();
        match line.split_once(' ').unwrap_or((line, "")) {
            ("install-list", "") => match self.run(&["list"]).stderr(Stdio::null()).output() {
                Ok(output) => disks_line(&String::from_utf8_lossy(&output.stdout)),
                Err(_) => "disks".into(),
            },
            ("install-status", "") => state.line(),
            ("install", device) if valid_device(device) => {
                if matches!(state, InstallState::Running { .. }) {
                    return "busy".into();
                }
                let child = self.run(&["install", device]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
                let Ok(mut child) = child else { return "failed the installer did not start".into() };
                *self.state.lock().unwrap() = InstallState::Running { percent: 0, message: "Starting".into() };
                let (stdout, stderr) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
                let shared = self.state.clone();
                thread::spawn(move || {
                    let errors = thread::spawn(move || {
                        BufReader::new(stderr).lines().map_while(Result::ok).last().unwrap_or_default()
                    });
                    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                        let mut state = shared.lock().unwrap();
                        *state = state.clone().after(&line);
                    }
                    let status = child.wait();
                    let last_error = errors.join().unwrap_or_default();
                    let mut state = shared.lock().unwrap();
                    if !matches!(status, Ok(status) if status.success()) || *state != InstallState::Done {
                        let message = last_error.strip_prefix("FAILED ").unwrap_or(&last_error).to_owned();
                        *state = InstallState::Failed(if message.is_empty() { "the installer stopped".into() } else { message });
                    }
                });
                "started".into()
            }
            _ => "invalid".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_whole_nvme_and_virtio_disks_only() {
        for good in ["/dev/nvme0n1", "/dev/nvme12n3", "/dev/vda", "/dev/vdz"] {
            assert!(valid_device(good), "{good}");
        }
        for bad in ["/dev/nvme0n1p1", "/dev/vda1", "/dev/sda", "/dev/nvme0", "/dev/vd", "vda", "/dev/../dev/vda", "/dev/nvmexn1"] {
            assert!(!valid_device(bad), "{bad}");
        }
    }

    #[test]
    fn lists_disks_and_follows_progress() {
        assert_eq!(disks_line("/dev/nvme0n1 512110190592\n/dev/vda1 10\njunk\n"), "disks /dev/nvme0n1=512110190592");
        assert_eq!(disks_line(""), "disks");
        let state = InstallState::Idle.after("PROGRESS 45 Writing the recovery system");
        assert_eq!(state.line(), "running 45 Writing the recovery system");
        assert_eq!(state.clone().after("noise"), state);
        assert_eq!(state.after("DONE"), InstallState::Done);
    }

    #[test]
    fn runs_the_installer_and_reports_its_end() {
        let folder = std::env::temp_dir().join(format!("openshell-install-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let script = folder.join("install.sh");
        std::fs::write(
            &script,
            "case $1 in list) echo '/dev/vdb 20000000000';; install) echo 'PROGRESS 50 Half'; echo DONE;; esac\n",
        )
        .unwrap();
        let installer = Installer::new(vec!["/bin/sh".into(), script.to_string_lossy().into_owned()]);
        assert_eq!(installer.answer("install-list"), "disks /dev/vdb=20000000000");
        assert_eq!(installer.answer("install /dev/sda"), "invalid");
        assert_eq!(installer.answer("install /dev/vdb"), "started");
        for _ in 0..100 {
            if installer.answer("install-status") == "done" {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(installer.answer("install-status"), "done");
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
