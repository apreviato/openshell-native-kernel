//! The firewall for the session, as ufw gives it on OpenShell Live: on or
//! off, and rules that allow, deny or limit new connections to a port.
//!
//! OpenShell Native's kernel filters by itself (its network library's
//! `iface::firewall`): root writes the rules, as text, to
//! `/proc/net/firewall`, and they hold until the next ones. The supervisor
//! keeps them in its settings folder (`settings_dir`), in that same text,
//! with a mark while the firewall is on; installed-session.sh puts them in
//! force at each boot, and the supervisor at each change. The session asks
//! on the system socket (install.rs):
//!
//! ```text
//! firewall status                  → "firewall <json>": enabled, and the
//!                                    rules' text (the kernel's format)
//! firewall enable on|off           → "ok"
//! firewall add <action> <protocol> <first-port> <last-port> <from>
//!                                  → "ok": the rule goes last
//! firewall delete <number>         → "ok": rules count from 1
//! ```
//!
//! `<action>` is allow, deny, reject or limit; `<protocol>` tcp, udp or
//! any; `<from>` any, an address or a network (`192.168.1.0/24`). What is
//! not allowed in is denied, what goes out is free, and what the computer
//! itself started is answered, as ufw's defaults are. Without a settings
//! folder (a live session) the rules last until the computer stops.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::PathBuf;

pub const PREFIX: &str = "firewall ";
/// Where the kernel takes and shows the rules.
pub const KERNEL_FILE: &str = "/proc/net/firewall";
/// In `settings_dir` (shared with installed-session.sh): the rules, and
/// the mark of a firewall that is on.
pub const RULES_FILE: &str = "firewall.rules";
pub const ON_MARK: &str = "firewall.on";
/// Where a session that keeps nothing has them.
const VOLATILE_FOLDER: &str = "/run/openshell-firewall";
/// What is not allowed in, when the firewall is first turned on.
const FIRST_RULES: &str = "default deny\n";
/// The kernel's limit.
const MAX_RULES: usize = 256;

pub fn is_request(line: &str) -> bool {
    line.starts_with(PREFIX)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Status,
    Enable(bool),
    /// The rule's line, in the kernel's format.
    Add(String),
    Delete(usize),
}

fn port(word: &str) -> Option<u16> {
    (!word.is_empty() && word.len() <= 5 && word.bytes().all(|digit| digit.is_ascii_digit())).then(|| word.parse::<u16>().ok())?.filter(|port| *port != 0)
}

/// An address or a network as it is written: the kernel checks the rest.
fn valid_from(word: &str) -> bool {
    word == "any" || (!word.is_empty() && word.len() <= 49 && word.bytes().all(|byte| byte.is_ascii_hexdigit() || matches!(byte, b'.' | b':' | b'/')))
}

impl Request {
    pub fn parse(line: &str) -> Option<Self> {
        let mut words = line.strip_prefix(PREFIX)?.split(' ');
        let request = match words.next()? {
            "status" => Request::Status,
            "enable" => Request::Enable(match words.next()? {
                "on" => true,
                "off" => false,
                _ => return None,
            }),
            "add" => {
                let action = words.next().filter(|action| matches!(*action, "allow" | "deny" | "reject" | "limit"))?;
                let protocol = words.next().filter(|protocol| matches!(*protocol, "tcp" | "udp" | "any"))?;
                let (first, last) = (port(words.next()?)?, port(words.next()?)?);
                let from = words.next().filter(|from| valid_from(from))?;
                if first > last {
                    return None;
                }
                Request::Add(format!("rule {action} {protocol} {first} {last} {from}"))
            }
            "delete" => {
                let word = words.next()?;
                let number = (!word.is_empty() && word.len() <= 3 && word.bytes().all(|digit| digit.is_ascii_digit())).then(|| word.parse::<usize>().ok())??;
                Request::Delete(number.checked_sub(1)?)
            }
            _ => return None,
        };
        words.next().is_none().then_some(request)
    }
}

