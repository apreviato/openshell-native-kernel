//! Remote access by SSH for the session, as OpenShell Live's Settings ▸
//! Remote access gives it with openssh-server: on or off.
//!
//! The server (OpenSSH's sshd) is root's and listens for the whole network,
//! so the session only asks for it, on the system socket (install.rs):
//!
//! ```text
//! ssh status          → "ssh <json>": enabled (asked for), active (the
//!                       server runs), port, and its host keys' fingerprints
//!                       as `ssh-keygen -l` prints them
//! ssh enable on|off   → "ok"
//! ```
//!
//! The supervisor keeps a mark in its settings folder (`settings_dir`) while
//! SSH is on. installed-session.sh, which is root with every capability,
//! runs the server while the mark is there: it makes the host keys once
//! (they stay in the settings folder, so the computer keeps its identity),
//! gives the account its shell, home and password hash where sshd looks for
//! them, and leaves in `/run/openshell-ssh` a file while the server runs and
//! the keys' fingerprints. Turning SSH on while the firewall is on lets its
//! port in, and turning it off closes it again, as Live does with ufw.
//! A live session, which has no account password and keeps nothing, has no
//! SSH server: the requests answer "unavailable".

use std::fs;
use std::io;
use std::path::PathBuf;

use crate::firewall::Firewall;

pub const PREFIX: &str = "ssh ";
/// In `settings_dir` (shared with installed-session.sh): SSH is on.
pub const ON_MARK: &str = "ssh.on";
/// What installed-session.sh leaves while the server runs.
pub const RUN_FOLDER: &str = "/run/openshell-ssh";
pub const RUNNING_FILE: &str = "running";
pub const FINGERPRINTS_FILE: &str = "fingerprints";
/// The server, where the system image installs it.
pub const SERVER: &str = "/opt/openshell-compat/bin/sshd";
/// The port installed-session.sh gives the server.
pub const PORT: u16 = 22;
/// The firewall's rule for it (firewall.rs, the kernel's format).
pub const FIREWALL_RULE: &str = "rule allow tcp 22 22 any";
/// Host keys: one of each kind the server could have.
const MAX_FINGERPRINTS: usize = 8;

pub fn is_request(line: &str) -> bool {
    line.starts_with(PREFIX)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Status,
    Enable(bool),
}

impl Request {
    pub fn parse(line: &str) -> Option<Self> {
        match line.strip_prefix(PREFIX)? {
            "status" => Some(Request::Status),
            "enable on" => Some(Request::Enable(true)),
            "enable off" => Some(Request::Enable(false)),
            _ => None,
        }
    }
}

/// The supervisor's side of the requests.
pub struct Ssh {
    /// The supervisor's settings folder; a live session has none.
    pub settings: Option<PathBuf>,
    pub run: PathBuf,
    pub server: PathBuf,
    pub firewall: Firewall,
}

impl Ssh {
    pub fn new(settings: Option<PathBuf>) -> Self {
        Self { firewall: Firewall::new(settings.clone()), settings, run: PathBuf::from(RUN_FOLDER), server: PathBuf::from(SERVER) }
    }

