//! The session supervisor of OpenShell Native, started by init as root.
//!
//! It starts the X server, the window manager and the shell launcher as the
//! session user, restarts what fails within a budget, reaps the processes that
//! the session leaves behind and shuts everything down in order. Each boot
//! keeps its logs in its own directory.

use std::env;
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use openshell_shell::session::{boot_log_dir, CommandLine, HealthWatch, RestartBudget, SessionConfig};

const USAGE: &str = "Usage: openshell-session <config>";
const SIGINT: i32 = 2;
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;
const WNOHANG: i32 = 1;
const PR_SET_CHILD_SUBREAPER: i32 = 36;
const POLL: Duration = Duration::from_millis(100);
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const KEPT_BOOTS: usize = 5;
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
    fn geteuid() -> u32;
    fn chown(path: *const std::ffi::c_char, uid: u32, gid: u32) -> i32;
    fn signal(number: i32, handler: extern "C" fn(i32)) -> usize;
    fn capset(header: *mut CapHeader, data: *const CapData) -> i32;
}

#[repr(C)]
struct CapHeader {
    version: u32,
    pid: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CapData {
    effective: u32,
    permitted: u32,
    inheritable: u32,
}

const CAPABILITY_VERSION_3: u32 = 0x2008_0522;
const CAP_FOWNER: u32 = 3;
const CAP_KILL: u32 = 5;
const CAP_SETGID: u32 = 6;
const CAP_SETUID: u32 = 7;
/// What the supervisor keeps once the session's directories exist: starting
/// the session as its user, signalling its processes and removing the stale
/// socket of its display, which belongs to the user.
pub const KEPT_CAPABILITIES: u32 = 1 << CAP_FOWNER | 1 << CAP_KILL | 1 << CAP_SETGID | 1 << CAP_SETUID;

fn drop_capabilities() -> io::Result<()> {
    let mut header = CapHeader { version: CAPABILITY_VERSION_3, pid: 0 };
    let kept = CapData { effective: KEPT_CAPABILITIES, permitted: KEPT_CAPABILITIES, inheritable: 0 };
    let none = CapData { effective: 0, permitted: 0, inheritable: 0 };
    if unsafe { capset(&mut header, [kept, none].as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

extern "C" fn request_stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}

/// A line of the supervisor's own log, with the time since the epoch.
struct Journal(File);

impl Journal {
    fn note(&mut self, message: &str) {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        let _ = writeln!(self.0, "{}.{:03} {message}", now.as_secs(), now.subsec_millis());
        eprintln!("openshell-session: {message}");
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    XServer,
    WindowManager,
    Shell,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::XServer => "x",
            Role::WindowManager => "wm",
            Role::Shell => "shell",
        }
    }
}

/// A process of the session, which leads its own process group.
struct Member {
    role: Role,
    pid: i32,
    exited: Option<i32>,
}

struct Supervisor {
    config: SessionConfig,
    logs: std::path::PathBuf,
    journal: Journal,
    members: Vec<Member>,
}

fn describe(status: i32) -> String {
    if status & 0x7f == 0 {
        format!("status {}", (status >> 8) & 0xff)
    } else {
        format!("signal {}", status & 0x7f)
    }
}

impl Supervisor {
    fn spawn(&mut self, role: Role, command_line: &CommandLine) -> io::Result<()> {
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o640)
            .open(self.logs.join(format!("{}.log", role.name())))?;
        let mut command = Command::new(&command_line[0]);
        command
            .args(&command_line[1..])
            .env_clear()
            .env("HOME", &self.config.home)
            .env("DISPLAY", &self.config.display)
            .env("LANG", "C.UTF-8")
            .envs(self.config.env.iter().map(|(name, value)| (name, value)))
            .current_dir(&self.config.home)
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0);
        // std drops the supplementary groups when it changes the user as root.
        command.uid(self.config.uid).gid(self.config.gid);
        let child = command.spawn()?;
        let pid = child.id() as i32;
        // The supervisor reaps every process with waitpid, the session's
        // orphans included.
        drop(child);
        self.journal.note(&format!("started {} (pid {pid})", role.name()));
        self.members.retain(|member| member.role != role);
        self.members.push(Member { role, pid, exited: None });
        Ok(())
    }

