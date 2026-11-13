use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use openshell_shell::{read_hostd_readiness, write_cef_bootstrap, write_cef_startup};
use openshell_shell::session::{DESKTOP_HEALTH_ENV, DESKTOP_HEALTH_RESET};

const USAGE: &str = "Usage: openshell-shell <desktop-bundle> <cef-host> <state-dir> [node]";
const SIGINT: i32 = 2;
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// hostd may restart this many times within RESTART_WINDOW before the session fails.
const MAX_RESTARTS: usize = 5;
const RESTART_WINDOW: Duration = Duration::from_secs(60);
const ESRCH: i32 = 3;
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn flock(fd: i32, operation: i32) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
    fn geteuid() -> u32;
    fn signal(number: i32, handler: extern "C" fn(i32)) -> usize;
}

extern "C" fn request_stop(_: i32) {
    STOP_REQUESTED.store(true, Ordering::Relaxed);
}

struct SessionProcess(Child);

impl Drop for SessionProcess {
    fn drop(&mut self) {
        // Both children lead process groups created by session_command. The
        // group can outlive its leader, for example with hostd's terminals.
        let group = -(self.0.id() as i32);
        let leader_exited = matches!(self.0.try_wait(), Ok(Some(_)));
        if unsafe { kill(group, SIGTERM) } != 0 && leader_exited {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            let _ = self.0.try_wait();
            if unsafe { kill(group, 0) } != 0 && io::Error::last_os_error().raw_os_error() == Some(ESRCH) {
                return;
            }
            thread::sleep(POLL_INTERVAL);
        }
        unsafe { kill(group, SIGKILL) };
        let _ = self.0.wait();
    }
}

/// Restarts within RESTART_WINDOW, with an exponential delay between them.
struct RestartBudget(Vec<Instant>);

impl RestartBudget {
    fn next_delay(&mut self) -> Option<Duration> {
        let now = Instant::now();
        self.0.retain(|time| now.duration_since(*time) < RESTART_WINDOW);
        if self.0.len() >= MAX_RESTARTS {
            return None;
        }
        self.0.push(now);
        Some(Duration::from_millis(250 << (self.0.len() - 1)))
    }
}

fn create_directory(path: &Path, private: bool) -> io::Result<PathBuf> {
    DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    let directory = fs::symlink_metadata(path)?;
    let forbidden_mode = if private { 0o077 } else { 0o022 };
    if !directory.is_dir() || directory.uid() != unsafe { geteuid() } || directory.mode() & forbidden_mode != 0 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, format!("Invalid session directory ownership or permissions: {}", path.display())));
    }
    fs::canonicalize(path)
}

fn require_file(path: &Path, label: &str) -> io::Result<()> {
    if !path.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("Missing {label}: {}", path.display())));
    }
    Ok(())
}

fn hostd_url(child: &mut Child, port: u16) -> Result<Option<String>, Box<dyn Error>> {
    let stdout = child.stdout.take().ok_or("hostd stdout is unavailable")?;
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let _ = sender.send(read_hostd_readiness(&mut reader, port));
        let _ = io::copy(&mut reader, &mut io::sink());
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if STOP_REQUESTED.load(Ordering::Relaxed) {
            return Ok(None);
        }
        match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(result) => return Ok(Some(result?)),
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => continue,
            Err(_) => return Err("hostd did not emit a readiness message".into()),
        }
    }
}

fn session_command(executable: &OsStr) -> Command {
    let mut command = Command::new(executable);
    command.env_clear().process_group(0);
    // A system without /etc/fonts, such as NixOS, names its font configuration,
    // and XDG_DATA_DIRS says where the installed applications' entries are.
    for name in ["PATH", "LANG", "LC_ALL", "DISPLAY", "XAUTHORITY", "FONTCONFIG_FILE", "XDG_DATA_DIRS"] {
        if let Some(value) = env::var_os(name) {
            command.env(name, value);
        }
    }
    command
}