    /// `ssh-keygen -l`'s lines, as installed-session.sh left them:
    /// "256 SHA256:… comment (ED25519)".
    fn fingerprints(&self) -> Vec<String> {
        fs::read_to_string(self.run.join(FINGERPRINTS_FILE))
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.is_empty() && line.len() <= 200 && line.bytes().all(|byte| (0x20..0x7f).contains(&byte) && byte != b'"' && byte != b'\\'))
            .take(MAX_FINGERPRINTS)
            .map(str::to_owned)
            .collect()
    }

    fn enable(&self, mark: PathBuf, on: bool) -> io::Result<()> {
        if on {
            fs::write(mark, b"")?;
            self.firewall.open(FIREWALL_RULE)
        } else {
            match fs::remove_file(mark) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
            self.firewall.close(FIREWALL_RULE)
        }
    }

    /// The answer to a request line.
    pub fn answer(&self, line: &str) -> String {
        let Some(request) = Request::parse(line) else { return "invalid".into() };
        let Some(settings) = self.settings.as_ref().filter(|_| self.server.exists()) else {
            return "unavailable".into();
        };
        let mark = settings.join(ON_MARK);
        match request {
            Request::Status => {
                let enabled = mark.exists();
                // The server takes a moment to stop: it is not active once
                // it was turned off.
                let active = enabled && self.run.join(RUNNING_FILE).exists();
                let fingerprints: Vec<String> = self.fingerprints().iter().map(|line| format!("\"{line}\"")).collect();
                format!("ssh {{\"enabled\":{enabled},\"active\":{active},\"port\":{PORT},\"fingerprints\":[{}]}}", fingerprints.join(","))
            }
            Request::Enable(on) => match self.enable(mark, on) {
                Ok(()) => "ok".into(),
                Err(error) => format!("failed {error}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh(name: &str) -> (Ssh, PathBuf) {
        let folder = std::env::temp_dir().join(format!("openshell-ssh-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(folder.join("system")).unwrap();
        fs::create_dir_all(folder.join("run")).unwrap();
        fs::write(folder.join("sshd"), "").unwrap();
        fs::write(folder.join("kernel"), "").unwrap();
        let settings = Some(folder.join("system"));
        let firewall = Firewall { settings: settings.clone(), kernel: folder.join("kernel") };
        (Ssh { settings, run: folder.join("run"), server: folder.join("sshd"), firewall }, folder)
    }

    #[test]
    fn requests_are_whole_lines() {
        assert_eq!(Request::parse("ssh status"), Some(Request::Status));
        assert_eq!(Request::parse("ssh enable on"), Some(Request::Enable(true)));
        assert_eq!(Request::parse("ssh enable off"), Some(Request::Enable(false)));
        for bad in ["ssh", "ssh ", "ssh status now", "ssh enable", "ssh enable yes", "ssh enable on ", "ssh  status", "sshd status", "ssh port 2222"] {
            assert_eq!(Request::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn turns_on_and_off_and_tells_how_it_is() {
        let (ssh, folder) = ssh("status");
        assert_eq!(ssh.answer("ssh status"), "ssh {\"enabled\":false,\"active\":false,\"port\":22,\"fingerprints\":[]}");
        assert_eq!(ssh.answer("ssh enable on"), "ok");
        assert!(folder.join("system").join(ON_MARK).exists());
        // Asked for, and the server not up yet.
        assert_eq!(ssh.answer("ssh status"), "ssh {\"enabled\":true,\"active\":false,\"port\":22,\"fingerprints\":[]}");
        fs::write(folder.join("run").join(RUNNING_FILE), "").unwrap();
        fs::write(
            folder.join("run").join(FINGERPRINTS_FILE),
            "256 SHA256:abcDEF+/123 root@openshell (ED25519)\n\n3072 SHA256:xyz root@openshell (RSA)\nbad \"quote\"\nbad \\ slash\nbad \u{7} bell\n",
        )
        .unwrap();
        assert_eq!(
            ssh.answer("ssh status"),
            "ssh {\"enabled\":true,\"active\":true,\"port\":22,\"fingerprints\":[\"256 SHA256:abcDEF+/123 root@openshell (ED25519)\",\"3072 SHA256:xyz root@openshell (RSA)\"]}"
        );
        assert_eq!(ssh.answer("ssh enable on"), "ok");
        assert_eq!(ssh.answer("ssh enable off"), "ok");
        assert!(!folder.join("system").join(ON_MARK).exists());
        // The server has not stopped yet: off is off.
        assert!(ssh.answer("ssh status").starts_with("ssh {\"enabled\":false,\"active\":false,"));
        assert_eq!(ssh.answer("ssh enable off"), "ok");
        assert_eq!(ssh.answer("ssh enable maybe"), "invalid");
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn its_port_follows_it_through_a_firewall_that_is_on() {
        let (ssh, folder) = ssh("firewall");
        // The firewall off: its rules are left alone.
        assert_eq!(ssh.answer("ssh enable on"), "ok");
        assert_eq!(fs::read_to_string(folder.join("kernel")).unwrap(), "");
        assert!(!folder.join("system").join(crate::firewall::RULES_FILE).exists());
        assert_eq!(ssh.answer("ssh enable off"), "ok");
        assert_eq!(ssh.firewall.answer("firewall enable on"), "ok");
        assert_eq!(ssh.firewall.answer("firewall add deny tcp 80 80 any"), "ok");
        assert_eq!(ssh.answer("ssh enable on"), "ok");
        assert_eq!(fs::read_to_string(folder.join("kernel")).unwrap(), "default deny\nrule deny tcp 80 80 any\nrule allow tcp 22 22 any\n");
        // Once is enough.
        assert_eq!(ssh.answer("ssh enable on"), "ok");
        assert_eq!(fs::read_to_string(folder.join("system").join(crate::firewall::RULES_FILE)).unwrap(), "default deny\nrule deny tcp 80 80 any\nrule allow tcp 22 22 any\n");
        assert_eq!(ssh.answer("ssh enable off"), "ok");
        assert_eq!(fs::read_to_string(folder.join("kernel")).unwrap(), "default deny\nrule deny tcp 80 80 any\n");
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn a_session_that_keeps_nothing_or_an_image_without_the_server_has_none() {
        let (mut ssh, folder) = ssh("none");
        fs::remove_file(folder.join("sshd")).unwrap();
        assert_eq!(ssh.answer("ssh status"), "unavailable");
        assert_eq!(ssh.answer("ssh enable on"), "unavailable");
        assert_eq!(ssh.answer("ssh nonsense"), "invalid");
        fs::write(folder.join("sshd"), "").unwrap();
        ssh.settings = None;
        assert_eq!(ssh.answer("ssh status"), "unavailable");
        let _ = fs::remove_dir_all(folder);
    }
}
