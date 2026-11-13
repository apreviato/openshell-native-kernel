//! The computer's settings that belong to root: the time zone and the account's
//! full name. On OpenShell Live, the Desktop changes them with `timedatectl` and
//! `sudo chfn`; OpenShell Native has neither, and its root filesystem lives in
//! memory. The session asks the supervisor on its system socket (install.rs):
//!
//! ```text
//! timezone <Area/City>   → "ok" or "invalid"
//! name <full name>       → "ok" or "invalid"
//! ```
//!
//! The supervisor sets /etc/localtime and the session user's /etc/passwd entry
//! at once, and keeps each value in its settings folder (`settings_dir`, on
//! the data partition of an installed system), from which it sets them again
//! at every boot, before the session starts.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

pub const TIMEZONE_REQUEST: &str = "timezone";
pub const NAME_REQUEST: &str = "name";
/// Where the system's files are; tests use their own.
pub const ZONEINFO: &str = "/usr/share/zoneinfo";
pub const LOCALTIME: &str = "/etc/localtime";
pub const PASSWD: &str = "/etc/passwd";
const MAX_NAME: usize = 64;

#[derive(Debug, PartialEq, Eq)]
pub enum Setting {
    TimeZone(String),
    Name(String),
}

impl Setting {
    /// The setting of a request line, None for any other line (or "invalid"
    /// when the verb is right and the value is not: see `is_request`).
    pub fn from_request(line: &str) -> Option<Self> {
        let (verb, value) = line.split_once(' ')?;
        match verb {
            TIMEZONE_REQUEST if valid_timezone(value) => Some(Setting::TimeZone(value.to_owned())),
            NAME_REQUEST if valid_name(value) => Some(Setting::Name(value.to_owned())),
            _ => None,
        }
    }

    pub fn is_request(line: &str) -> bool {
        let verb = line.split(' ').next().unwrap_or("");
        verb == TIMEZONE_REQUEST || verb == NAME_REQUEST
    }

    fn file(&self) -> &'static str {
        match self {
            Setting::TimeZone(_) => TIMEZONE_REQUEST,
            Setting::Name(_) => NAME_REQUEST,
        }
    }

    fn value(&self) -> &str {
        match self {
            Setting::TimeZone(value) | Setting::Name(value) => value,
        }
    }
}

/// An IANA zone name such as `America/Sao_Paulo` or `UTC`: no path tricks.
pub fn valid_timezone(zone: &str) -> bool {
    !zone.is_empty()
        && zone.len() <= 64
        && !zone.starts_with('/')
        && zone.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
        && zone.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_+-/".contains(&byte))
}

/// What chfn takes as a full name: no field separators or control characters.
pub fn valid_name(name: &str) -> bool {
    name.len() <= MAX_NAME && !name.trim().is_empty() && !name.chars().any(|c| c.is_control() || c == ':' || c == ',' || c == '=')
}

/// The system's files a setting changes.
pub struct SystemFiles {
    pub zoneinfo: PathBuf,
    pub localtime: PathBuf,
    pub passwd: PathBuf,
}

impl Default for SystemFiles {
    fn default() -> Self {
        Self { zoneinfo: ZONEINFO.into(), localtime: LOCALTIME.into(), passwd: PASSWD.into() }
    }
}

impl SystemFiles {
    /// Applies a setting for the session user `uid`.
    pub fn apply(&self, setting: &Setting, uid: u32) -> io::Result<()> {
        match setting {
            Setting::TimeZone(zone) => {
                let target = self.zoneinfo.join(zone);
                if !target.is_file() {
                    return Err(io::Error::new(io::ErrorKind::NotFound, format!("no time zone {zone}")));
                }
                let partial = self.localtime.with_extension("partial");
                let _ = fs::remove_file(&partial);
                symlink(&target, &partial)?;
                fs::rename(&partial, &self.localtime)
            }
            Setting::Name(name) => {
                let text = fs::read_to_string(&self.passwd)?;
                let changed = with_full_name(&text, uid, name)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no account {uid}")))?;
                write_atomically(&self.passwd, changed.as_bytes())
            }
        }
    }

    /// The zone /etc/localtime names, if it is a link into the zone database.
    pub fn timezone(&self) -> Option<String> {
        let target = fs::read_link(&self.localtime).ok()?;
        target.strip_prefix(&self.zoneinfo).ok()?.to_str().map(str::to_owned)
    }
}

