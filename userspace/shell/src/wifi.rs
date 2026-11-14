//! Wi-Fi for the session, as NetworkManager gives it on OpenShell Live: the
//! Desktop lists networks, joins one with its password, forgets it, turns the
//! radio off. On OpenShell Native the kernel's Wi-Fi interfaces are run by
//! wpa_supplicant (as root, from installed-session.sh), whose control socket
//! is as good as root itself: it loads any configuration file and any
//! plug-in it is told to. So the session never reaches it. The session asks
//! the supervisor on the system socket (install.rs), and the supervisor,
//! which checks every word, speaks to wpa_supplicant:
//!
//! ```text
//! wifi status                      → "wifi <json>": enabled, interface, and
//!                                    wpa_supplicant's STATUS and SIGNAL_POLL
//! wifi networks                    → "wifi <json>": its SCAN_RESULTS and
//!                                    LIST_NETWORKS
//! wifi scan                        → "ok", "busy" or "failed <why>"
//! wifi connect <ssid> <security> [<secret>] [hidden]
//!                                  → "ok <id>": the network is saved and tried
//! wifi select <id>                 → "ok": a saved network is tried now, and
//!                                    first from now on
//! wifi forget <id>                 → "ok"
//! wifi autoconnect <id> on|off     → "ok"
//! wifi disconnect                  → "ok"
//! wifi enable on|off               → "ok": the radio, kept across restarts
//! ```
//!
//! `<ssid>` and `<secret>` are hexadecimal (a name is any 1 to 32 bytes);
//! `<security>` is `open`, `psk` (WPA and WPA2), `sae` (WPA3) or `psk+sae`
//! (a network that takes both). Without a Wi-Fi interface every request
//! answers "unavailable". The saved networks are wpa_supplicant's own file,
//! in a folder only root enters (it holds the passwords) in the supervisor's
//! settings folder (`settings_dir`), or in /run for a live session, which
//! keeps nothing. Of the saved networks in range, wpa_supplicant joins the
//! one with the highest priority: a network that is chosen gets a priority
//! above every other's, so the last one chosen wins, here and after a
//! restart, and the others stay known for when it is out of range.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

pub const PREFIX: &str = "wifi ";
/// Where wpa_supplicant listens (installed-session.sh starts it with the
/// first; it makes the second for each interface it is given).
pub const GLOBAL_SOCKET: &str = "/run/openshell-wifi/global";
pub const INTERFACE_SOCKETS: &str = "/run/openshell-wifi/interfaces";
/// Where a session that keeps nothing has its networks.
const VOLATILE_FOLDER: &str = "/run/openshell-wifi/state";
/// The folder in `settings_dir`, root's alone, and in it the saved networks
/// and the mark of a radio turned off.
const FOLDER: &str = "wifi";
const CONFIG_FILE: &str = "networks.conf";
const DISABLED_MARK: &str = "off";
/// The longest answer of wpa_supplicant taken: the scan results of a few
/// hundred networks.
const MAX_REPLY: usize = 128 * 1024;
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

pub fn is_request(line: &str) -> bool {
    line.starts_with(PREFIX)
}

