//! The hotspot (Settings ▸ Network ▸ Hotspot), as NetworkManager's shared
//! Wi-Fi gives it on OpenShell Live: the Wi-Fi interface becomes an access
//! point with a name and a WPA2 password, and the devices that join it reach
//! the internet through this computer's other connection. On OpenShell Native
//! hostapd runs the network (as root, started here) and the kernel does the
//! rest once the interface runs one: its address (10.42.0.1/24), a DHCP
//! server for the devices and the sharing itself (`aster-bigtcp`'s `nat`).
//! The supervisor's requests (`wifi.rs` routes them):
//!
//! ```text
//! wifi hotspot                     → "wifi <json>": active, the interface,
//!                                    the saved name, password and band, the
//!                                    devices joined
//! wifi hotspot-start <ssid> <password> [a|bg]
//!                                  → "ok", or "failed <why>"
//! wifi hotspot-stop                → "ok"
//! ```
//!
//! `<ssid>` and `<password>` are hexadecimal; the band is 5 GHz (`a`) or
//! 2.4 GHz (`bg`, the default). While the hotspot runs, wpa_supplicant does
//! not have the interface; it gets it back when the hotspot stops. The name,
//! password and band are kept in the Wi-Fi folder only root enters, the
//! configuration hostapd reads in /run.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// hostapd as the image ships it, and where it keeps its state.
pub const HOSTAPD: &str = "/opt/openshell-compat/bin/hostapd";
const RUN_FOLDER: &str = "/run/openshell-wifi";
const CONTROL_FOLDER: &str = "/run/openshell-wifi/hostapd";
pub const CONFIG: &str = "/run/openshell-wifi/hostapd.conf";
const PID_FILE: &str = "/run/openshell-wifi/hostapd.pid";
/// The file of the saved name, password and band, in the Wi-Fi folder.
pub const SETTINGS_FILE: &str = "hotspot";
/// How long hostapd gets to start a network.
const START_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// The band a hotspot runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Band {
    /// 2.4 GHz.
    Bg,
    /// 5 GHz.
    A,
}

impl Band {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "bg" => Some(Band::Bg),
            "a" => Some(Band::A),
            _ => None,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Band::Bg => "bg",
            Band::A => "a",
        }
    }
}

/// A name of 1 to 32 bytes and a WPA2 passphrase (8 to 63 printable
/// characters), as NetworkManager takes them.
pub fn valid(ssid: &[u8], password: &[u8]) -> bool {
    (1..=32).contains(&ssid.len()) && (8..=63).contains(&password.len()) && password.iter().all(|byte| (0x20..=0x7e).contains(byte))
}

/// The saved hotspot: its name, password and band.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub ssid: Vec<u8>,
    pub password: Vec<u8>,
    pub band: Band,
}

impl Settings {
    pub fn parse(text: &str) -> Option<Self> {
        let (mut ssid, mut password, mut band) = (None, None, Band::Bg);
        for line in text.lines() {
            match line.split_once(' ') {
                Some(("ssid", value)) => ssid = super::wifi::unhex(value),
                Some(("password", value)) => password = super::wifi::unhex(value),
                Some(("band", value)) => band = Band::parse(value)?,
                _ => {}
            }
        }
        let settings = Settings { ssid: ssid?, password: password?, band };
        valid(&settings.ssid, &settings.password).then_some(settings)
    }

    pub fn text(&self) -> String {
        format!("ssid {}\npassword {}\nband {}\n", super::wifi::hex(&self.ssid), super::wifi::hex(&self.password), self.band.word())
    }
}