    /// Reaps every exited child. The session's members get their status;
    /// orphans reparented to the supervisor are only noted.
    fn reap(&mut self) {
        loop {
            let mut status = 0;
            let pid = unsafe { waitpid(-1, &mut status, WNOHANG) };
            if pid <= 0 {
                return;
            }
            if let Some(member) = self.members.iter_mut().find(|member| member.pid == pid) {
                member.exited = Some(status);
                let role = member.role.name();
                self.journal.note(&format!("{role} (pid {pid}) exited with {}", describe(status)));
            } else {
                self.journal.note(&format!("reaped orphan pid {pid} ({})", describe(status)));
            }
        }
    }

    fn exited(&self, role: Role) -> Option<i32> {
        self.members.iter().find(|member| member.role == role).and_then(|member| member.exited)
    }

    /// Stops a member and the rest of its process group, then reaps it.
    fn stop(&mut self, role: Role) {
        let Some(pid) = self.members.iter().find(|member| member.role == role).map(|member| member.pid) else {
            return;
        };
        unsafe { kill(-pid, SIGTERM) };
        let deadline = Instant::now() + STOP_TIMEOUT;
        while Instant::now() < deadline && unsafe { kill(-pid, 0) } == 0 {
            self.reap();
            thread::sleep(POLL);
        }
        if unsafe { kill(-pid, 0) } == 0 {
            self.journal.note(&format!("{} did not stop; killing its group", role.name()));
            unsafe { kill(-pid, SIGKILL) };
            thread::sleep(POLL);
        }
        self.reap();
        self.members.retain(|member| member.role != role);
    }

    fn stop_all(&mut self) {
        for role in [Role::Shell, Role::WindowManager, Role::XServer] {
            self.stop(role);
        }
    }

    fn wait_for_x(&mut self) -> Result<(), Box<dyn Error>> {
        let socket = self.config.x_socket();
        let deadline = Instant::now() + READY_TIMEOUT;
        while !socket.exists() {
            self.reap();
            if STOP.load(Ordering::Relaxed) {
                return Err("stopped".into());
            }
            if let Some(status) = self.exited(Role::XServer) {
                return Err(format!("the X server exited with {}", describe(status)).into());
            }
            if Instant::now() > deadline {
                return Err("the X server did not create its socket".into());
            }
            thread::sleep(POLL);
        }
        self.journal.note("x is ready");
        Ok(())
    }

    /// Starts X and the window manager, then the shell.
    fn start(&mut self) -> Result<(), Box<dyn Error>> {
        release_display(&self.config)?;
        let x_server = self.config.x_server.clone();
        self.spawn(Role::XServer, &x_server)?;
        self.wait_for_x()?;
        if let Some(window_manager) = self.config.window_manager.clone() {
            self.spawn(Role::WindowManager, &window_manager)?;
        }
        let shell = self.config.shell.clone();
        self.spawn(Role::Shell, &shell)?;
        Ok(())
    }

    /// The session is healthy: the release the computer booted is confirmed
    /// (the A/B transaction, distro/ab), once per boot. A failure is recorded;
    /// the release then stays on trial and the next boots fall back.
    fn confirm_boot(&mut self) {
        let Some(confirm) = self.config.confirm.clone() else { return };
        match Command::new(&confirm[0]).args(&confirm[1..]).stdin(Stdio::null()).status() {
            Ok(status) if status.success() => self.journal.note("the session is healthy; confirmed the boot"),
            Ok(status) => self.journal.note(&format!("confirming the boot failed ({status})")),
            Err(error) => self.journal.note(&format!("confirming the boot failed ({error})")),
        }
    }

