use std::env;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, BufReader};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use openshell_shell::{read_hostd_readiness, write_cef_startup};

const USAGE: &str = "Usage: openshell-shell <desktop-bundle> <cef-host> <state-dir> [node]";
const SIGINT: i32 = 2;
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;
const POLL_INTERVAL: Duration = Duration::from_millis(100);
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
        if matches!(self.0.try_wait(), Ok(Some(_))) {
            return;
        }
        let pid = self.0.id() as i32;
        // Both children lead process groups created by session_command.
        unsafe { kill(-pid, SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(4);
        while Instant::now() < deadline {
            if matches!(self.0.try_wait(), Ok(Some(_))) {
                return;
            }
            thread::sleep(POLL_INTERVAL);
        }
        unsafe { kill(-pid, SIGKILL) };
        let _ = self.0.wait();
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
    for name in ["PATH", "LANG", "LC_ALL", "DISPLAY", "XAUTHORITY"] {
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
    let runtime = create_directory(&state.join("runtime"), true)?;
    let profile_path = profile.to_str().filter(|path| !path.contains(['\r', '\n']))
        .ok_or("CEF profile path must be UTF-8 without line breaks")?;

    let mut daemon = SessionProcess(session_command(&node)
        .arg(&hostd)
        .args(["--host", "127.0.0.1", "--port", &port.to_string(), "--offline"])
        .arg("--ui").arg(&ui)
        .arg("--home").arg(&home)
        .arg("--data-dir").arg(&data)
        .env("HOME", &home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?);
    let Some(url) = hostd_url(&mut daemon.0, port)? else { return Ok(()) };

    let mut browser = session_command(cef.as_os_str());
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
    let pipe = browser.0.stdin.take().ok_or("CEF startup pipe is unavailable")?;
    write_cef_startup(pipe, &url, profile_path)?;

    while !STOP_REQUESTED.load(Ordering::Relaxed) {
        if let Some(status) = browser.0.try_wait()? {
            return if status.success() { Ok(()) } else { Err(format!("CEF host exited with {status}").into()) };
        }
        if let Some(status) = daemon.0.try_wait()? {
            return Err(format!("hostd exited with {status}").into());
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
