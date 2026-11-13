//! Updates of an installed OpenShell Native from its release channel
//! (distro/image/channel.sh), as Settings ▸ Updates installs them on OpenShell
//! Live: downloaded during the session, applied at the next restart.
//!
//! The Desktop downloads the release archive into the inbox, a folder of the
//! session user in the supervisor's updates folder (`updates_dir`). The
//! session asks on the system socket (install.rs):
//!
//! ```text
//! update-status           → "idle", "staged <archive>", "applied <release>",
//!                           "failed <message>" or, without updates, "unavailable"
//! update-stage <archive>  → "ok" or "invalid"
//! update-discard          → "ok"
//! ```
//!
//! Staging only records the archive's name, in a file of root's. Before the
//! computer restarts or stops, and at the next boot if it did not,
//! installed-session.sh (as init) unpacks it, writes the inactive slot with
//! `openshell-ab update-mounted`, which checks the release's signed manifest
//! against the release keys, schedules the trial boot and records the result
//! (`last`). Nothing in the inbox is trusted before that check.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const UPDATE_STATUS: &str = "update-status";
pub const UPDATE_STAGE: &str = "update-stage";
pub const UPDATE_DISCARD: &str = "update-discard";
/// The folders and files in `updates_dir` (shared with installed-session.sh).
pub const INBOX: &str = "inbox";
pub const STAGED: &str = "staged";
pub const LAST: &str = "last";
/// What the session tells hostd: the inbox, and the folder of the release keys.
pub const INBOX_ENV: &str = "OPENSHELL_NATIVE_UPDATES_INBOX";
pub const RELEASE_KEYS_ENV: &str = "OPENSHELL_NATIVE_RELEASE_KEYS";

/// A release archive's name, as channel.sh names them.
pub fn valid_archive(name: &str) -> bool {
    name.strip_prefix("openshell-native-")
        .and_then(|rest| rest.strip_suffix("-amd64.tar"))
        .is_some_and(|version| {
            !version.is_empty()
                && version.len() <= 64
                && version.bytes().all(|byte| byte.is_ascii_alphanumeric() || b".-_+".contains(&byte))
        })
}

pub fn is_request(line: &str) -> bool {
    let verb = line.split(' ').next().unwrap_or("");
    verb == UPDATE_STATUS || verb == UPDATE_STAGE || verb == UPDATE_DISCARD
}

/// The updates folder of an installed system.
pub struct Updates(pub PathBuf);

impl Updates {
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// The answer to one request line (`is_request`).
    pub fn answer(&self, line: &str) -> io::Result<String> {
        let (verb, argument) = line.split_once(' ').unwrap_or((line, ""));
        match (verb, argument) {
            (UPDATE_STATUS, "") => self.status(),
            (UPDATE_STAGE, name) if valid_archive(name) && self.path(INBOX).join(name).is_file() => {
                write_root_file(&self.path(STAGED), name)?;
                Ok("ok".into())
            }
            (UPDATE_DISCARD, "") => {
                match fs::remove_file(self.path(STAGED)) {
                    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                    _ => {}
                }
                Ok("ok".into())
            }
            _ => Ok("invalid".into()),
        }
    }

    fn status(&self) -> io::Result<String> {
        if let Some(name) = read_line(&self.path(STAGED)) {
            if valid_archive(&name) {
                return Ok(format!("staged {name}"));
            }
        }
        Ok(match read_line(&self.path(LAST)) {
            Some(last) if last.starts_with("applied ") || last.starts_with("failed ") => last,
            _ => "idle".into(),
        })
    }
}

fn read_line(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let line: String = text.lines().next()?.chars().filter(|c| !c.is_control()).take(300).collect();
    Some(line)
}

fn write_root_file(path: &Path, line: &str) -> io::Result<()> {
    let partial = path.with_extension("partial");
    fs::write(&partial, format!("{line}\n"))?;
    fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_release_archives_only() {
        assert!(valid_archive("openshell-native-2.1.0-amd64.tar"));
        for bad in ["openshell-native--amd64.tar", "openshell-native-2.1.0-amd64.tar.gz", "../openshell-native-1-amd64.tar", "openshell-native-a/b-amd64.tar", "x.tar"] {
            assert!(!valid_archive(bad), "{bad}");
        }
    }

    #[test]
    fn stages_and_reports() {
        let root = std::env::temp_dir().join(format!("openshell-updates-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(INBOX)).unwrap();
        let updates = Updates(root.clone());
        assert_eq!(updates.answer("update-status").unwrap(), "idle");
        assert_eq!(updates.answer("update-stage openshell-native-2.2.0-amd64.tar").unwrap(), "invalid", "not downloaded");
        fs::write(root.join(INBOX).join("openshell-native-2.2.0-amd64.tar"), b"x").unwrap();
        assert_eq!(updates.answer("update-stage openshell-native-2.2.0-amd64.tar").unwrap(), "ok");
        assert_eq!(updates.answer("update-status").unwrap(), "staged openshell-native-2.2.0-amd64.tar");
        assert_eq!(updates.answer("update-discard").unwrap(), "ok");
        assert_eq!(updates.answer("update-discard").unwrap(), "ok");
        fs::write(root.join(LAST), "applied 2.2.0\n").unwrap();
        assert_eq!(updates.answer("update-status").unwrap(), "applied 2.2.0");
        assert_eq!(updates.answer("update-stage ../x").unwrap(), "invalid");
        assert!(is_request("update-status") && !is_request("updates"));
        fs::remove_dir_all(&root).unwrap();
    }
}