    /// Runs the session until the shell ends it, a stop is requested or
    /// failures exceed the budget.
    fn run(&mut self) -> Result<(), Box<dyn Error>> {
        let mut session_budget = RestartBudget::new(3, Duration::from_secs(300));
        let mut shell_budget = RestartBudget::new(5, Duration::from_secs(60));
        let mut wm_budget = RestartBudget::new(5, Duration::from_secs(60));
        let mut health = HealthWatch::new(self.config.confirm_after);
        self.start()?;
        health.started(Instant::now());
        while !STOP.load(Ordering::Relaxed) {
            self.reap();
            if health.healthy(Instant::now()) {
                self.confirm_boot();
            }
            if let Some(status) = self.exited(Role::XServer) {
                // Without X, the whole session starts over.
                self.journal.note(&format!("x failed ({}); restarting the session", describe(status)));
                self.stop_all();
                let delay = session_budget.next_delay(Instant::now()).ok_or("the X server keeps failing")?;
                thread::sleep(delay);
                self.start()?;
                health.started(Instant::now());
                continue;
            }
            if let Some(status) = self.exited(Role::Shell) {
                if status == 0 {
                    self.journal.note("the shell ended the session");
                    return Ok(());
                }
                let delay = shell_budget.next_delay(Instant::now()).ok_or("the shell keeps failing")?;
                self.journal.note(&format!("restarting the shell in {} ms", delay.as_millis()));
                self.stop(Role::Shell);
                thread::sleep(delay);
                let shell = self.config.shell.clone();
                self.spawn(Role::Shell, &shell)?;
                health.started(Instant::now());
            }
            if self.exited(Role::WindowManager).is_some() {
                if let Some(delay) = wm_budget.next_delay(Instant::now()) {
                    self.stop(Role::WindowManager);
                    thread::sleep(delay);
                    if let Some(window_manager) = self.config.window_manager.clone() {
                        self.spawn(Role::WindowManager, &window_manager)?;
                    }
                }
            }
            thread::sleep(POLL);
        }
        self.journal.note("stop requested");
        Ok(())
    }
}

/// Removes the socket and lock of a display whose X server is gone. A display
/// in use by a running server is an error.
fn release_display(config: &SessionConfig) -> Result<(), Box<dyn Error>> {
    let number = &config.display[1..];
    let lock = format!("/tmp/.X{number}-lock");
    if let Ok(text) = fs::read_to_string(&lock) {
        if let Ok(pid) = text.trim().parse::<i32>() {
            if pid > 0 && unsafe { kill(pid, 0) } == 0 {
                return Err(format!("display {} is in use by pid {pid}", config.display).into());
            }
        }
        fs::remove_file(&lock)?;
    }
    match fs::remove_file(config.x_socket()) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

fn prepare_home(config: &SessionConfig) -> Result<(), Box<dyn Error>> {
    // The X servers of every user put their sockets here.
    fs::DirBuilder::new().recursive(true).create("/tmp/.X11-unix")?;
    fs::set_permissions("/tmp/.X11-unix", std::os::unix::fs::PermissionsExt::from_mode(0o1777))?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&config.home)?;
    let path = std::ffi::CString::new(config.home.as_os_str().as_encoded_bytes())?;
    if unsafe { chown(path.as_ptr(), config.uid, config.gid) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() != 1 || args[0] == "--help" {
        eprintln!("{USAGE}");
        return if args.len() == 1 { ExitCode::SUCCESS } else { ExitCode::from(2) };
    }
    let result = (|| -> Result<(), Box<dyn Error>> {
        if unsafe { geteuid() } != 0 {
            return Err("The supervisor runs as root and starts the session as its user".into());
        }
        let config = SessionConfig::parse(&fs::read_to_string(&args[0])?)?;
        for number in [SIGINT, SIGTERM] {
            if unsafe { signal(number, request_stop) } == usize::MAX {
                return Err(io::Error::last_os_error().into());
            }
        }
        // Processes that the session leaves behind are reparented here and reaped.
        if unsafe { prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } != 0 {
            return Err(io::Error::last_os_error().into());
        }
        prepare_home(&config)?;
        let logs = boot_log_dir(Path::new(&config.log_dir), KEPT_BOOTS)?;
        let journal = Journal(OpenOptions::new().create(true).append(true).mode(0o640).open(logs.join("session.log"))?);
        let mut supervisor = Supervisor { config, logs, journal, members: Vec::new() };
        supervisor.journal.note(&format!("session logs in {}", supervisor.logs.display()));
        // Root keeps only what the supervision needs.
        drop_capabilities()?;
        supervisor.journal.note(&format!("kept capabilities {KEPT_CAPABILITIES:#x}"));
        let outcome = supervisor.run();
        if let Err(error) = &outcome {
            supervisor.journal.note(&format!("session failed: {error}"));
        }
        supervisor.stop_all();
        supervisor.journal.note("session stopped");
        outcome
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("openshell-session: {error}");
            ExitCode::FAILURE
        }
    }
}
