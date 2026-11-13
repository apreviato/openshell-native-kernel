//! The session of OpenShell Native: the configuration of the supervisor and
//! the policies it applies, kept apart from the process handling so that they
//! can be tested.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const DESKTOP_HEALTHY: &str = "OPENSHELL_DESKTOP_HEALTHY";
pub const DESKTOP_HEALTH_RESET: &str = "OPENSHELL_DESKTOP_HEALTH_RESET";
pub const DESKTOP_HEALTH_ENV: &str = "OPENSHELL_NATIVE_HEALTH";
const HEALTH_EXPIRY: Duration = Duration::from_secs(15);

/// A command line of the configuration: words separated by spaces, without
/// quoting. System paths of the image have no spaces.
pub type CommandLine = Vec<String>;

#[derive(Debug, PartialEq, Eq)]
pub struct SessionConfig {
    pub uid: u32,
    pub gid: u32,
    pub display: String,
    pub home: PathBuf,
    pub log_dir: PathBuf,
    pub x_server: CommandLine,
    pub window_manager: Option<CommandLine>,
    pub shell: CommandLine,
    pub env: Vec<(String, String)>,
    /// Run once per boot, as root, when the session is healthy: it confirms
    /// the release the computer booted (`openshell-ab <disk> confirm <slot>`).
    pub confirm: Option<CommandLine>,
    /// How long the Desktop reports health continuously before confirming.
    pub confirm_after: Duration,
}

/// By default the Desktop must report health continuously for a minute.
pub const DEFAULT_CONFIRM_AFTER: Duration = Duration::from_secs(60);

fn command(value: &str, key: &str) -> Result<CommandLine, String> {
    let words: CommandLine = value.split_whitespace().map(str::to_owned).collect();
    match words.first() {
        Some(program) if program.starts_with('/') => Ok(words),
        _ => Err(format!("{key} needs an absolute program path")),
    }
}

fn absolute(value: &str, key: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value);
    if path.is_absolute() && !value.contains("..") {
        Ok(path)
    } else {
        Err(format!("{key} needs an absolute path"))
    }
}

impl SessionConfig {
    /// Reads `key=value` lines. Empty lines and lines starting with `#` are
    /// ignored; `env` may repeat.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (mut user, mut display, mut home, mut log_dir) = (None, None, None, None);
        let (mut x_server, mut window_manager, mut shell) = (None, None, None);
        let (mut confirm, mut confirm_after) = (None, DEFAULT_CONFIRM_AFTER);
        let mut env = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .map(|(key, value)| (key.trim(), value.trim()))
                .ok_or_else(|| format!("line {}: expected key=value", number + 1))?;
            match key {
                "user" => {
                    let (uid, gid) = value.split_once(':').ok_or("user is uid:gid")?;
                    user = Some((
                        uid.parse::<u32>().map_err(|_| "invalid uid")?,
                        gid.parse::<u32>().map_err(|_| "invalid gid")?,
                    ));
                }
                "display" => {
                    let number = value.strip_prefix(':').ok_or("display is :N")?;
                    number.parse::<u16>().map_err(|_| "display is :N")?;
                    display = Some(value.to_owned());
                }
                "home" => home = Some(absolute(value, key)?),
                "log_dir" => log_dir = Some(absolute(value, key)?),
                "x" => x_server = Some(command(value, key)?),
                "wm" => window_manager = Some(command(value, key)?),
                "shell" => shell = Some(command(value, key)?),
                "confirm" => confirm = Some(command(value, key)?),
                "confirm_after" => {
                    let seconds = value.parse::<u64>().map_err(|_| "confirm_after is a number of seconds")?;
                    confirm_after = Duration::from_secs(seconds.max(1));
                }
                "env" => {
                    let (name, setting) = value.split_once('=').ok_or("env is NAME=value")?;
                    let reserved = ["DISPLAY", "HOME", "USER", "LOGNAME"];
                    if name.is_empty()
                        || reserved.contains(&name)
                        || !name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    {
                        return Err(format!("line {}: invalid env name", number + 1));
                    }
                    env.push((name.to_owned(), setting.to_owned()));
                }
                _ => return Err(format!("line {}: unknown key {key}", number + 1)),
            }
        }
        let (uid, gid) = user.ok_or("missing user")?;
        if uid == 0 {
            return Err("The session does not run as root".into());
        }
        Ok(Self {
            uid,
            gid,
            display: display.ok_or("missing display")?,
            home: home.ok_or("missing home")?,
            log_dir: log_dir.ok_or("missing log_dir")?,
            x_server: x_server.ok_or("missing x")?,
            window_manager,
            shell: shell.ok_or("missing shell")?,
            env,
            confirm,
            confirm_after,
        })
    }

    /// The X server's socket, which appears once it accepts clients.
    pub fn x_socket(&self) -> PathBuf {
        PathBuf::from(format!("/tmp/.X11-unix/X{}", &self.display[1..]))
    }
}

/// Continuous health from the current Desktop, with a deadline for each report.
pub struct HealthWatch {
    after: Duration,
    since: Option<Instant>,
    last: Option<Instant>,
}

impl HealthWatch {
    pub fn new(after: Duration) -> Self {
        Self { after, since: None, last: None }
    }

    pub fn reset(&mut self) {
        self.since = None;
        self.last = None;
    }

    pub fn report(&mut self, now: Instant) {
        if !self.fresh(now) {
            self.since = Some(now);
        }
        self.last = Some(now);
    }

    fn fresh(&self, now: Instant) -> bool {
        self.last.is_some_and(|last| now.duration_since(last) < HEALTH_EXPIRY)
    }

    pub fn healthy(&self, now: Instant) -> bool {
        self.fresh(now) && self.since.is_some_and(|since| now.duration_since(since) >= self.after)
    }
}