/// How a network is secured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    Open,
    Psk,
    Sae,
    PskSae,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    Status,
    Networks,
    Scan,
    Connect { ssid: Vec<u8>, security: Security, secret: Vec<u8>, hidden: bool },
    Select(u32),
    Forget(u32),
    Autoconnect(u32, bool),
    Disconnect,
    Enable(bool),
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len()).step_by(2).map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok()).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn switch(word: &str) -> Option<bool> {
    match word {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// A network's number in wpa_supplicant: digits only, small.
fn network_id(word: &str) -> Option<u32> {
    (!word.is_empty() && word.len() <= 6 && word.bytes().all(|byte| byte.is_ascii_digit())).then(|| word.parse().ok())?
}

/// A WPA passphrase (8 to 63 printable characters) or the 64 hexadecimal
/// digits of its key; a WPA3 password is any 1 to 63 printable characters.
fn valid_secret(security: Security, secret: &[u8]) -> bool {
    let printable = secret.iter().all(|byte| (0x20..=0x7e).contains(byte));
    match security {
        Security::Open => secret.is_empty(),
        Security::Sae => printable && (1..=63).contains(&secret.len()),
        Security::Psk | Security::PskSae => {
            (printable && (8..=63).contains(&secret.len())) || (secret.len() == 64 && secret.iter().all(u8::is_ascii_hexdigit))
        }
    }
}

impl Request {
    pub fn parse(line: &str) -> Option<Self> {
        let mut words = line.strip_prefix(PREFIX)?.split(' ');
        let request = match words.next()? {
            "status" => Request::Status,
            "networks" => Request::Networks,
            "scan" => Request::Scan,
            "disconnect" => Request::Disconnect,
            "select" => Request::Select(network_id(words.next()?)?),
            "forget" => Request::Forget(network_id(words.next()?)?),
            "autoconnect" => Request::Autoconnect(network_id(words.next()?)?, switch(words.next()?)?),
            "enable" => Request::Enable(switch(words.next()?)?),
            "connect" => {
                let ssid = unhex(words.next()?)?;
                if ssid.is_empty() || ssid.len() > 32 {
                    return None;
                }
                let security = match words.next()? {
                    "open" => Security::Open,
                    "psk" => Security::Psk,
                    "sae" => Security::Sae,
                    "psk+sae" => Security::PskSae,
                    _ => return None,
                };
                let mut secret = Vec::new();
                let mut hidden = false;
                for word in words.by_ref() {
                    match word {
                        "hidden" if !hidden => hidden = true,
                        _ if secret.is_empty() && !hidden => secret = unhex(word)?,
                        _ => return None,
                    }
                }
                if !valid_secret(security, &secret) {
                    return None;
                }
                Request::Connect { ssid, security, secret, hidden }
            }
            _ => return None,
        };
        words.next().is_none().then_some(request)
    }
}

/// Speaking to wpa_supplicant: its global socket, and each interface's.
pub trait Supplicant {
    fn global(&mut self, command: &str) -> io::Result<String>;
    fn interface(&mut self, name: &str, command: &str) -> io::Result<String>;
}

/// wpa_supplicant's control sockets: a datagram each way.
pub struct ControlSockets;

static CLIENTS: AtomicU32 = AtomicU32::new(0);

fn ask(server: &Path, command: &str) -> io::Result<String> {
    // The answer comes to a socket with a name: one of root's own, next to
    // the server's, gone when it is answered.
    let folder = server.parent().unwrap_or(Path::new("/run"));
    let own = folder.join(format!(".supervisor-{}-{}", std::process::id(), CLIENTS.fetch_add(1, Ordering::Relaxed)));
    let _ = fs::remove_file(&own);
    let socket = UnixDatagram::bind(&own)?;
    let result = (|| {
        socket.set_read_timeout(Some(REPLY_TIMEOUT))?;
        socket.set_write_timeout(Some(REPLY_TIMEOUT))?;
        socket.connect(server)?;
        socket.send(command.as_bytes())?;
        let mut reply = vec![0u8; MAX_REPLY];
        loop {
            let length = socket.recv(&mut reply)?;
            // Lines that start with '<' are events for monitors, not the
            // answer.
            if reply[..length].first() != Some(&b'<') {
                return Ok(String::from_utf8_lossy(&reply[..length]).into_owned());
            }
        }
    })();
    let _ = fs::remove_file(&own);
    result
}

impl Supplicant for ControlSockets {
    fn global(&mut self, command: &str) -> io::Result<String> {
        ask(Path::new(GLOBAL_SOCKET), command)
    }

    fn interface(&mut self, name: &str, command: &str) -> io::Result<String> {
        ask(&Path::new(INTERFACE_SOCKETS).join(name), command)
    }
}

/// The Wi-Fi interfaces among the kernel's (`/proc/net/dev`): the names
/// Linux gives them, `wlan0` or `wlp2s0`.
pub fn wifi_interfaces(proc_net_dev: &str) -> Vec<String> {
    proc_net_dev
        .lines()
        .filter_map(|line| line.split_once(':'))
        .map(|(name, _)| name.trim())
        .filter(|name| name.starts_with("wl") && name.len() <= 15 && name.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        .map(str::to_owned)
        .collect()
}

/// The saved networks of wpa_supplicant's `LIST_NETWORKS`: each number, and
/// whether it is turned off (not joined on its own).
pub fn saved_networks(list: &str) -> Vec<(u32, bool)> {
    list.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let id = network_id(fields.next()?)?;
            Some((id, fields.nth(2).is_some_and(|flags| flags.contains("[DISABLED]"))))
        })
        .collect()
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
pub struct Wifi {
    /// The supervisor's settings folder, where an installed system keeps
    /// the networks.
    pub settings: Option<PathBuf>,
    pub proc_net_dev: PathBuf,
}

impl Wifi {
    pub fn new(settings: Option<PathBuf>) -> Self {
        Self { settings, proc_net_dev: PathBuf::from("/proc/net/dev") }
    }

    /// The folder of the saved networks, which only root enters.
    fn folder(&self) -> io::Result<PathBuf> {
        let folder = self.settings.as_ref().map_or_else(|| PathBuf::from(VOLATILE_FOLDER), |settings| settings.join(FOLDER));
        fs::create_dir_all(&folder)?;
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700))?;
        Ok(folder)
    }

    fn enabled(&self) -> bool {
        self.settings.as_ref().is_none_or(|settings| !settings.join(FOLDER).join(DISABLED_MARK).exists())
    }

    fn set_enabled(&self, enabled: bool) -> io::Result<()> {
        if self.settings.is_none() {
            return Ok(());
        }
        let mark = self.folder()?.join(DISABLED_MARK);
        if enabled {
            match fs::remove_file(mark) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            }
        } else {
            fs::write(mark, b"")
        }
    }

    /// The first Wi-Fi interface: a computer has one.
    fn interface(&self) -> Option<String> {
        wifi_interfaces(&fs::read_to_string(&self.proc_net_dev).ok()?).into_iter().next()
    }

    /// Hands the interface to wpa_supplicant if it does not run it yet,
    /// with the saved networks: it then joins the ones it knows.
    fn attach(&self, supplicant: &mut dyn Supplicant, name: &str) -> io::Result<()> {
        if supplicant.global("INTERFACES")?.lines().any(|line| line == name) {
            return Ok(());
        }
        let config = self.folder()?.join(CONFIG_FILE);
        if !config.exists() {
            // wpa_supplicant rewrites it when networks change, with the
            // permissions it runs with: the folder is what keeps the
            // passwords root's.
            fs::write(&config, b"update_config=1\n")?;
            fs::set_permissions(&config, fs::Permissions::from_mode(0o600))?;
        }
        let config = config.to_str().ok_or_else(|| io::Error::other("the settings folder's name is not text"))?;
        let answer = supplicant.global(&format!("INTERFACE_ADD {name}\t{config}\tnl80211\t{INTERFACE_SOCKETS}"))?;
        if answer.trim() == "OK" { Ok(()) } else { Err(io::Error::other(format!("wpa_supplicant did not take {name}"))) }
    }

    fn detach(&self, supplicant: &mut dyn Supplicant, name: &str) -> io::Result<()> {
        if supplicant.global("INTERFACES")?.lines().any(|line| line == name) {
            supplicant.global(&format!("INTERFACE_REMOVE {name}"))?;
        }
        Ok(())
    }

    /// A command that answers "OK".
    fn done(supplicant: &mut dyn Supplicant, name: &str, command: &str) -> Result<(), String> {
        match supplicant.interface(name, command) {
            Ok(answer) if answer.trim() == "OK" => Ok(()),
            Ok(answer) => Err(format!("failed wpa_supplicant answered {:?} to {}", answer.trim(), command.split(' ').next().unwrap_or(""))),
            Err(error) => Err(format!("failed {error}")),
        }
    }

    fn connect(&self, supplicant: &mut dyn Supplicant, name: &str, ssid: &[u8], security: Security, secret: &[u8], hidden: bool) -> Result<u32, String> {
        let id = supplicant.interface(name, "ADD_NETWORK").map_err(|error| format!("failed {error}"))?;
        let id = network_id(id.trim()).ok_or("failed wpa_supplicant did not make the network")?;
        // A secret between quotes is a passphrase; 64 digits are the key.
        let quoted = |secret: &[u8]| format!("\"{}\"", String::from_utf8_lossy(secret));
        let mut settings = vec![format!("ssid {}", hex(ssid))];
        match security {
            Security::Open => settings.push("key_mgmt NONE".into()),
            Security::Psk => {
                settings.push("key_mgmt WPA-PSK WPA-PSK-SHA256".into());
                settings.push(format!("psk {}", if secret.len() == 64 { String::from_utf8_lossy(secret).into_owned() } else { quoted(secret) }));
            }
            Security::Sae => {
                settings.push("key_mgmt SAE".into());
                settings.push(format!("sae_password {}", quoted(secret)));
                // WPA3 protects management frames, always.
                settings.push("ieee80211w 2".into());
            }
            Security::PskSae => {
                settings.push("key_mgmt WPA-PSK WPA-PSK-SHA256 SAE".into());
                settings.push(format!("psk {}", if secret.len() == 64 { String::from_utf8_lossy(secret).into_owned() } else { quoted(secret) }));
                settings.push("ieee80211w 1".into());
            }
        }
        if hidden {
            settings.push("scan_ssid 1".into());
        }
        let applied = settings.iter().try_for_each(|setting| Self::done(supplicant, name, &format!("SET_NETWORK {id} {setting}")));
        if let Err(why) = applied.and_then(|()| self.select(supplicant, name, id, true)) {
            let _ = supplicant.interface(name, &format!("REMOVE_NETWORK {id}"));
            // Never the command: it may hold the password.
            return Err(why.split(" to ").next().unwrap_or("failed").to_owned());
        }
        Ok(id)
    }

    /// Tries a network now: it gets the highest priority, so that
    /// wpa_supplicant, told to look again, joins it rather than another it
    /// knows. (Its own "select" would turn the others off instead, and
    /// they would be saved so.) `keep`: the choice is saved; a network
    /// that is not joined on its own stays so in the file.
    fn select(&self, supplicant: &mut dyn Supplicant, name: &str, id: u32, keep: bool) -> Result<(), String> {
        let list = supplicant.interface(name, "LIST_NETWORKS").unwrap_or_default();
        let highest = saved_networks(&list)
            .iter()
            .filter(|(other, _)| *other != id)
            .filter_map(|(other, _)| supplicant.interface(name, &format!("GET_NETWORK {other} priority")).ok()?.trim().parse::<u32>().ok())
            .max()
            .unwrap_or(0);
        // Priorities are small numbers that grow by one each time: a
        // number near the limit means a file from elsewhere.
        let priority = highest.saturating_add(1).min(i32::MAX as u32);
        Self::done(supplicant, name, &format!("SET_NETWORK {id} priority {priority}"))?;
        Self::done(supplicant, name, &format!("ENABLE_NETWORK {id} no-connect"))?;
        if keep {
            let _ = supplicant.interface(name, "SAVE_CONFIG");
        }
        Self::done(supplicant, name, "REASSOCIATE")
    }

    /// The answer to a request line.
    pub fn answer(&self, line: &str, supplicant: &mut dyn Supplicant) -> String {
        let Some(request) = Request::parse(line) else { return "invalid".into() };
        let Some(name) = self.interface() else { return "unavailable".into() };
        if let Request::Enable(enabled) = request {
            let changed = self.set_enabled(enabled).and_then(|()| if enabled { self.attach(supplicant, &name) } else { self.detach(supplicant, &name) });
            return changed.map_or_else(|error| format!("failed {error}"), |()| "ok".into());
        }
        let enabled = self.enabled();
        if enabled {
            if let Err(error) = self.attach(supplicant, &name) {
                return format!("failed {error}");
            }
        }
        let text = |supplicant: &mut dyn Supplicant, command: &str| if enabled { supplicant.interface(&name, command).unwrap_or_default() } else { String::new() };
        let done = |supplicant: &mut dyn Supplicant, command: &str| {
            if !enabled {
                return "failed Wi-Fi is off".to_owned();
            }
            Self::done(supplicant, &name, command).map_or_else(|why| why, |()| "ok".into())
        };
        match request {
            Request::Enable(_) => unreachable!(),
            Request::Status => {
                let mut out = format!("wifi {{\"enabled\":{enabled},\"interface\":");
                json_string(&mut out, &name);
                out.push_str(",\"status\":");
                json_string(&mut out, &text(supplicant, "STATUS"));
                out.push_str(",\"signal\":");
                json_string(&mut out, &text(supplicant, "SIGNAL_POLL"));
                out.push('}');
                out
            }
            Request::Networks => {
                let mut out = String::from("wifi {\"scan\":");
                json_string(&mut out, &text(supplicant, "SCAN_RESULTS"));
                out.push_str(",\"saved\":");
                json_string(&mut out, &text(supplicant, "LIST_NETWORKS"));
                out.push('}');
                out
            }
            Request::Scan => {
                if !enabled {
                    return "failed Wi-Fi is off".into();
                }
                match supplicant.interface(&name, "SCAN") {
                    Ok(answer) if answer.trim() == "OK" => "ok".into(),
                    Ok(answer) if answer.contains("BUSY") => "busy".into(),
                    Ok(answer) => format!("failed {}", answer.trim()),
                    Err(error) => format!("failed {error}"),
                }
            }
            Request::Connect { ssid, security, secret, hidden } => {
                if !enabled {
                    return "failed Wi-Fi is off".into();
                }
                match self.connect(supplicant, &name, &ssid, security, &secret, hidden) {
                    Ok(id) => format!("ok {id}"),
                    Err(why) => why,
                }
            }
            Request::Select(id) => {
                if !enabled {
                    return "failed Wi-Fi is off".into();
                }
                // One that is not joined on its own is tried this once.
                let list = supplicant.interface(&name, "LIST_NETWORKS").unwrap_or_default();
                let manual = saved_networks(&list).iter().any(|(saved, disabled)| *saved == id && *disabled);
                self.select(supplicant, &name, id, !manual).map_or_else(|why| why, |()| "ok".into())
            }
            Request::Forget(id) => {
                let answer = done(supplicant, &format!("REMOVE_NETWORK {id}"));
                let _ = supplicant.interface(&name, "SAVE_CONFIG");
                answer
            }
            Request::Autoconnect(id, on) => {
                // `disabled` in the file is what keeps a network from being
                // joined on its own.
                let answer = done(supplicant, &format!("SET_NETWORK {id} disabled {}", u8::from(!on)));
                let _ = supplicant.interface(&name, "SAVE_CONFIG");
                answer
            }
            Request::Disconnect => done(supplicant, "DISCONNECT"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wpa_supplicant that records what it is asked.
    #[derive(Default)]
    struct Fake {
        attached: Vec<String>,
        asked: Vec<String>,
        fail: Option<&'static str>,
    }

    impl Supplicant for Fake {
        fn global(&mut self, command: &str) -> io::Result<String> {
            self.asked.push(format!("global {command}"));
            let mut words = command.split('\t');
            match words.next().unwrap_or("").split(' ').collect::<Vec<_>>().as_slice() {
                ["INTERFACES"] => Ok(self.attached.join("\n")),
                ["INTERFACE_ADD", name] => {
                    self.attached.push((*name).to_owned());
                    Ok("OK\n".into())
                }
                ["INTERFACE_REMOVE", name] => {
                    self.attached.retain(|attached| attached != name);
                    Ok("OK\n".into())
                }
                _ => Ok("FAIL\n".into()),
            }
        }

        fn interface(&mut self, name: &str, command: &str) -> io::Result<String> {
            self.asked.push(format!("{name} {command}"));
            if self.fail.is_some_and(|fail| command.starts_with(fail)) {
                return Ok("FAIL\n".into());
            }
            Ok(match command {
                "ADD_NETWORK" => "3\n".into(),
                "GET_NETWORK 0 priority" => "5\n".into(),
                "GET_NETWORK 1 priority" => "2\n".into(),
                "GET_NETWORK 3 priority" => "9\n".into(),
                "STATUS" => "wpa_state=COMPLETED\nssid=caf\\xc3\\xa9 \"x\"\n".into(),
                "SIGNAL_POLL" => "RSSI=-61\n".into(),
                "SCAN_RESULTS" => "bssid / frequency / signal level / flags / ssid\n02:00:00:00:02:00\t5180\t-61\t[WPA2-PSK-CCMP][ESS]\tnet\n".into(),
                "LIST_NETWORKS" => "network id / ssid / bssid / flags\n0\thome\tany\t\n1\tcafe\tany\t[DISABLED]\n3\tnet\tany\t[CURRENT]\n".into(),
                _ => "OK\n".into(),
            })
        }
    }

    fn wifi(folder: &Path) -> Wifi {
        fs::create_dir_all(folder).unwrap();
        let dev = folder.join("dev");
        fs::write(&dev, "Inter-|   Receive\n face |bytes\n    lo:  10 1\n  eth0:  10 1\n wlan0:  10 1\n").unwrap();
        Wifi { settings: Some(folder.join("system")), proc_net_dev: dev }
    }

    fn folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("openshell-wifi-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        folder
    }

    #[test]
    fn requests_are_checked_word_by_word() {
        assert_eq!(Request::parse("wifi status"), Some(Request::Status));
        assert_eq!(Request::parse("wifi select 12"), Some(Request::Select(12)));
        assert_eq!(Request::parse("wifi autoconnect 0 off"), Some(Request::Autoconnect(0, false)));
        assert_eq!(Request::parse("wifi enable on"), Some(Request::Enable(true)));
        assert_eq!(
            Request::parse("wifi connect 6e6574 psk 70617373776f7264 hidden"),
            Some(Request::Connect { ssid: b"net".to_vec(), security: Security::Psk, secret: b"password".to_vec(), hidden: true })
        );
        assert_eq!(
            Request::parse("wifi connect 6e6574 open"),
            Some(Request::Connect { ssid: b"net".to_vec(), security: Security::Open, secret: Vec::new(), hidden: false })
        );
        let key = "ab".repeat(32);
        assert!(Request::parse(&format!("wifi connect 6e6574 psk {}", hex(key.as_bytes()))).is_some());
        for bad in [
            "wifi",
            "wifi ",
            "wifi status now",
            "wifi select",
            "wifi select -1",
            "wifi select 1 2",
            "wifi select 1234567",
            "wifi forget 1\tSET x",
            "wifi autoconnect 1 maybe",
            "wifi enable",
            // No name, a name of 33 bytes, digits that are not hexadecimal.
            "wifi connect  open",
            "wifi connect 6e6574",
            &format!("wifi connect {} open", "61".repeat(33)),
            "wifi connect 6e657 open",
            "wifi connect zz open",
            "wifi connect 6e6574 wep 6162636465",
            // A password where none belongs, one too short, one with a
            // line break (0a), a second one.
            "wifi connect 6e6574 open 70617373776f7264",
            "wifi connect 6e6574 psk",
            "wifi connect 6e6574 psk 61626364656667",
            "wifi connect 6e6574 psk 70617373776f72640a70736b3d78",
            "wifi connect 6e6574 psk 70617373776f7264 70617373776f7264",
            "wifi connect 6e6574 psk 70617373776f7264 hidden hidden",
            "wifi connect 6e6574 sae",
        ] {
            assert_eq!(Request::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn finds_the_wifi_interfaces() {
        let text = "Inter-|   Receive\n face |bytes\n    lo: 1\n wlan0: 2\nwlp2s0: 3\n  eth0: 4\n wl/x: 5\n";
        assert_eq!(wifi_interfaces(text), ["wlan0", "wlp2s0"]);
        assert!(wifi_interfaces("").is_empty());
    }

    #[test]
    fn without_an_interface_nothing_is_available() {
        let folder = folder("none");
        let mut wifi = wifi(&folder);
        fs::write(&wifi.proc_net_dev, "    lo: 1\n  eth0: 2\n").unwrap();
        let mut fake = Fake::default();
        assert_eq!(wifi.answer("wifi status", &mut fake), "unavailable");
        assert_eq!(wifi.answer("wifi bogus", &mut fake), "invalid");
        assert!(fake.asked.is_empty());
        wifi.proc_net_dev = folder.join("missing");
        assert_eq!(wifi.answer("wifi scan", &mut fake), "unavailable");
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn the_interface_is_handed_over_once_with_roots_file() {
        let folder = folder("attach");
        let wifi = wifi(&folder);
        let mut fake = Fake::default();
        let status = wifi.answer("wifi status", &mut fake);
        assert_eq!(
            status,
            "wifi {\"enabled\":true,\"interface\":\"wlan0\",\"status\":\"wpa_state=COMPLETED\\u000assid=caf\\\\xc3\\\\xa9 \\\"x\\\"\\u000a\",\"signal\":\"RSSI=-61\\u000a\"}"
        );
        let config = folder.join("system").join(FOLDER).join(CONFIG_FILE);
        assert_eq!(fs::read_to_string(&config).unwrap(), "update_config=1\n");
        assert_eq!(fs::metadata(&config).unwrap().permissions().mode() & 0o777, 0o600);
        // Only root enters the folder: wpa_supplicant rewrites the file
        // with its own permissions.
        assert_eq!(fs::metadata(config.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fake.asked[1], format!("global INTERFACE_ADD wlan0\t{}\tnl80211\t{INTERFACE_SOCKETS}", config.display()));
        // The second request finds it there.
        fake.asked.clear();
        assert!(wifi.answer("wifi networks", &mut fake).starts_with("wifi {\"scan\":\"bssid / frequency"));
        assert_eq!(fake.asked, ["global INTERFACES", "wlan0 SCAN_RESULTS", "wlan0 LIST_NETWORKS"]);
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn joins_a_network_and_keeps_it() {
        let folder = folder("connect");
        let wifi = wifi(&folder);
        let mut fake = Fake { attached: vec!["wlan0".into()], ..Fake::default() };
        assert_eq!(wifi.answer("wifi connect 6e6574 psk+sae 70617373776f7264 hidden", &mut fake), "ok 3");
        assert_eq!(
            fake.asked[1..],
            [
                "wlan0 ADD_NETWORK",
                "wlan0 SET_NETWORK 3 ssid 6e6574",
                "wlan0 SET_NETWORK 3 key_mgmt WPA-PSK WPA-PSK-SHA256 SAE",
                "wlan0 SET_NETWORK 3 psk \"password\"",
                "wlan0 SET_NETWORK 3 ieee80211w 1",
                "wlan0 SET_NETWORK 3 scan_ssid 1",
                // Above the others (5 and 2) from now on, saved so, and
                // looked for at once.
                "wlan0 LIST_NETWORKS",
                "wlan0 GET_NETWORK 0 priority",
                "wlan0 GET_NETWORK 1 priority",
                "wlan0 SET_NETWORK 3 priority 6",
                "wlan0 ENABLE_NETWORK 3 no-connect",
                "wlan0 SAVE_CONFIG",
                "wlan0 REASSOCIATE",
            ]
        );
        fake.asked.clear();
        assert_eq!(wifi.answer("wifi connect 6e6574 open", &mut fake), "ok 3");
        assert!(fake.asked.contains(&"wlan0 SET_NETWORK 3 key_mgmt NONE".to_owned()));
        // A network wpa_supplicant refuses is not left behind, and the
        // answer does not repeat the password.
        fake.asked.clear();
        fake.fail = Some("SET_NETWORK 3 psk");
        let answer = wifi.answer("wifi connect 6e6574 psk 70617373776f7264", &mut fake);
        assert!(answer.starts_with("failed "), "{answer}");
        assert!(!answer.contains("password"), "{answer}");
        assert_eq!(fake.asked.last().unwrap(), "wlan0 REMOVE_NETWORK 3");
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn the_radio_stays_off_until_turned_on() {
        let folder = folder("enable");
        let wifi = wifi(&folder);
        let mut fake = Fake { attached: vec!["wlan0".into()], ..Fake::default() };
        assert_eq!(wifi.answer("wifi enable off", &mut fake), "ok");
        assert!(fake.attached.is_empty());
        assert!(folder.join("system").join(FOLDER).join(DISABLED_MARK).exists());
        // Off: nothing is handed over, nothing is asked of the interface.
        fake.asked.clear();
        assert_eq!(wifi.answer("wifi status", &mut fake), "wifi {\"enabled\":false,\"interface\":\"wlan0\",\"status\":\"\",\"signal\":\"\"}");
        assert_eq!(wifi.answer("wifi scan", &mut fake), "failed Wi-Fi is off");
        assert_eq!(wifi.answer("wifi select 1", &mut fake), "failed Wi-Fi is off");
        assert!(fake.asked.is_empty());
        assert_eq!(wifi.answer("wifi enable on", &mut fake), "ok");
        assert_eq!(fake.attached, ["wlan0"]);
        assert!(!folder.join("system").join(FOLDER).join(DISABLED_MARK).exists());
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn saved_networks_change_through_their_numbers() {
        let folder = folder("saved");
        let wifi = wifi(&folder);
        let mut fake = Fake { attached: vec!["wlan0".into()], ..Fake::default() };
        assert_eq!(wifi.answer("wifi autoconnect 3 off", &mut fake), "ok");
        assert_eq!(fake.asked[1..], ["wlan0 SET_NETWORK 3 disabled 1", "wlan0 SAVE_CONFIG"]);
        fake.asked.clear();
        assert_eq!(wifi.answer("wifi forget 3", &mut fake), "ok");
        assert_eq!(fake.asked[1..], ["wlan0 REMOVE_NETWORK 3", "wlan0 SAVE_CONFIG"]);
        fake.asked.clear();
        assert_eq!(wifi.answer("wifi select 0", &mut fake), "ok");
        assert_eq!(
            fake.asked[1..],
[
                "wlan0 LIST_NETWORKS",
                "wlan0 LIST_NETWORKS",
                "wlan0 GET_NETWORK 1 priority",
                "wlan0 GET_NETWORK 3 priority",
                "wlan0 SET_NETWORK 0 priority 10",
                "wlan0 ENABLE_NETWORK 0 no-connect",
                "wlan0 SAVE_CONFIG",
                "wlan0 REASSOCIATE",
            ]
        );
        // One turned off by hand is tried this once: the file keeps it off.
        fake.asked.clear();
        assert_eq!(wifi.answer("wifi select 1", &mut fake), "ok");
        assert_eq!(fake.asked[3..], ["wlan0 GET_NETWORK 0 priority", "wlan0 GET_NETWORK 3 priority", "wlan0 SET_NETWORK 1 priority 10", "wlan0 ENABLE_NETWORK 1 no-connect", "wlan0 REASSOCIATE"]);
        assert_eq!(saved_networks("network id / ssid / bssid / flags\n7\tx\tany\t[TEMP-DISABLED]\nbad\n"), [(7, false)]);
        fake.fail = Some("DISCONNECT");
        assert_eq!(wifi.answer("wifi disconnect", &mut fake), "failed wpa_supplicant answered \"FAIL\" to DISCONNECT");
        let _ = fs::remove_dir_all(folder);
    }
}