/// hostapd's configuration for `settings` on `interface`. `wide`: as wide
/// as the band goes (HT, and VHT and HE on 5 GHz at 80 MHz); else the plain
/// 20 MHz every Wi-Fi chip that runs an access point takes.
pub fn config(interface: &str, settings: &Settings, wide: bool) -> String {
    let mut text = String::new();
    let _ = writeln!(text, "interface={interface}");
    text.push_str("driver=nl80211\n");
    let _ = writeln!(text, "ctrl_interface={CONTROL_FOLDER}");
    // The name as its bytes, whatever they are.
    let _ = writeln!(text, "ssid2={}", super::wifi::hex(&settings.ssid));
    text.push_str("utf8_ssid=1\n");
    match settings.band {
        Band::Bg => text.push_str("hw_mode=g\nchannel=6\n"),
        Band::A => text.push_str("hw_mode=a\nchannel=36\n"),
    }
    text.push_str("wmm_enabled=1\nieee80211n=1\n");
    if wide && settings.band == Band::A {
        // Channels 36 to 48 as one 80 MHz channel, centered on 42.
        text.push_str("ht_capab=[HT40+][SHORT-GI-20][SHORT-GI-40]\n");
        text.push_str("ieee80211ac=1\nvht_oper_chwidth=1\nvht_oper_centr_freq_seg0_idx=42\n");
        text.push_str("ieee80211ax=1\nhe_oper_chwidth=1\nhe_oper_centr_freq_seg0_idx=42\n");
        // No scan of the neighbours first: the radio stays on its channel.
        text.push_str("noscan=1\n");
    }
    text.push_str("auth_algs=1\nwpa=2\nwpa_key_mgmt=WPA-PSK\nrsn_pairwise=CCMP\n");
    // The passphrase is the rest of its line, as hostapd reads it (printable
    // characters only: `valid` checked them).
    let _ = writeln!(text, "wpa_passphrase={}", String::from_utf8_lossy(&settings.password));
    text
}

/// Running hostapd.
pub trait Hostapd {
    /// Starts it with this configuration, in the background.
    fn start(&mut self, config: &Path) -> io::Result<()>;
    /// Ends it, if it runs.
    fn stop(&mut self);
    /// Its answer to STATUS for `interface`, None when it does not run.
    fn status(&mut self, interface: &str) -> Option<String>;
    /// The devices joined (its `STA-FIRST` and `STA-NEXT` answers).
    fn stations(&mut self, interface: &str) -> usize;
}

extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

/// The hostapd of the image, as root.
pub struct Daemon;

impl Daemon {
    fn pid() -> Option<i32> {
        fs::read_to_string(PID_FILE).ok()?.trim().parse().ok().filter(|pid| *pid > 1)
    }

    fn alive(pid: i32) -> bool {
        // Signal 0 checks the process is there.
        unsafe { kill(pid, 0) == 0 }
    }
}

impl Hostapd for Daemon {
    fn start(&mut self, config: &Path) -> io::Result<()> {
        let status = Command::new(HOSTAPD).arg("-B").arg("-P").arg(PID_FILE).arg(config).status()?;
        if status.success() { Ok(()) } else { Err(io::Error::other("hostapd did not start")) }
    }

    fn stop(&mut self) {
        let Some(pid) = Self::pid() else { return };
        unsafe {
            kill(pid, SIGTERM);
        }
        let deadline = Instant::now() + STOP_TIMEOUT;
        while Self::alive(pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
        if Self::alive(pid) {
            unsafe {
                kill(pid, SIGKILL);
            }
        }
        let _ = fs::remove_file(PID_FILE);
    }

    fn status(&mut self, interface: &str) -> Option<String> {
        let pid = Self::pid()?;
        if !Self::alive(pid) {
            return None;
        }
        super::wifi::ask(&Path::new(CONTROL_FOLDER).join(interface), "STATUS").ok()
    }

    fn stations(&mut self, interface: &str) -> usize {
        let socket = Path::new(CONTROL_FOLDER).join(interface);
        let mut count = 0;
        let mut answer = super::wifi::ask(&socket, "STA-FIRST").unwrap_or_default();
        while let Some(address) = answer.lines().next().filter(|line| line.len() == 17).map(str::to_owned) {
            count += 1;
            if count > 255 {
                break;
            }
            answer = super::wifi::ask(&socket, &format!("STA-NEXT {address}")).unwrap_or_default();
        }
        count
    }
}

/// Whether hostapd's STATUS says the network runs.
fn enabled(status: &str) -> bool {
    status.lines().any(|line| line == "state=ENABLED")
}

/// The hotspot's side of the supervisor.
pub struct Hotspot<'a> {
    /// The Wi-Fi folder (root's alone), where the settings are kept.
    pub folder: &'a Path,
    /// Where hostapd's configuration is written.
    pub config: PathBuf,
}