/// Restarts allowed within a window, with an exponential delay between them.
pub struct RestartBudget {
    limit: usize,
    window: Duration,
    restarts: Vec<Instant>,
}

impl RestartBudget {
    pub fn new(limit: usize, window: Duration) -> Self {
        Self { limit, window, restarts: Vec::new() }
    }

    /// The delay before the next restart, or None once the budget is spent.
    pub fn next_delay(&mut self, now: Instant) -> Option<Duration> {
        self.restarts.retain(|time| now.duration_since(*time) < self.window);
        if self.restarts.len() >= self.limit {
            return None;
        }
        self.restarts.push(now);
        Some(Duration::from_millis(500 << (self.restarts.len() - 1).min(4)))
    }
}

/// Creates the log directory of this boot, `NNNN` after the newest one, and
/// removes the oldest ones beyond `keep`.
pub fn boot_log_dir(root: &Path, keep: usize) -> io::Result<PathBuf> {
    fs::create_dir_all(root)?;
    let mut boots: Vec<u32> = fs::read_dir(root)?
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse::<u32>().ok())
        .collect();
    boots.sort_unstable();
    let next = boots.last().map_or(1, |last| last + 1);
    let directory = root.join(format!("{next:04}"));
    fs::create_dir(&directory)?;
    boots.push(next);
    let excess = boots.len().saturating_sub(keep.max(1));
    for old in &boots[..excess] {
        fs::remove_dir_all(root.join(format!("{old:04}")))?;
    }
    Ok(directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "\
# The kiosk of the image
user=1000:1000
display=:0
home=/var/lib/openshell/home
log_dir=/var/log/openshell
x=/usr/bin/Xvfb :0 -screen 0 1280x800x24 -nolisten tcp
wm=/usr/bin/openbox
shell=/usr/bin/openshell-shell /opt/openshell /opt/cef/openshell-cef-host /var/lib/openshell/state
env=FONTCONFIG_FILE=/etc/fonts/fonts.conf
env=PATH=/usr/bin:/bin
";

    #[test]
    fn parses_the_session_configuration() {
        let config = SessionConfig::parse(CONFIG).unwrap();
        assert_eq!((config.uid, config.gid), (1000, 1000));
        assert_eq!(config.x_server.len(), 7);
        assert_eq!(config.shell[0], "/usr/bin/openshell-shell");
        assert_eq!(config.x_socket(), PathBuf::from("/tmp/.X11-unix/X0"));
        assert_eq!(config.env[1], ("PATH".into(), "/usr/bin:/bin".into()));
    }

    #[test]
    fn rejects_unsafe_or_incomplete_configurations() {
        for (from, to) in [
            ("user=1000:1000", "user=0:0"),
            ("x=/usr/bin/Xvfb", "x=Xvfb"),
            ("home=/var/lib/openshell/home", "home=relative"),
            ("log_dir=/var/log/openshell", "log_dir=/var/log/../etc"),
            ("display=:0", "display=0"),
            ("env=PATH=", "env=HOME="),
            ("wm=/usr/bin/openbox", "unknown=1"),
            ("shell=/usr/bin/openshell-shell", "#"),
        ] {
            assert!(SessionConfig::parse(&CONFIG.replace(from, to)).is_err(), "{to}");
        }
    }

    #[test]
    fn reads_the_confirmation_of_the_boot() {
        let config = SessionConfig::parse(CONFIG).unwrap();
        assert_eq!((config.confirm, config.confirm_after), (None, DEFAULT_CONFIRM_AFTER));
        let text = format!("{CONFIG}confirm=/usr/bin/openshell-ab /var/lib/openshell/disk confirm a\nconfirm_after=30\n");
        let config = SessionConfig::parse(&text).unwrap();
        assert_eq!(config.confirm.unwrap()[2], "confirm");
        assert_eq!(config.confirm_after, Duration::from_secs(30));
        assert!(SessionConfig::parse(&format!("{CONFIG}confirm=openshell-ab\n")).is_err());
    }

    #[test]
    fn confirmation_requires_fresh_desktop_health_throughout_the_window() {
        let start = Instant::now();
        let time = |seconds| start + Duration::from_secs(seconds);
        let mut watch = HealthWatch::new(Duration::from_secs(20));
        assert!(!watch.healthy(time(30)));
        watch.report(time(40));
        assert!(!watch.healthy(time(60)));
        watch.report(time(60));
        watch.report(time(70));
        assert!(!watch.healthy(time(70)));
        watch.report(time(80));
        assert!(watch.healthy(time(80)));
        watch.reset();
        assert!(!watch.healthy(time(80)));
    }

    #[test]
    fn spends_the_restart_budget_with_growing_delays() {
        let mut budget = RestartBudget::new(3, Duration::from_secs(60));
        let start = Instant::now();
        let delays: Vec<_> = (0..4).map(|_| budget.next_delay(start)).collect();
        assert_eq!(delays[0], Some(Duration::from_millis(500)));
        assert_eq!(delays[2], Some(Duration::from_secs(2)));
        assert_eq!(delays[3], None);
        assert!(budget.next_delay(start + Duration::from_secs(61)).is_some());
    }

    #[test]
    fn keeps_the_logs_of_recent_boots() {
        let root = std::env::temp_dir().join(format!("openshell-session-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dirs: Vec<_> = (0..4).map(|_| boot_log_dir(&root, 2).unwrap()).collect();
        assert!(dirs[3].ends_with("0004"));
        let mut kept: Vec<_> = fs::read_dir(&root).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        kept.sort();
        assert_eq!(kept, ["0003", "0004"]);
        fs::remove_dir_all(&root).unwrap();
    }
}