/// /etc/passwd with the full name (the first part of the comment field) of
/// `uid` replaced, or None without that account.
pub fn with_full_name(passwd: &str, uid: u32, name: &str) -> Option<String> {
    let mut found = false;
    let lines: Vec<String> = passwd
        .lines()
        .map(|line| {
            let mut fields: Vec<&str> = line.split(':').collect();
            if fields.len() == 7 && fields[2].parse() == Ok(uid) && !found {
                found = true;
                let rest = fields[4].split_once(',').map(|(_, rest)| format!(",{rest}")).unwrap_or_default();
                let comment = format!("{name}{rest}");
                fields[4] = &comment;
                return fields.join(":");
            }
            line.to_owned()
        })
        .collect();
    found.then(|| lines.join("\n") + "\n")
}

fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let partial = path.with_extension("partial");
    let mode = fs::metadata(path).map(|meta| meta.permissions()).ok();
    let mut file = fs::File::create(&partial)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    if let Some(mode) = mode {
        fs::set_permissions(&partial, mode)?;
    }
    fs::rename(&partial, path)
}

/// The settings kept in a folder, one file per setting.
pub struct Saved(pub PathBuf);

impl Saved {
    pub fn save(&self, setting: &Setting) -> io::Result<()> {
        fs::create_dir_all(&self.0)?;
        write_atomically(&self.0.join(setting.file()), format!("{}\n", setting.value()).as_bytes())
    }

    /// The saved settings that are still valid.
    pub fn load(&self) -> Vec<Setting> {
        [TIMEZONE_REQUEST, NAME_REQUEST]
            .iter()
            .filter_map(|verb| {
                let value = fs::read_to_string(self.0.join(verb)).ok()?;
                Setting::from_request(&format!("{verb} {}", value.trim_end_matches('\n')))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_setting_requests() {
        assert_eq!(Setting::from_request("timezone America/Sao_Paulo"), Some(Setting::TimeZone("America/Sao_Paulo".into())));
        assert_eq!(Setting::from_request("timezone UTC"), Some(Setting::TimeZone("UTC".into())));
        assert_eq!(Setting::from_request("name Ana Souza"), Some(Setting::Name("Ana Souza".into())));
        for bad in ["timezone ../etc/passwd", "timezone /UTC", "timezone Europe//Rome", "timezone a b", "name a:b", "name x,y", "name  ", "timezone", "zone UTC"] {
            assert_eq!(Setting::from_request(bad), None, "{bad}");
        }
        assert!(Setting::is_request("name a:b") && Setting::is_request("timezone") && !Setting::is_request("names"));
    }

    #[test]
    fn changes_only_the_full_name_of_the_account() {
        let passwd = "root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:nobody,room 1:/nonexistent:/usr/sbin/nologin\n";
        assert_eq!(
            with_full_name(passwd, 65534, "Ana Souza").unwrap(),
            "root:x:0:0:root:/root:/bin/sh\nnobody:x:65534:65534:Ana Souza,room 1:/nonexistent:/usr/sbin/nologin\n"
        );
        assert_eq!(with_full_name(passwd, 1000, "x"), None);
    }

    #[test]
    fn applies_and_keeps_settings() {
        let root = std::env::temp_dir().join(format!("openshell-settings-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("zoneinfo/America")).unwrap();
        fs::write(root.join("zoneinfo/America/Sao_Paulo"), b"TZif").unwrap();
        fs::write(root.join("passwd"), "nobody:x:65534:65534:nobody:/nonexistent:/bin/false\n").unwrap();
        let files = SystemFiles { zoneinfo: root.join("zoneinfo"), localtime: root.join("localtime"), passwd: root.join("passwd") };
        let zone = Setting::TimeZone("America/Sao_Paulo".into());
        files.apply(&zone, 65534).unwrap();
        assert_eq!(files.timezone().as_deref(), Some("America/Sao_Paulo"));
        assert!(files.apply(&Setting::TimeZone("Mars/Base".into()), 65534).is_err());
        let name = Setting::Name("Ana".into());
        files.apply(&name, 65534).unwrap();
        assert!(fs::read_to_string(root.join("passwd")).unwrap().starts_with("nobody:x:65534:65534:Ana:"));
        let saved = Saved(root.join("saved"));
        saved.save(&zone).unwrap();
        saved.save(&name).unwrap();
        assert_eq!(saved.load(), vec![zone, name]);
        fs::remove_dir_all(&root).unwrap();
    }
}