impl<'a> Hotspot<'a> {
    pub fn new(folder: &'a Path) -> Self {
        Self { folder, config: PathBuf::from(CONFIG) }
    }

    pub fn settings(&self) -> Option<Settings> {
        Settings::parse(&fs::read_to_string(self.folder.join(SETTINGS_FILE)).ok()?)
    }

    pub fn active(&self, hostapd: &mut dyn Hostapd, interface: &str) -> bool {
        hostapd.status(interface).is_some_and(|status| enabled(&status))
    }

    /// The answer to `wifi hotspot`.
    pub fn status(&self, hostapd: &mut dyn Hostapd, interface: &str) -> String {
        let active = self.active(hostapd, interface);
        let mut out = format!("wifi {{\"active\":{active},\"interface\":\"{interface}\"");
        if let Some(settings) = self.settings() {
            let _ = write!(
                out,
                ",\"ssid\":\"{}\",\"password\":\"{}\",\"band\":\"{}\"",
                super::wifi::hex(&settings.ssid),
                super::wifi::hex(&settings.password),
                settings.band.word()
            );
        }
        if active {
            let _ = write!(out, ",\"stations\":{}", hostapd.stations(interface));
        }
        out.push('}');
        out
    }

    fn write_config(&self, text: &str) -> io::Result<()> {
        if let Some(folder) = self.config.parent() {
            fs::create_dir_all(folder)?;
            fs::set_permissions(folder, fs::Permissions::from_mode(0o700))?;
        }
        // The file holds the password.
        fs::write(&self.config, b"")?;
        fs::set_permissions(&self.config, fs::Permissions::from_mode(0o600))?;
        fs::write(&self.config, text)
    }

    /// Starts the hotspot: the settings are saved, then hostapd runs the
    /// widest network the radio takes, else a plain one. The caller has taken
    /// the interface from wpa_supplicant.
    pub fn start(&self, hostapd: &mut dyn Hostapd, interface: &str, settings: &Settings, wait: Duration) -> Result<(), String> {
        let saved = self.folder.join(SETTINGS_FILE);
        fs::write(&saved, b"").and_then(|()| fs::set_permissions(&saved, fs::Permissions::from_mode(0o600))).and_then(|()| fs::write(&saved, settings.text())).map_err(|error| format!("failed {error}"))?;
        hostapd.stop();
        for wide in [true, false] {
            self.write_config(&config(interface, settings, wide)).map_err(|error| format!("failed {error}"))?;
            if hostapd.start(&self.config).is_err() {
                continue;
            }
            let deadline = Instant::now() + wait;
            loop {
                if self.active(hostapd, interface) {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(200));
            }
            hostapd.stop();
            if settings.band == Band::Bg && wide {
                // 2.4 GHz is the same both ways.
                break;
            }
        }
        let _ = fs::remove_file(&self.config);
        Err(match settings.band {
            Band::A => "failed this Wi-Fi adapter cannot run a hotspot on 5 GHz here".into(),
            Band::Bg => "failed this Wi-Fi adapter cannot run a hotspot".into(),
        })
    }

    pub fn stop(&self, hostapd: &mut dyn Hostapd) {
        hostapd.stop();
        let _ = fs::remove_file(&self.config);
    }

    /// How long a start waits for hostapd.
    pub fn start_timeout() -> Duration {
        START_TIMEOUT
    }

    pub fn run_folder() -> &'static str {
        RUN_FOLDER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hostapd that runs the network on the configuration's first try or
    /// only on the plain one.
    #[derive(Default)]
    struct Fake {
        running: bool,
        configs: Vec<String>,
        only_plain: bool,
        stops: usize,
    }

    impl Hostapd for Fake {
        fn start(&mut self, config: &Path) -> io::Result<()> {
            let text = fs::read_to_string(config)?;
            self.running = !(self.only_plain && text.contains("ieee80211ac=1"));
            self.configs.push(text);
            Ok(())
        }

        fn stop(&mut self) {
            self.running = false;
            self.stops += 1;
        }