fn run(args: &[OsString]) -> Result<(), Box<dyn Error>> {
    if !(3..=4).contains(&args.len()) {
        return Err(USAGE.into());
    }
    if unsafe { geteuid() } == 0 || env::var_os("DISPLAY").is_none() {
        return Err("Use an unprivileged user in an X11 session".into());
    }
    for number in [SIGINT, SIGTERM] {
        if unsafe { signal(number, request_stop) } == usize::MAX {
            return Err(io::Error::last_os_error().into());
        }
    }

    let bundle = fs::canonicalize(&args[0])?;
    let cef = fs::canonicalize(&args[1])?;
    let hostd = bundle.join("hostd/openshell-hostd.mjs");
    let ui = bundle.join("ui");
    require_file(&cef, "CEF host")?;
    require_file(&hostd, "hostd")?;
    require_file(&ui.join("index.html"), "desktop UI")?;
    let bundled_node = bundle.join("node/bin/node");
    let node = args.get(3).cloned().unwrap_or_else(|| {
        if bundled_node.is_file() { bundled_node.into_os_string() } else { "node".into() }
    });

    let port = env::var("OPENSHELL_NATIVE_PORT").unwrap_or_else(|_| "7788".into()).parse::<u16>()?;
    if port == 0 {
        return Err("A persistent desktop profile requires a nonzero hostd port".into());
    }
    let state = create_directory(Path::new(&args[2]), false)?;
    let lock = OpenOptions::new().create(true).truncate(false).write(true).open(state.join("session.lock"))?;
    if unsafe { flock(lock.as_raw_fd(), 2 | 4) } != 0 {
        return Err("The Native session state is already in use or cannot be locked".into());
    }
    let home = create_directory(&state.join("home"), true)?;
    let data = create_directory(&state.join("hostd"), true)?;
    let profile = create_directory(&state.join("cef"), true)?;
    let runtime = env::var_os("OPENSHELL_NATIVE_RUNTIME_DIR").map(PathBuf::from)
        .unwrap_or_else(|| state.join("runtime"));
    let runtime = create_directory(&runtime, true)?;
    let profile_path = profile.to_str().filter(|path| !path.contains(['\r', '\n']))
        .ok_or("CEF profile path must be UTF-8 without line breaks")?;

    // hostd's errors, kept per session. Its stdout carries the bootstrap
    // secret and is never logged.
    let log = OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(state.join("hostd.log"))?;
    let start_hostd = || -> Result<(SessionProcess, Option<String>), Box<dyn Error>> {
        let mut daemon = SessionProcess(session_command(&node)
            .arg(&hostd)
            .args(["--host", "127.0.0.1", "--port", &port.to_string(), "--offline"])
            .arg("--ui").arg(&ui)
            .arg("--home").arg(&home)
            .arg("--data-dir").arg(&data)
            .env("HOME", &home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(log.try_clone()?)
            .spawn()?);
        let url = hostd_url(&mut daemon.0, port)?;
        Ok((daemon, url))
    };
    let (mut daemon, Some(url)) = start_hostd()? else { return Ok(()) };

    let mut browser = session_command(cef.as_os_str());
    if env::var(DESKTOP_HEALTH_ENV).as_deref() == Ok("1") {
        browser.env(DESKTOP_HEALTH_ENV, "1");
    }
    if env::var_os("XAUTHORITY").is_none() {
        if let Some(path) = env::var_os("HOME").map(|home| PathBuf::from(home).join(".Xauthority")) {
            if path.is_file() { browser.env("XAUTHORITY", path); }
        }
    }
    let mut browser = SessionProcess(browser
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", &runtime)
        .stdin(Stdio::piped())
        .spawn()?);
    // The pipe stays open for the session: the host ends the session when it closes.
    let mut pipe = browser.0.stdin.take().ok_or("CEF startup pipe is unavailable")?;
    write_cef_startup(&mut pipe, &url, profile_path)?;

    let mut budget = RestartBudget(Vec::new());
    while !STOP_REQUESTED.load(Ordering::Relaxed) {
        if let Some(status) = browser.0.try_wait()? {
            return if status.success() { Ok(()) } else { Err(format!("CEF host exited with {status}").into()) };
        }
        if let Some(status) = daemon.0.try_wait()? {
            if env::var(DESKTOP_HEALTH_ENV).as_deref() == Ok("1") {
                let mut stdout = io::stdout().lock();
                writeln!(stdout, "{DESKTOP_HEALTH_RESET}")?;
                stdout.flush()?;
            }
            let Some(delay) = budget.next_delay() else {
                return Err(format!("hostd exited with {status} too often").into());
            };
            eprintln!("openshell-shell: hostd exited with {status}; restarting");
            drop(daemon);
            thread::sleep(delay);
            let (restarted, url) = start_hostd()?;
            daemon = restarted;
            let Some(url) = url else { break };
            write_cef_bootstrap(&mut pipe, &url)?;
        }
        thread::sleep(POLL_INTERVAL);
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() == 1 && (args[0] == "--help" || args[0] == "-h") {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("openshell-shell: {error}");
            ExitCode::FAILURE
        }
    }
}