fn json_string(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// The supervisor's side of the requests.
pub struct Firewall {
    /// The supervisor's settings folder, where an installed system keeps
    /// the rules.
    pub settings: Option<PathBuf>,
    pub kernel: PathBuf,
}

impl Firewall {
    pub fn new(settings: Option<PathBuf>) -> Self {
        Self { settings, kernel: PathBuf::from(KERNEL_FILE) }
    }

    fn folder(&self) -> PathBuf {
        self.settings.clone().unwrap_or_else(|| PathBuf::from(VOLATILE_FOLDER))
    }

    fn enabled(&self) -> bool {
        self.folder().join(ON_MARK).exists()
    }

    /// The saved rules: the default, then one rule a line.
    fn rules(&self) -> String {
        fs::read_to_string(self.folder().join(RULES_FILE)).ok().filter(|text| text.starts_with("default ")).unwrap_or_else(|| FIRST_RULES.to_owned())
    }

    /// Hands the kernel the rules (or none: the firewall off).
    fn apply(&self, enabled: bool, rules: &str) -> io::Result<()> {
        // One write is the whole set; an empty line is no rules.
        fs::write(&self.kernel, if enabled { rules } else { "\n" })
    }

    /// Puts rules in force (if the firewall is on) and saves them. The
    /// kernel is asked first: rules it refuses are not kept.
    fn change(&self, enabled: bool, rules: &str) -> io::Result<()> {
        self.apply(enabled, rules)?;
        let folder = self.folder();
        fs::create_dir_all(&folder)?;
        fs::write(folder.join(RULES_FILE), rules)?;
        let mark = folder.join(ON_MARK);
        if enabled {
            fs::write(mark, b"")
        } else {
            match fs::remove_file(mark) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            }
        }
    }

    /// Lets a service of the system in (ssh.rs): its rule goes last, unless
    /// it is there. Only while the firewall is on, as Live does with ufw:
    /// a firewall turned on later starts from the rules the person keeps.
    pub fn open(&self, rule: &str) -> io::Result<()> {
        let rules = self.rules();
        if !self.enabled() || !self.kernel.exists() || rules.lines().any(|line| line == rule) {
            return Ok(());
        }
        self.change(true, &format!("{rules}{rule}\n"))
    }

    /// Takes a service's rule away again, while the firewall is on.
    pub fn close(&self, rule: &str) -> io::Result<()> {
        let rules = self.rules();
        if !self.enabled() || !self.kernel.exists() || !rules.lines().any(|line| line == rule) {
            return Ok(());
        }
        let kept: Vec<&str> = rules.lines().filter(|line| *line != rule).collect();
        self.change(true, &format!("{}\n", kept.join("\n")))
    }

    /// The answer to a request line.
    pub fn answer(&self, line: &str) -> String {
        let Some(request) = Request::parse(line) else { return "invalid".into() };
        if !self.kernel.exists() {
            return "unavailable".into();
        }
        let (enabled, rules) = (self.enabled(), self.rules());
        let changed = match request {
            Request::Status => {
                let mut out = format!("firewall {{\"enabled\":{enabled},\"rules\":");
                json_string(&mut out, &rules);
                out.push('}');
                return out;
            }
            Request::Enable(on) => self.change(on, &rules),
            Request::Add(rule) => {
                if rules.lines().count() > MAX_RULES {
                    return "failed there are too many rules".into();
                }
                self.change(enabled, &format!("{rules}{rule}\n"))
            }
            Request::Delete(index) => {
                let mut lines: Vec<&str> = rules.lines().collect();
                // The first line is the default; the rules follow.
                if index + 1 >= lines.len() {
                    return "invalid".into();
                }
                lines.remove(index + 1);
                self.change(enabled, &format!("{}\n", lines.join("\n")))
            }
        };
        match changed {
            Ok(()) => "ok".into(),
            // The kernel refuses what is not a set of rules (an address
            // that is none).
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => "invalid".into(),
            Err(error) => format!("failed {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn firewall(name: &str) -> (Firewall, PathBuf) {
        let folder = std::env::temp_dir().join(format!("openshell-firewall-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        let kernel = folder.join("kernel");
        fs::write(&kernel, "").unwrap();
        (Firewall { settings: Some(folder.join("system")), kernel }, folder)
    }

    #[test]
    fn requests_are_checked_word_by_word() {
        assert_eq!(Request::parse("firewall status"), Some(Request::Status));
        assert_eq!(Request::parse("firewall enable on"), Some(Request::Enable(true)));
        assert_eq!(Request::parse("firewall delete 3"), Some(Request::Delete(2)));
        assert_eq!(Request::parse("firewall add allow tcp 22 22 any"), Some(Request::Add("rule allow tcp 22 22 any".into())));
        assert_eq!(
            Request::parse("firewall add limit any 6000 6007 192.168.1.0/24"),
            Some(Request::Add("rule limit any 6000 6007 192.168.1.0/24".into()))
        );
        assert!(Request::parse("firewall add deny udp 53 53 fe80::1/64").is_some());
        for bad in [
            "firewall",
            "firewall status now",
            "firewall enable",
            "firewall enable yes",
            "firewall delete 0",
            "firewall delete -1",
            "firewall delete 1 2",
            "firewall delete 1000",
            "firewall add allow tcp 22 22",
            "firewall add allow tcp 22 22 any extra",
            "firewall add permit tcp 22 22 any",
            "firewall add allow icmp 22 22 any",
            "firewall add allow tcp 0 22 any",
            "firewall add allow tcp 23 22 any",
            "firewall add allow tcp 22 65536 any",
            "firewall add allow tcp 22 22 any\nrule allow any 1 65535 any",
            "firewall add allow tcp 22 22 host.example",
            "firewall add allow tcp 22 22 ",
        ] {
            assert_eq!(Request::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn turning_on_puts_the_rules_in_force_and_keeps_them() {
        let (firewall, folder) = firewall("enable");
        assert_eq!(firewall.answer("firewall status"), "firewall {\"enabled\":false,\"rules\":\"default deny\\u000a\"}");
        assert_eq!(firewall.answer("firewall add allow tcp 22 22 any"), "ok");
        // Off: the kernel has no rules, the file has them.
        assert_eq!(fs::read_to_string(&firewall.kernel).unwrap(), "\n");
        let saved = folder.join("system").join(RULES_FILE);
        assert_eq!(fs::read_to_string(&saved).unwrap(), "default deny\nrule allow tcp 22 22 any\n");
        assert_eq!(firewall.answer("firewall enable on"), "ok");
        assert_eq!(fs::read_to_string(&firewall.kernel).unwrap(), "default deny\nrule allow tcp 22 22 any\n");
        assert!(folder.join("system").join(ON_MARK).exists());
        assert_eq!(firewall.answer("firewall add limit tcp 5900 5900 192.168.1.0/24"), "ok");
        assert_eq!(
            firewall.answer("firewall status"),
            "firewall {\"enabled\":true,\"rules\":\"default deny\\u000arule allow tcp 22 22 any\\u000arule limit tcp 5900 5900 192.168.1.0/24\\u000a\"}"
        );
        assert_eq!(fs::read_to_string(&firewall.kernel).unwrap(), "default deny\nrule allow tcp 22 22 any\nrule limit tcp 5900 5900 192.168.1.0/24\n");
        // Rules count from 1, after the default.
        assert_eq!(firewall.answer("firewall delete 1"), "ok");
        assert_eq!(fs::read_to_string(&firewall.kernel).unwrap(), "default deny\nrule limit tcp 5900 5900 192.168.1.0/24\n");
        assert_eq!(firewall.answer("firewall delete 2"), "invalid");
        assert_eq!(firewall.answer("firewall delete 1"), "ok");
        assert_eq!(fs::read_to_string(&saved).unwrap(), "default deny\n");
        assert_eq!(firewall.answer("firewall enable off"), "ok");
        assert_eq!(fs::read_to_string(&firewall.kernel).unwrap(), "\n");
        assert!(!folder.join("system").join(ON_MARK).exists());
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn without_the_kernels_file_there_is_no_firewall() {
        let (mut firewall, folder) = firewall("none");
        firewall.kernel = folder.join("missing");
        assert_eq!(firewall.answer("firewall status"), "unavailable");
        assert_eq!(firewall.answer("firewall enable on"), "unavailable");
        assert_eq!(firewall.answer("firewall bogus"), "invalid");
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn what_the_kernel_refuses_is_not_kept() {
        let (mut firewall, folder) = firewall("refused");
        assert_eq!(firewall.answer("firewall enable on"), "ok");
        // A kernel file that cannot be written (a folder) stands for the
        // kernel's refusal.
        firewall.kernel = folder.join("system");
        let answer = firewall.answer("firewall add allow tcp 80 80 any");
        assert!(answer.starts_with("failed "), "{answer}");
        assert_eq!(fs::read_to_string(folder.join("system").join(RULES_FILE)).unwrap(), "default deny\n");
        let _ = fs::remove_dir_all(folder);
    }
}