        fn status(&mut self, _: &str) -> Option<String> {
            self.running.then(|| "state=ENABLED\n".into())
        }

        fn stations(&mut self, _: &str) -> usize {
            2
        }
    }

    fn folder(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("openshell-hotspot-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn settings_round_trip_and_are_checked() {
        let settings = Settings { ssid: "Café net".as_bytes().to_vec(), password: b"pass word!".to_vec(), band: Band::A };
        assert_eq!(Settings::parse(&settings.text()), Some(settings));
        assert_eq!(Settings::parse("ssid 6e6574\npassword 73686f7274\n"), None);
        assert!(valid(b"x", b"12345678"));
        assert!(!valid(b"", b"12345678"));
        assert!(!valid(b"x", b"1234567"));
        assert!(!valid(b"x", b"12345678\n"));
    }

    #[test]
    fn the_configuration_names_the_network_and_its_band() {
        let settings = Settings { ssid: b"net".to_vec(), password: b"password".to_vec(), band: Band::A };
        let wide = config("wlan0", &settings, true);
        assert!(wide.contains("interface=wlan0\n"));
        assert!(wide.contains("ssid2=6e6574\n"));
        assert!(wide.contains("hw_mode=a\nchannel=36\n"));
        assert!(wide.contains("vht_oper_centr_freq_seg0_idx=42\n"));
        assert!(wide.ends_with("wpa_passphrase=password\n"));
        let plain = config("wlan0", &settings, false);
        assert!(!plain.contains("ieee80211ac"));
        let bg = config("wlan0", &Settings { band: Band::Bg, ..settings }, true);
        assert!(bg.contains("hw_mode=g\nchannel=6\n") && !bg.contains("ieee80211ac"));
    }

    #[test]
    fn starts_wide_else_plain_and_keeps_the_password_root_s() {
        let folder = folder("start");
        let mut hotspot = Hotspot::new(&folder);
        hotspot.config = folder.join("run").join("hostapd.conf");
        let settings = Settings { ssid: b"net".to_vec(), password: b"password".to_vec(), band: Band::A };
        let mut fake = Fake { only_plain: true, ..Fake::default() };
        assert_eq!(hotspot.start(&mut fake, "wlan0", &settings, Duration::from_millis(1)), Ok(()));
        assert_eq!(fake.configs.len(), 2);
        assert!(!fake.configs[1].contains("ieee80211ac"));
        assert_eq!(fs::metadata(&hotspot.config).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(folder.join(SETTINGS_FILE)).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(hotspot.settings(), Some(settings));
        let status = hotspot.status(&mut fake, "wlan0");
        assert_eq!(status, "wifi {\"active\":true,\"interface\":\"wlan0\",\"ssid\":\"6e6574\",\"password\":\"70617373776f7264\",\"band\":\"a\",\"stations\":2}");
        hotspot.stop(&mut fake);
        assert!(!hotspot.config.exists());
        assert_eq!(hotspot.status(&mut fake, "wlan0"), "wifi {\"active\":false,\"interface\":\"wlan0\",\"ssid\":\"6e6574\",\"password\":\"70617373776f7264\",\"band\":\"a\"}");
        let _ = fs::remove_dir_all(folder);
    }

    #[test]
    fn a_network_that_never_runs_is_a_failure() {
        let folder = folder("fail");
        let mut hotspot = Hotspot::new(&folder);
        hotspot.config = folder.join("run").join("hostapd.conf");
        struct Never;
        impl Hostapd for Never {
            fn start(&mut self, _: &Path) -> io::Result<()> {
                Ok(())
            }
            fn stop(&mut self) {}
            fn status(&mut self, _: &str) -> Option<String> {
                Some("state=DISABLED\n".into())
            }
            fn stations(&mut self, _: &str) -> usize {
                0
            }
        }
        let settings = Settings { ssid: b"net".to_vec(), password: b"password".to_vec(), band: Band::Bg };
        assert_eq!(hotspot.start(&mut Never, "wlan0", &settings, Duration::from_millis(1)), Err("failed this Wi-Fi adapter cannot run a hotspot".into()));
        assert!(!hotspot.config.exists());
        let _ = fs::remove_dir_all(folder);
    }
}
